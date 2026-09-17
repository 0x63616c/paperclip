//! Getting work off the UI loop, and getting the answer back onto it.
//!
//! The rule this module exists to make keepable: **nothing long-running
//! happens on the event loop, and nothing at all happens in `draw`.** An app
//! that downloads a package inside `draw` is an app whose screen is frozen for
//! the length of the download, on a device where a frozen screen looks
//! identical to a crashed one.
//!
//! So: an app hands the work to a thread, and the thread hands back a value.
//! That value arrives as [`Event::Completed`](crate::Event::Completed) on the
//! event loop, in the same queue as input and lifecycle events, so the app's
//! state is only ever touched from one thread and there is no lock anywhere in
//! an app.
//!
//! The completion type is the app's own — [`App::Completion`](crate::App::Completion)
//! — because the platform has no idea what "finished" means for a chess clock
//! or a package download, and a `Vec<u8>` that every app parses back into its
//! own type would be a worse version of the same thing.
//!
//! This is the whole mechanism. There is no executor, no task registry and no
//! cancellation: the App Store's downloads need a thread and a channel, and
//! anything more would be a runtime nobody asked for.

use std::sync::mpsc::{SendError, Sender};

use paper_protocol::{CodecError, HostMessage};

/// Something that arrived at the event loop, from either source.
///
/// Internal to the runtime: [`Completer`] is the only public thing that can
/// put one on the queue, and it only ever puts a completion there.
// The host variant is much larger than a typical completion, which clippy
// flags. Boxing it would trade a 224-byte move for a heap allocation on every
// input event, and input is the highest-rate thing on this channel — the moves
// are cheaper. The queue carries the larger variant either way.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum Incoming<C> {
    /// A message from the host, or the reason there will not be another.
    Host(Result<HostMessage, CodecError>),
    /// A worker thread finished.
    Completed(C),
}

/// The event loop hung up before the work finished.
///
/// Not an error worth handling in most apps: it means the app is exiting, and
/// the work being thrown away is the correct outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the app's event loop has stopped")]
pub struct Disconnected;

/// A handle a worker thread uses to report back.
///
/// Cloneable and `Send`, so one app can have as many workers as it has work.
#[derive(Debug)]
pub struct Completer<C> {
    sender: Sender<Incoming<C>>,
}

// Derived `Clone` would demand `C: Clone`, which is wrong: the channel is
// cloneable whatever travels through it.
impl<C> Clone for Completer<C> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl<C: Send + 'static> Completer<C> {
    /// Wraps the event loop's sender.
    pub(crate) fn new(sender: Sender<Incoming<C>>) -> Self {
        Self { sender }
    }

    /// Delivers a completion to the event loop.
    pub fn complete(&self, completion: C) -> Result<(), Disconnected> {
        self.sender
            .send(Incoming::Completed(completion))
            .map_err(|SendError(_)| Disconnected)
    }

    /// Runs `work` on a new thread and delivers whatever it returns.
    ///
    /// The thread is detached. An app that needs to know whether its work is
    /// still running tracks that in its own state — the platform deliberately
    /// does not, because a task registry is a lifecycle of its own and this
    /// mechanism exists to avoid having one.
    ///
    /// A panic in `work` is a panic in the app's own process, and the
    /// supervisor treats the app as having crashed. It does not take the event
    /// loop's thread with it, so the app gets to keep drawing while the host
    /// decides what to do — which is the point of it being a thread at all.
    pub fn spawn<F>(&self, work: F)
    where
        F: FnOnce() -> C + Send + 'static,
    {
        let completer = self.clone();
        std::thread::spawn(move || {
            let completion = work();
            // The app is gone; dropping the answer is the right outcome.
            let _ = completer.complete(completion);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{Completer, Incoming};
    use std::sync::mpsc::channel;

    #[test]
    fn a_worker_thread_reports_back_onto_the_event_queue() {
        let (sender, receiver) = channel();
        let completer = Completer::new(sender);
        completer.spawn(|| 42_u32);

        match receiver.recv().expect("a completion arrives") {
            Incoming::Completed(value) => assert_eq!(value, 42),
            other => panic!("expected a completion, got {other:?}"),
        }
    }

    #[test]
    fn several_workers_share_one_completer() {
        let (sender, receiver) = channel();
        let completer = Completer::new(sender);
        for index in 0..4_u32 {
            let completer = completer.clone();
            completer.spawn(move || index);
        }
        let mut seen: Vec<u32> = (0..4)
            .map(|_| match receiver.recv().unwrap() {
                Incoming::Completed(value) => value,
                other => panic!("expected a completion, got {other:?}"),
            })
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2, 3]);
    }

    /// Work that finishes after the app has gone is dropped, not a panic: the
    /// last thing a shutting-down app needs is a worker taking it down.
    #[test]
    fn a_completion_with_nobody_listening_is_not_a_failure() {
        let (sender, receiver) = channel::<Incoming<u32>>();
        let completer = Completer::new(sender);
        drop(receiver);
        assert!(completer.complete(1).is_err());
    }
}
