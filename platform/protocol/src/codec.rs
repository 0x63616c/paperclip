//! Framing: how a message becomes bytes, and how bytes stop being trusted.
//!
//! A frame is a four-byte little-endian length followed by that many bytes of
//! JSON. That is the entire format.
//!
//! ## Why JSON
//!
//! The wire carries control messages at a rate of a few hundred per second at
//! the very worst — ten touch contacts at the panel's refresh rate — and never
//! carries pixels. At that rate a compact binary encoding saves nothing worth
//! measuring, and it costs the thing that is actually scarce on this project: a
//! frame you can read in a log while a tablet is doing something wrong in a
//! room with no debugger. The e-ink panel, not the socket, is the bottleneck.
//!
//! ## Where the trust boundary is
//!
//! [`read_message`] is the only place bytes from another process become a
//! typed value, so it is the only place the limits have to hold. The length
//! prefix is checked against [`MAX_MESSAGE_BYTES`] **before a buffer is
//! allocated**: a hostile prefix of `0xFFFFFFFF` costs four bytes and an
//! error, not four gigabytes. A short read after a valid prefix is
//! [`CodecError::Truncated`] rather than a partial value.
//!
//! This is the layer an app that ignores the SDK entirely still has to get
//! past, which is why none of its enforcement is in the SDK.

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::limits::MAX_MESSAGE_BYTES;

/// Bytes of length prefix in front of every frame.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Why a frame could not be read or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodecError {
    /// The peer closed the connection between frames. Not an error in itself:
    /// this is what a clean shutdown looks like from the reading side.
    #[error("the connection was closed")]
    Closed,

    /// A frame claimed a length over [`MAX_MESSAGE_BYTES`].
    ///
    /// Reported without reading the body, so the cost of a hostile prefix is
    /// the prefix.
    #[error("message frame claims {len} bytes, over the {max} byte limit")]
    TooLarge {
        /// What the prefix claimed.
        len: u64,
        /// The limit.
        max: usize,
    },

    /// The connection ended part way through a frame.
    #[error("message frame ended after {read} of {expected} bytes")]
    Truncated {
        /// How much arrived.
        read: usize,
        /// How much the prefix promised.
        expected: usize,
    },

    /// The frame was not a message this protocol version knows.
    #[error("message frame is not a valid protocol message")]
    Malformed(#[source] serde_json::Error),

    /// The message could not be encoded, which is a bug in the sender.
    #[error("message could not be encoded")]
    Encode(#[source] serde_json::Error),

    /// The underlying transport failed.
    #[error("transport failure")]
    Io(#[source] io::Error),
}

/// Encodes a message to its frame body.
///
/// Refuses to produce a body over [`MAX_MESSAGE_BYTES`] rather than writing
/// one the peer is obliged to reject. A sender that can emit a message the
/// receiver must kill it for is a protocol with a trap in it.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, CodecError> {
    let body = serde_json::to_vec(message).map_err(CodecError::Encode)?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(CodecError::TooLarge {
            len: body.len() as u64,
            max: MAX_MESSAGE_BYTES,
        });
    }
    Ok(body)
}

/// Decodes a frame body.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, CodecError> {
    serde_json::from_slice(body).map_err(CodecError::Malformed)
}

/// Writes one framed message.
///
/// The prefix and the body go out in a single `write_all`, so a reader never
/// sees a length with no message behind it.
pub fn write_message<W: Write, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), CodecError> {
    let body = encode(message)?;
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    writer.write_all(&frame).map_err(CodecError::Io)?;
    writer.flush().map_err(CodecError::Io)
}

/// Reads one framed message.
///
/// Returns [`CodecError::Closed`] when the peer hung up cleanly between
/// frames, which callers should treat as the end of the conversation rather
/// than as a failure.
pub fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, CodecError> {
    let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
    match read_exact_or_eof(reader, &mut prefix)? {
        0 => return Err(CodecError::Closed),
        n if n < LENGTH_PREFIX_BYTES => {
            return Err(CodecError::Truncated {
                read: n,
                expected: LENGTH_PREFIX_BYTES,
            });
        }
        _ => {}
    }

    let len = u32::from_le_bytes(prefix) as usize;
    // Before the allocation, deliberately. This is the whole point of the
    // length prefix being checked rather than trusted.
    if len > MAX_MESSAGE_BYTES {
        return Err(CodecError::TooLarge {
            len: len as u64,
            max: MAX_MESSAGE_BYTES,
        });
    }

    let mut body = vec![0u8; len];
    let read = read_exact_or_eof(reader, &mut body)?;
    if read < len {
        return Err(CodecError::Truncated {
            read,
            expected: len,
        });
    }
    decode(&body)
}

