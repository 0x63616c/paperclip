//! What the supervisor has to have actually done before it counts as up (§13).
//!
//! # Why this is not `is_active`
//!
//! §13 states it plainly: **process startup alone is not health evidence.**
//! `Type=notify` already buys more than a pid — `active` means the process
//! sent `READY=1` — but it is still one bit, and one bit cannot say *how far*
//! a supervisor got before it stopped getting further. A platform update that
//! rolls back on "it did not become active" can report only that something was
//! wrong. A platform update that rolls back on "it never got past
//! `device-adapter`" has told whoever reads the journal where to look.
//!
//! So the supervisor publishes the rung it has reached, in its status file,
//! as it reaches it. The ladder is:
//!
//! ```text
//! process           the unit has a main pid and it is alive
//! control           the command and status files exist; the supervisor is addressable
//! protocol          the protocol version this build speaks is recorded
//! device-adapter    the facilities probe succeeded and the stock adapter exists
//! home              the selected release's Home binary is an executable this machine can run
//! ready             READY=1 sent
//! ```
//!
//! Each rung is a fact about the machine, checkable after the fact by someone
//! reading the status file with `cat`. None of them is "a function was
//! called".
//!
//! # Who reads it
//!
//! The updater, from outside the process, and only from outside: a supervisor
//! that grades its own health is the liveness check this module exists to
//! replace. The updater additionally re-checks the `home` rung itself, because
//! that one is verifiable without trusting the process under test.

use std::fmt;
use std::str::FromStr;

/// The field the supervisor writes the reached rung under, in its status file.
pub const STATUS_FIELD: &str = "ready";

/// How far a supervisor has got.
///
/// Ordered: `Rung::Ready` is greater than every other, and a supervisor never
/// goes down a rung while it is running. Comparing is how "did it get at least
/// this far" is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rung {
    /// The process exists. The weakest claim there is, and the one §13 says is
    /// not evidence on its own.
    Process,
    /// The command and status files exist: something can talk to it.
    Control,
    /// The protocol version this build speaks has been recorded.
    Protocol,
    /// The facilities probe succeeded and the stock adapter was constructed.
    DeviceAdapter,
    /// The selected release's Home binary is present and executable here.
    Home,
    /// `READY=1` sent. The session is up.
    Ready,
}

impl Rung {
    /// Every rung, lowest first.
    pub const LADDER: [Rung; 6] = [
        Rung::Process,
        Rung::Control,
        Rung::Protocol,
        Rung::DeviceAdapter,
        Rung::Home,
        Rung::Ready,
    ];

    /// The name used in the status file and in reports.
    pub fn label(self) -> &'static str {
        match self {
            Rung::Process => "process",
            Rung::Control => "control",
            Rung::Protocol => "protocol",
            Rung::DeviceAdapter => "device-adapter",
            Rung::Home => "home",
            Rung::Ready => "ready",
        }
    }

    /// What reaching this rung means, for a report a person reads.
    pub fn describe(self) -> &'static str {
        match self {
            Rung::Process => "the supervisor process is alive",
            Rung::Control => "the supervisor's command and status files exist",
            Rung::Protocol => "the supervisor recorded the protocol version it speaks",
            Rung::DeviceAdapter => "the facilities probe succeeded and the stock adapter is up",
            Rung::Home => "the selected release's Home binary runs on this machine",
            Rung::Ready => "the supervisor sent READY=1",
        }
    }

    /// The rung above this one, if there is one.
    pub fn next(self) -> Option<Rung> {
        Rung::LADDER
            .iter()
            .copied()
            .find(|rung| *rung > self)
            .filter(|_| self != Rung::Ready)
    }
}

impl fmt::Display for Rung {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

impl FromStr for Rung {
    type Err = UnknownRung;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Rung::LADDER
            .iter()
            .copied()
            .find(|rung| rung.label() == text)
            .ok_or_else(|| UnknownRung(text.to_owned()))
    }
}

/// A rung name that is not one of the six.
///
/// Deliberately an error rather than a lenient `None`. A status file carrying
/// a rung this build does not know is a version mismatch between the updater
/// and the release it is grading, and treating it as "not ready yet" would
/// turn that into a rollback with a misleading reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a readiness rung")]
pub struct UnknownRung(String);

/// The rung a supervisor has reached, as it climbs.
///
/// Monotone by construction: [`Self::reached`] never lowers the recorded rung,
/// so a transient failure late in startup cannot make the status file claim
/// the supervisor went backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ladder {
    reached: Rung,
}

impl Default for Ladder {
    fn default() -> Self {
        Self::new()
    }
}

impl Ladder {
    /// A ladder at the bottom rung: the process exists, and nothing else is
    /// claimed.
    pub fn new() -> Self {
        Self {
            reached: Rung::Process,
        }
    }

    /// Records that `rung` has been reached.
    pub fn reached(&mut self, rung: Rung) {
        if rung > self.reached {
            self.reached = rung;
        }
    }

    /// The highest rung reached so far.
    pub fn highest(self) -> Rung {
        self.reached
    }

    /// Whether the supervisor got at least as far as `rung`.
    pub fn at_least(self, rung: Rung) -> bool {
        self.reached >= rung
    }
}

#[cfg(test)]
mod tests {
    use super::{Ladder, Rung};

    #[test]
    fn the_ladder_is_ordered_lowest_first() {
        let ladder = Rung::LADDER;
        assert!(ladder.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(ladder.first().copied(), Some(Rung::Process));
        assert_eq!(ladder.last().copied(), Some(Rung::Ready));
    }

    #[test]
    fn every_rung_round_trips_through_its_label() {
        for rung in Rung::LADDER {
            assert_eq!(rung.label().parse::<Rung>(), Ok(rung));
        }
    }

    #[test]
    fn an_unknown_rung_is_an_error_not_a_silent_bottom() {
        assert!("nearly".parse::<Rung>().is_err());
    }

    #[test]
    fn a_ladder_never_goes_down() {
        let mut ladder = Ladder::new();
        ladder.reached(Rung::Home);
        ladder.reached(Rung::Control);
        assert_eq!(ladder.highest(), Rung::Home);
        assert!(ladder.at_least(Rung::Protocol));
        assert!(!ladder.at_least(Rung::Ready));
    }

    #[test]
    fn ready_is_the_top() {
        assert_eq!(Rung::Ready.next(), None);
        assert_eq!(Rung::Process.next(), Some(Rung::Control));
    }
}
