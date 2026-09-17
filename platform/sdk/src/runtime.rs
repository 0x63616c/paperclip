//! The frame loop every app runs, so no app has to write one.
//!
//! [`run`] does the handshake, reports readiness, pumps events, answers draw
//! requests, and saves on the way out. An app supplies an [`App`] and never
//! sees a message.
//!
//! ## Two sources, one queue
//!
//! Host messages arrive on a socket; worker threads finish whenever they
//! finish. Both go into one `mpsc` channel — a reader thread owns the socket
//! and forwards everything it decodes — so the loop is a blocking `recv` and
//! an app's state is only ever touched from one thread. No executor, no
//! polling, no locks in an app.
//!
//! ## What this loop is not
//!
//! It is not what holds the app to the contract. Everything here is inside the
//! app's own process and an app is free to link none of it. The checks below
//! exist so an *honest* app fails at the point of the mistake instead of being
//! killed later by the host: the host runs the real ones, over the socket,
//! against a process it assumes is hostile.

use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use paper_protocol::{
    Action, AppMessage, CURRENT, CodecError, Diagnostic, ExitReason, FrameDone, Hello, HostMessage,
    LifecycleEvent, MAX_SHARED_GRANTS, ProtocolVersion, Ready, Saved, codec,
};

use crate::app::{App, Context, Event};
use crate::storage::Storage;
use crate::surface::{Surface, SurfaceError, SurfaceProvider};
use crate::tasks::{Completer, Incoming};

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The host asked the app to exit and it saved first.
    Exited {
        /// Why it was asked.
        reason: ExitReason,
        /// Whether the save succeeded.
        saved: bool,
    },
    /// The host said goodbye, or closed the connection.
    HostClosed,
}

/// Why a session could not run.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RuntimeError {
    /// The first message was not a `Hello`.
    ///
    /// Nothing an app can recover from: without a `Hello` it does not know its
    /// own id, its viewport or where its storage is.
    #[error("the host did not open the session with `hello`")]
    NoHello,

    /// The host speaks a protocol this binary was not built against.
    ///
    /// Refused explicitly rather than negotiated down. An app built against
    /// `1.3` on a `1.1` host would be reading for messages that do not exist.
    #[error("host speaks protocol {host}, which this app ({app}) cannot run against")]
    UnsupportedProtocol {
        /// What the host offered.
        host: ProtocolVersion,
        /// What this binary speaks.
        app: ProtocolVersion,
    },

    /// The host handed over more shared directories than the contract allows.
    ///
    /// The host's bug, not the app's, and refused rather than truncated: an
    /// app silently missing a share the user granted is a worse outcome than
    /// one that does not start.
    #[error("host granted {granted} shared directories, over the limit of {max}")]
    TooManyGrants {
        /// How many arrived.
        granted: usize,
        /// The limit.
        max: usize,
    },

    /// The drawing surface could not be bound.
    #[error("the drawing surface could not be opened")]
    Surface(#[from] SurfaceError),

    /// The connection failed.
    #[error("the connection to the host failed")]
    Transport(#[from] CodecError),
}

/// Runs an app to completion.
///
/// `reader` and `writer` are the two halves of the launch connection the
/// supervisor handed this process; `surfaces` binds the drawing surface the
/// host described.
pub fn run<A, R, W, P>(
    mut app: A,
    mut reader: R,
    mut writer: W,
    mut surfaces: P,
) -> Result<Outcome, RuntimeError>
where
    A: App,
    R: Read + Send + 'static,
    W: Write,
    P: SurfaceProvider,
{
    let hello = match codec::read_message::<_, HostMessage>(&mut reader)? {
        HostMessage::Hello(hello) => hello,
        _ => return Err(RuntimeError::NoHello),
    };
    if !hello.protocol.can_run(CURRENT) {
        return Err(RuntimeError::UnsupportedProtocol {
            host: hello.protocol,
            app: CURRENT,
        });
    }
    if hello.paths.shared.len() > MAX_SHARED_GRANTS {
        return Err(RuntimeError::TooManyGrants {
            granted: hello.paths.shared.len(),
            max: MAX_SHARED_GRANTS,
        });
    }

    let mut surface = surfaces.open(&hello.surface)?;
    let storage = Storage::new(hello.paths.clone(), &hello.capabilities);

    let (sender, receiver) = channel::<Incoming<A::Completion>>();
    let completer = Completer::new(sender.clone());
    thread::spawn(move || pump(&mut reader, &sender));

    // Readiness is the app's promise that its surface is mapped. Sending it
    // before `surfaces.open` succeeded would be a lie the host acts on by
    // asking for a frame.
    codec::write_message(&mut writer, &AppMessage::Ready(Ready { protocol: CURRENT }))?;

    Loop {
        app: &mut app,
        hello: &hello,
        storage,
        completer,
        surface: &mut surface,
        writer: &mut writer,
        receiver,
    }
    .drive()
}

/// Everything the loop needs, so `drive` reads as the state machine it is.
struct Loop<'a, A: App, S: Surface, W: Write> {
    app: &'a mut A,
    hello: &'a Hello,
    storage: Storage,
    completer: Completer<A::Completion>,
    surface: &'a mut S,
    writer: &'a mut W,
    receiver: Receiver<Incoming<A::Completion>>,
}