/// Fills `buf`, returning how much arrived before EOF.
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize, CodecError> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(CodecError::Io(error)),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::{CodecError, LENGTH_PREFIX_BYTES, read_message, write_message};
    use crate::limits::MAX_MESSAGE_BYTES;
    use crate::message::{AppMessage, Diagnostic, DiagnosticLevel, Ready};
    use crate::version::CURRENT;

    fn ready() -> AppMessage {
        AppMessage::Ready(Ready { protocol: CURRENT })
    }

    #[test]
    fn a_message_survives_the_round_trip() {
        let mut wire = Vec::new();
        write_message(&mut wire, &ready()).unwrap();
        let back: AppMessage = read_message(&mut wire.as_slice()).unwrap();
        assert_eq!(back, ready());
    }

    #[test]
    fn several_messages_read_back_in_order() {
        let mut wire = Vec::new();
        write_message(&mut wire, &ready()).unwrap();
        write_message(
            &mut wire,
            &AppMessage::Diagnostic(Diagnostic::new(DiagnosticLevel::Info, "second")),
        )
        .unwrap();
        let mut reader = wire.as_slice();
        assert_eq!(read_message::<_, AppMessage>(&mut reader).unwrap(), ready());
        assert!(matches!(
            read_message::<_, AppMessage>(&mut reader).unwrap(),
            AppMessage::Diagnostic(_)
        ));
        assert!(matches!(
            read_message::<_, AppMessage>(&mut reader),
            Err(CodecError::Closed)
        ));
    }

    /// The hostile case this whole layer exists for: a four-byte prefix that
    /// promises four gigabytes. It must be refused on the prefix, without the
    /// body ever being allocated — this test would exhaust memory rather than
    /// fail if the check moved after the `vec![0; len]`.
    #[test]
    fn an_enormous_length_prefix_is_refused_before_allocating() {
        let mut wire = u32::MAX.to_le_bytes().to_vec();
        wire.extend_from_slice(b"{}");
        let error = read_message::<_, AppMessage>(&mut wire.as_slice()).unwrap_err();
        assert!(
            matches!(error, CodecError::TooLarge { len, max }
                if len == u64::from(u32::MAX) && max == MAX_MESSAGE_BYTES),
            "{error:?}"
        );
    }

    /// A body one byte over the limit is refused even though the prefix is
    /// perfectly well formed.
    #[test]
    fn an_oversized_body_is_refused() {
        let len = MAX_MESSAGE_BYTES + 1;
        let mut wire = (len as u32).to_le_bytes().to_vec();
        wire.extend(std::iter::repeat_n(b' ', len));
        assert!(matches!(
            read_message::<_, AppMessage>(&mut wire.as_slice()),
            Err(CodecError::TooLarge { .. })
        ));
    }

    /// And a sender cannot produce one: the trap of "legal to write, fatal to
    /// receive" does not exist here.
    #[test]
    fn an_oversized_message_cannot_be_written_either() {
        let huge = AppMessage::Request(crate::lifecycle::Request::Redraw);
        let mut wire = Vec::new();
        write_message(&mut wire, &huge).expect("a small message is fine");

        // Encode something genuinely over the limit by hand.
        let body = vec![0u8; MAX_MESSAGE_BYTES + 1];
        let error = super::encode(&body).unwrap_err();
        assert!(matches!(error, CodecError::TooLarge { .. }), "{error:?}");
    }

    #[test]
    fn a_frame_that_stops_half_way_is_truncated_not_partial() {
        let mut wire = Vec::new();
        write_message(&mut wire, &ready()).unwrap();
        wire.truncate(wire.len() - 3);
        let error = read_message::<_, AppMessage>(&mut wire.as_slice()).unwrap_err();
        assert!(matches!(error, CodecError::Truncated { .. }), "{error:?}");

        let stub = vec![0u8; LENGTH_PREFIX_BYTES - 1];
        assert!(matches!(
            read_message::<_, AppMessage>(&mut stub.as_slice()),
            Err(CodecError::Truncated { .. })
        ));
    }

    /// Well-framed rubbish is still rubbish. An app is free to write whatever
    /// it likes into the socket; it does not become a message by being the
    /// right length.
    #[test]
    fn well_framed_nonsense_is_not_a_message() {
        for body in [
            &b"{}"[..],
            &b"not json at all"[..],
            &br#"{"type":"invented"}"#[..],
            &br#"{"type":"ready"}"#[..], // right tag, missing `protocol`
        ] {
            let mut wire = (body.len() as u32).to_le_bytes().to_vec();
            wire.extend_from_slice(body);
            assert!(
                matches!(
                    read_message::<_, AppMessage>(&mut wire.as_slice()),
                    Err(CodecError::Malformed(_))
                ),
                "accepted {:?}",
                String::from_utf8_lossy(body)
            );
        }
    }
}
