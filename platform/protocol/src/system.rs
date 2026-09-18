//! System facts an app may ask for with no grant, and the push channel that
//! tells it when one changed without being asked (WWW-50).
//!
//! Before this module, an app's whole vocabulary was five messages and none
//! of them could ask the platform anything (see the module doc on
//! [`crate::message`]). This is the first addition to that vocabulary, scoped
//! to what Android's own precedent treats as safe to hand out unconditionally:
//! current battery state, current network connectivity, wall-clock time and
//! platform version facts. Historical or per-app accounting, and anything
//! that changes install state, is a different, granted surface and is not
//! part of this module — see ADR-0028.
//!
//! **Push, do not poll.** [`SystemEvent`] exists so a host can tell an app the
//! battery or network changed without the app asking on a timer. On e-ink,
//! polling means redrawing, and redrawing costs a visible flash.
//!
//! **Denials are machine-readable.** [`SystemDenial`] distinguishes a backend
//! that has nothing to report from an app that asked too often, rather than
//! collapsing both into one generic failure.
//!
//! **The vocabulary grew a second time.** [`SystemQueryKind::Admin`] and
//! [`SystemValue::Admin`] (WWW-71, ADR-0028) carry
//! [`crate::admin::AdminQuery`] and [`crate::admin::AdminValue`] — Settings'
//! nine admin operations, gated by caller identity rather than by a grant.
//! See [`crate::admin`] for that vocabulary and why it is not part of the
//! no-grant tier above.

use crate::admin::{AdminQuery, AdminValue};

/// Correlates a [`SystemQuery`] with the [`SystemAnswer`] it produced.
///
/// Issued by the app, increasing per connection — the mirror of
/// [`FrameId`](crate::FrameId), and for the same reason: the wire is
/// asynchronous, so an answer has to name which question it is answering
/// rather than relying on order. Answers may arrive interleaved with draw
/// requests and lifecycle events.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct QueryId(u64);

impl QueryId {
    /// The first query id of a session.
    pub const FIRST: QueryId = QueryId(0);

    /// Wraps a raw value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw value.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next query id.
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Which system fact, or admin operation, an app is asking for.
///
/// Not [`Copy`]: [`Self::Admin`] carries an [`AdminQuery`], which names an
/// [`AppId`](crate::AppId) it acts on and so cannot be. The four no-grant
/// facts above it stay cheap to construct either way.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SystemQueryKind {
    /// Wall-clock time.
    Time,
    /// Current battery state.
    Battery,
    /// Current network connectivity.
    Network,
    /// Platform version facts.
    Platform,
    /// One of Settings' nine admin operations (WWW-71). Gated by caller
    /// identity at the host, not by a grant — see [`crate::admin`].
    Admin(AdminQuery),
}

/// An app asking the host for one system fact.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SystemQuery {
    /// Matched against the [`SystemAnswer`] that answers it.
    pub id: QueryId,
    /// What is being asked.
    pub kind: SystemQueryKind,
}

/// Wall-clock time, in milliseconds since the Unix epoch.
///
/// Not [`Instant`](std::time::Instant): an app asking "what time is it" wants
/// a time it can show or compare against a saved timestamp, not a monotonic
/// counter with no fixed origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TimeFact {
    /// Milliseconds since the Unix epoch.
    pub unix_millis: u64,
}

/// Whether the battery is charging, and toward what.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum BatteryState {
    /// Drawing power from the mains and gaining charge.
    Charging,
    /// Running on battery.
    Discharging,
    /// Charging and at capacity.
    Full,
    /// The backend could not determine a state. Distinct from a denial: the
    /// battery itself answered, just not with a state this enum names yet.
    Unknown,
}

/// Current battery state — never a history, never per-app accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BatteryFact {
    /// 0–100.
    pub percent: u8,
    /// Charging, discharging, full or unknown.
    pub state: BatteryState,
}

/// Current network connectivity.
///
/// **Never credentials.** This is the informational, no-grant tier (§ project
/// description): what an app may learn about the network without being
/// granted anything is limited to whether it is connected, to what, and how
/// well — never a password, a token or anything that would let an app join a
/// network it was not already on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NetworkFact {
    /// Whether the device has a network connection right now.
    pub connected: bool,
    /// The network's name, when connected and the backend can name it.
    pub ssid: Option<String>,
    /// Signal strength, 0–100, when the backend can measure it.
    pub signal_percent: Option<u8>,
}

/// Platform version facts — the read-only half of
/// `paper_settings::host::PlatformInfo`, moved here because both the host and
/// any app asking `Platform` need the same shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlatformFact {
    /// Paperclip's own version.
    pub paperclip_version: String,
    /// The firmware image it is running on.
    pub firmware: String,
    /// The active platform release (Host, protocol support, Home, App Store,
    /// Settings — versioned together, per §13).
    pub active_release: String,
}

