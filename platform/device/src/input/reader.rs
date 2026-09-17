//! Turns an already-open evdev node into a stream of [`PointerEvent`]s.
//!
//! Split the same way the rest of `input` is: [`absorb`] is pure — bytes in,
//! [`RawEvent`]s and a carried remainder out — and is tested here directly.
//! [`drive`] adds the read loop around it, generic over [`Read`] rather than
//! tied to a file, so it runs against a pipe in a test; only [`open_node`],
//! which turns a `/dev/input/eventN` path into that `Read`, is Linux-only.
//!
//! Nothing here decides *which* node is the pen or the touchscreen — that is
//! [`nodes::resolve`](super::nodes::resolve) — or what the raw reports mean —
//! that is [`TouchDecoder`](super::TouchDecoder) and
//! [`PenDecoder`](super::PenDecoder). This module only gets the bytes from
//! the kernel to them.

use std::io::Read;
use std::sync::mpsc::Sender;

use paper_sdk::PointerEvent;

use super::evdev::{EVENT_BYTES, RawEvent};

/// Decodes as many whole events as `carry` plus `chunk` contain, and leaves
/// the trailing partial event — if any — in `carry` for the next call.
///
/// A `read()` on a character device has no reason to land on an event
/// boundary: the kernel hands back whatever was queued, and the last few
/// bytes of a read can be the first half of an event the next read
/// completes. Carrying that remainder is the one piece of state a reader
/// needs across calls.
pub fn absorb(carry: &mut Vec<u8>, chunk: &[u8]) -> Vec<RawEvent> {
    carry.extend_from_slice(chunk);
    let (events, leftover) = RawEvent::decode_all(carry);
    let start = carry.len() - leftover;
    carry.drain(..start);
    events
}

/// Reads `node` to exhaustion, decoding every event through `feed` and
/// sending whatever [`PointerEvent`]s that produces down `sink`.
///
/// Returns when the node closes, a read fails, or `sink`'s receiver is gone —
/// the last of those is the ordinary way this ends: the session that owns the
/// receiver dropped it because the interactive loop is exiting, and a reader
/// thread with nowhere left to send has nothing left to do.
pub fn drive(
    mut node: impl Read,
    mut feed: impl FnMut(RawEvent) -> Vec<PointerEvent>,
    sink: &Sender<PointerEvent>,
) {
    let mut carry = Vec::new();
    let mut buf = [0u8; EVENT_BYTES * 64];
    loop {
        let read = match node.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        for event in absorb(&mut carry, &buf[..read]) {
            for pointer in feed(event) {
                if sink.send(pointer).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::open_node;

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::File;
    use std::path::Path;

    use crate::error::DeviceError;

    /// Opens an evdev node for blocking reads.
    ///
    /// Read-only, and deliberately not `O_NONBLOCK`: [`super::drive`] is
    /// meant to run on its own thread and block in `read()` between reports,
    /// unlike [`super::nodes::linux::open_node`](crate::input::nodes) which
    /// only ever issues ioctls and wants to fail fast instead of wait.
    pub fn open_node(path: &Path) -> Result<File, DeviceError> {
        File::open(path).map_err(|source| DeviceError::io("open", path, source))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::{absorb, drive};
    use crate::input::evdev::RawEvent;

    fn event_bytes(kind: u16, code: u16, value: i32) -> [u8; 24] {
        let mut bytes = [0u8; 24];
        bytes[16..18].copy_from_slice(&kind.to_le_bytes());
        bytes[18..20].copy_from_slice(&code.to_le_bytes());
        bytes[20..24].copy_from_slice(&value.to_le_bytes());
        bytes
    }

    #[test]
    fn a_read_split_mid_event_is_carried_to_the_next_call() {
        let event = event_bytes(3, 0x35, 42);
        let mut carry = Vec::new();
        assert!(absorb(&mut carry, &event[..10]).is_empty());
        assert_eq!(carry.len(), 10);

        let events = absorb(&mut carry, &event[10..]);
        assert_eq!(
            events,
            vec![RawEvent {
                kind: 3,
                code: 0x35,
                value: 42
            }]
        );
        assert!(carry.is_empty());
    }

    #[test]
    fn two_whole_events_in_one_read_both_decode_and_nothing_is_carried() {
        let mut bytes = event_bytes(1, 0x14a, 1).to_vec();
        bytes.extend_from_slice(&event_bytes(0, 0, 0));
        let mut carry = Vec::new();

        let events = absorb(&mut carry, &bytes);

        assert_eq!(
            events,
            vec![
                RawEvent {
                    kind: 1,
                    code: 0x14a,
                    value: 1
                },
                RawEvent {
                    kind: 0,
                    code: 0,
                    value: 0
                }
            ]
        );
        assert!(carry.is_empty());
    }

    #[test]
    fn drive_stops_when_the_node_closes() {
        let events = event_bytes(0, 0, 0);
        let (sink, _receiver) = mpsc::channel();
        // An empty slice `Read`s as EOF, same as a closed pipe.
        drive(events.as_slice(), |_| Vec::new(), &sink);
        // Reaching here at all is the assertion: a real device node would
        // block forever on an empty read, but `&[u8]` reports EOF instead, so
        // `drive` returning proves it is `read()`-driven rather than looping
        // on a fixed iteration count.
    }

    #[test]
    fn drive_stops_once_nobody_is_receiving() {
        let event = event_bytes(3, 0x35, 1);
        let mut long = Vec::new();
        for _ in 0..8 {
            long.extend_from_slice(&event);
        }
        let (sink, receiver) = mpsc::channel();
        drop(receiver);
        drive(long.as_slice(), |raw| vec![dummy_pointer(raw)], &sink);
    }

    fn dummy_pointer(_raw: RawEvent) -> paper_sdk::PointerEvent {
        paper_sdk::PointerEvent::new(
            paper_sdk::Point::new(0.0, 0.0),
            paper_sdk::PointerPhase::Down,
            paper_sdk::Pointer::Touch,
            paper_sdk::ContactId::new(0),
        )
    }
}
