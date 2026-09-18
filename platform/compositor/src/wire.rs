//! What a client says to the compositor's socket, and what it hears back
//! (WWW-78).
//!
//! Deliberately not `paper_protocol`'s `HostMessage`/`AppMessage`: those two
//! enums are closed by that crate's own doc comment, and ADR-0033 left the
//! question of whether this wire ever merges into them for WWW-81, once a
//! real client exists to design that against. This is the "distinct wire
//! format" ADR-0033 named as the alternative.
//!
//! [`ClientHello`] is the one message that never travels through
//! [`paper_protocol::codec`] alone — it always arrives with a file
//! descriptor attached as `SCM_RIGHTS` ancillary data (see [`crate::fdpass`]),
//! which is the fd [`crate::pool::Pool::from_file`] maps. Every message
//! after it is a plain framed [`ClientRequest`]/[`HostEvent`].

use paper_protocol::{BufferSlot, Damage, ShmPoolDescriptor};

/// Which role a client registers as.
///
/// Exactly one connected client may be [`Self::Home`] at a time — the
/// compositor's fallback target once the foreground app dies or hangs. A
/// second `Home` registration replaces the first rather than being refused:
/// the compositor has no way to tell a restarted Home from an impostor, and
/// that arbitration is the supervisor's job (WWW-4), not this wire's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientRole {
    /// The app the compositor falls back to when the foreground app dies or
    /// hangs.
    Home,
    /// Any other app.
    App,
}

/// A client's opening message. Always first, always exactly once, and the
/// only message that arrives with an fd attached.
///
/// `label` names the client in diagnostics and in the "`<label> stopped`"
/// frame [`crate::stopped`] renders if this client dies while foreground —
/// not a security-relevant identity claim, the same way an app's own
/// [`Diagnostic`](paper_protocol::Diagnostic) message never carries an
/// enforced identity (`platform/protocol/src/message.rs`'s "An app never
/// says who it is"). Bounded only by
/// [`MAX_MESSAGE_BYTES`](paper_protocol::MAX_MESSAGE_BYTES), the same choke
/// point every other message on this wire already goes through — a second,
/// narrower bound here would duplicate a limit `limits.rs` already owns.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientHello {
    /// Which role this client is registering as.
    pub role: ClientRole,
    /// The name used in diagnostics and the stopped-frame text.
    pub label: String,
    /// The shape of the pool whose fd accompanies this message.
    pub pool: ShmPoolDescriptor,
}

/// Everything a client may say after [`ClientHello`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
#[non_exhaustive]
pub enum ClientRequest {
    /// Attach a buffer slot ([`Surface::attach`](crate::Surface::attach)).
    Attach(BufferSlot),
    /// Commit the attached buffer ([`Surface::commit`](crate::Surface::commit)).
    Commit(Damage),
}

/// Everything the compositor may say to a client after [`ClientHello`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
#[non_exhaustive]
pub enum HostEvent {
    /// The compositor is done reading this slot; the client may attach it
    /// again ([`Surface::release`](crate::Surface::release)).
    Released(BufferSlot),
}

#[cfg(test)]
mod tests {
    use super::{ClientHello, ClientRequest, ClientRole, HostEvent};
    use paper_protocol::{
        BufferSlot, Damage, PixelFormat, ShmPoolDescriptor, Size, SurfaceDescriptor,
    };

    #[test]
    fn a_hello_round_trips_through_its_wire_form() {
        let hello = ClientHello {
            role: ClientRole::App,
            label: "Chess".to_owned(),
            pool: ShmPoolDescriptor {
                buffer: SurfaceDescriptor::packed(Size::new(100, 100), PixelFormat::Argb8888),
            },
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert_eq!(serde_json::from_str::<ClientHello>(&json).unwrap(), hello);
    }

    #[test]
    fn requests_and_events_round_trip_through_their_wire_form() {
        let attach = ClientRequest::Attach(BufferSlot::A);
        let json = serde_json::to_string(&attach).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientRequest>(&json).unwrap(),
            attach
        );

        let commit = ClientRequest::Commit(Damage::Full);
        let json = serde_json::to_string(&commit).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientRequest>(&json).unwrap(),
            commit
        );

        let released = HostEvent::Released(BufferSlot::B);
        let json = serde_json::to_string(&released).unwrap();
        assert_eq!(serde_json::from_str::<HostEvent>(&json).unwrap(), released);
    }
}