/// The value a [`SystemQuery`] resolved to.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "fact")]
#[non_exhaustive]
pub enum SystemValue {
    /// Answers [`SystemQueryKind::Time`].
    Time(TimeFact),
    /// Answers [`SystemQueryKind::Battery`].
    Battery(BatteryFact),
    /// Answers [`SystemQueryKind::Network`].
    Network(NetworkFact),
    /// Answers [`SystemQueryKind::Platform`].
    Platform(PlatformFact),
    /// Answers [`SystemQueryKind::Admin`].
    Admin(AdminValue),
}

/// Why a [`SystemQuery`] was refused.
///
/// A closed, machine-readable set rather than a string: an app that wants to
/// grey out a battery indicator needs to tell "nothing to report right now"
/// apart from "you are asking too fast", and a generic failure cannot say
/// that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SystemDenialReason {
    /// The host has no working backend for this fact right now — no battery
    /// device found, the network tool could not be run, and so on.
    BackendUnavailable,
    /// This app has sent more queries than
    /// [`MAX_SYSTEM_QUERIES_PER_SECOND`](crate::limits::MAX_SYSTEM_QUERIES_PER_SECOND)
    /// allows for.
    RateLimited,
    /// This host build does not know this query kind — a newer app speaking
    /// a minor protocol version ahead of an older host (protocol bumps are
    /// additive; see the crate root's versioning docs).
    Unsupported,
    /// An [`SystemQueryKind::Admin`] query arrived on a connection that is
    /// not `dev.calum.settings` (WWW-71). Distinct from
    /// [`Self::BackendUnavailable`]: the backend is fine, the caller is not
    /// entitled to ask.
    NotPermitted,
}

/// Why a [`SystemQuery`] was refused, with an optional human-readable detail
/// for a diagnostic line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SystemDenial {
    /// The machine-readable reason.
    pub reason: SystemDenialReason,
    /// Free text for a log line. Never structured, for the reason
    /// [`Diagnostic`](crate::Diagnostic) is not: a field like this is a field
    /// something eventually dumps a secret into.
    pub detail: Option<String>,
}

impl SystemDenial {
    /// Builds a denial with no further detail.
    pub const fn new(reason: SystemDenialReason) -> Self {
        Self {
            reason,
            detail: None,
        }
    }

    /// Builds a denial carrying a short explanation.
    pub fn with_detail(reason: SystemDenialReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: Some(detail.into()),
        }
    }
}

/// The host's answer to one [`SystemQuery`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SystemAnswer {
    /// Which query this answers.
    pub id: QueryId,
    /// The value, or why there is not one.
    pub result: Result<SystemValue, SystemDenial>,
}

/// A system fact changed, and the host is telling every app that can read it
/// without being asked again.
///
/// Push, not poll (module doc). Carries the same no-grant facts
/// [`SystemQuery`] can ask for — time is deliberately absent, since a clock
/// ticking is not a change worth an app redrawing over.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "changed")]
#[non_exhaustive]
pub enum SystemEvent {
    /// The battery state changed.
    Battery(BatteryFact),
    /// Network connectivity changed.
    Network(NetworkFact),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_id_round_trips_as_a_number() {
        let id = QueryId::new(41);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "41");
        assert_eq!(serde_json::from_str::<QueryId>(&json).unwrap(), id);
        assert_eq!(id.next(), QueryId::new(42));
    }

    #[test]
    fn an_answer_carries_either_a_value_or_a_machine_readable_denial() {
        let ok = SystemAnswer {
            id: QueryId::FIRST,
            result: Ok(SystemValue::Battery(BatteryFact {
                percent: 87,
                state: BatteryState::Discharging,
            })),
        };
        let json = serde_json::to_string(&ok).unwrap();
        assert_eq!(serde_json::from_str::<SystemAnswer>(&json).unwrap(), ok);

        let denied = SystemAnswer {
            id: QueryId::FIRST,
            result: Err(SystemDenial::new(SystemDenialReason::RateLimited)),
        };
        let json = serde_json::to_string(&denied).unwrap();
        assert!(json.contains("rate-limited"), "{json}");
        assert_eq!(serde_json::from_str::<SystemAnswer>(&json).unwrap(), denied);
    }

    #[test]
    fn a_denial_reason_is_distinguishable_from_a_generic_failure() {
        let unavailable = SystemDenial::with_detail(
            SystemDenialReason::BackendUnavailable,
            "no power_supply battery node",
        );
        assert_eq!(unavailable.reason, SystemDenialReason::BackendUnavailable);
        assert_eq!(
            unavailable.detail.as_deref(),
            Some("no power_supply battery node")
        );
    }
}