impl<A: App, S: Surface, W: Write> Loop<'_, A, S, W> {
    fn drive(mut self) -> Result<Outcome, RuntimeError> {
        while let Ok(incoming) = self.receiver.recv() {
            let event = match incoming {
                Incoming::Completed(completion) => Event::Completed(completion),
                Incoming::Host(Err(CodecError::Closed)) => return Ok(Outcome::HostClosed),
                Incoming::Host(Err(error)) => return Err(error.into()),
                Incoming::Host(Ok(HostMessage::Goodbye)) => return Ok(Outcome::HostClosed),
                Incoming::Host(Ok(HostMessage::Hello(_))) => {
                    // A second `Hello` is the host misbehaving, not the app.
                    // Ignoring it keeps the app's world the one it was given.
                    continue;
                }
                Incoming::Host(Ok(HostMessage::Draw(request))) => {
                    self.draw(request.frame)?;
                    continue;
                }
                Incoming::Host(Ok(HostMessage::Pointer(pointer))) => Event::Pointer(pointer),
                Incoming::Host(Ok(HostMessage::Lifecycle(LifecycleEvent::Suspended))) => {
                    Event::Suspended
                }
                Incoming::Host(Ok(HostMessage::Lifecycle(LifecycleEvent::Resumed))) => {
                    Event::Resumed
                }
                Incoming::Host(Ok(HostMessage::Lifecycle(
                    event @ LifecycleEvent::PrepareToExit { reason, .. },
                ))) => {
                    let deadline = event.deadline().unwrap_or_default();
                    return self.exit(reason, deadline);
                }
                // A message from a newer minor version. Ignored on purpose:
                // that is what "additive" means, and refusing to run would
                // make every minor bump a breaking one.
                Incoming::Host(Ok(_)) => continue,
            };

            let (action, outbox) = self.deliver(&event);
            self.flush(outbox)?;
            if let Some(request) = action.request() {
                codec::write_message(self.writer, &AppMessage::Request(request))?;
            }
        }
        Ok(Outcome::HostClosed)
    }

    /// Runs one callback and returns what it decided, plus anything it logged.
    fn deliver(&mut self, event: &Event<A::Completion>) -> (Action, Vec<Diagnostic>) {
        let mut outbox = Vec::new();
        let action = {
            let mut context =
                Self::context(self.hello, &self.storage, &self.completer, &mut outbox);
            self.app.event(event, &mut context)
        };
        (action, outbox)
    }

    fn draw(&mut self, frame: paper_protocol::FrameId) -> Result<(), RuntimeError> {
        let mut outbox = Vec::new();
        {
            let mut context =
                Self::context(self.hello, &self.storage, &self.completer, &mut outbox);
            self.app.draw(self.surface.canvas(), &mut context);
        }
        let damage = self.app.damage();
        self.surface.publish(&damage)?;
        self.flush(outbox)?;
        codec::write_message(self.writer, &AppMessage::Frame(FrameDone { frame, damage }))?;
        Ok(())
    }

    /// Delivers the exit event, saves, reports, and stops.
    fn exit(
        &mut self,
        reason: ExitReason,
        deadline: std::time::Duration,
    ) -> Result<Outcome, RuntimeError> {
        let mut outbox = Vec::new();
        let saved = {
            let mut context =
                Self::context(self.hello, &self.storage, &self.completer, &mut outbox);
            // Whatever the event handler returns is ignored: the app is
            // leaving, and a `Redraw` requested on the way out would be a
            // frame nobody sees.
            let _ = self
                .app
                .event(&Event::PrepareToExit { reason, deadline }, &mut context);
            match self.app.save(&mut context) {
                Ok(()) => true,
                Err(error) => {
                    context.error(format!("save failed: {error}"));
                    false
                }
            }
        };
        self.flush(outbox)?;
        codec::write_message(self.writer, &AppMessage::Saved(Saved { reason, ok: saved }))?;
        Ok(Outcome::Exited { reason, saved })
    }

    fn flush(&mut self, outbox: Vec<Diagnostic>) -> Result<(), RuntimeError> {
        for diagnostic in outbox {
            codec::write_message(self.writer, &AppMessage::Diagnostic(diagnostic))?;
        }
        Ok(())
    }

    fn context<'c>(
        hello: &'c Hello,
        storage: &'c Storage,
        completer: &'c Completer<A::Completion>,
        outbox: &'c mut Vec<Diagnostic>,
    ) -> Context<'c, A::Completion> {
        Context::new(
            hello.session,
            &hello.app,
            &hello.version,
            hello.launch,
            hello.surface.extent,
            &hello.capabilities,
            storage,
            completer,
            outbox,
        )
    }
}

/// Reads the connection until it ends, forwarding everything onto the queue.
fn pump<C: Send + 'static, R: Read>(reader: &mut R, sender: &std::sync::mpsc::Sender<Incoming<C>>) {
    loop {
        let message = codec::read_message::<_, HostMessage>(reader);
        let fatal = message.is_err();
        // Either the loop has gone, or there will not be another message.
        if sender.send(Incoming::Host(message)).is_err() || fatal {
            return;
        }
    }
}
