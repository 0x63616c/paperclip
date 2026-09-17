//! Grading a candidate release: the readiness ladder, bounded (§13).
//!
//! # What this refuses to accept as evidence
//!
//! That the process exists. That the unit went `active`. That nothing has
//! crashed yet. §13 says it in one line — *process startup alone is not health
//! evidence* — and everything in this module is that line made operational.
//!
//! What counts instead is [`paper_host::readiness::Rung`]: the supervisor
//! publishes how far it got, and this module waits for `ready` and reports the
//! rung it stalled on when it does not arrive. The difference is not
//! pedantic. "The update failed" sends someone to the tablet. "The update
//! stalled at `device-adapter`" sends them to the display stack.
//!
//! # Bounded, and bounded in two directions
//!
//! A deadline, and no retries. [`Budget::deadline`] caps how long a candidate
//! has to climb; reaching it is a failure, not a reason to wait longer. And
//! there is no loop around [`watch`] anywhere — §13 forbids the unbounded
//! retry, and the way to not write one is to not have a function that could
//! contain it.
//!
//! # The seam
//!
//! [`SessionControl`] is the only thing here that touches the machine, and it
//! is a trait so the transaction can be tested where there is no systemd. That
//! is a seam for *tests of the decisions*, not a substitute for the VM: a Mac
//! test proves the transaction rolls back when a candidate stalls, and only
//! `tests/failure-harness` proves a candidate that stalls is detected.

use std::fmt;
use std::time::{Duration, Instant};

use paper_host::readiness::Rung;
use paper_protocol::ProtocolVersion;

use crate::error::Unhealthy;

/// How long a candidate gets, and how often it is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// How long the whole climb may take.
    pub deadline: Duration,
    /// How often to look.
    pub poll: Duration,
}

/// How long a candidate gets to climb.
///
/// **Derived, and deliberately well under
/// [`HOST_WATCHDOG`](paper_host::units::HOST_WATCHDOG).**
///
/// The ladder is climbed at startup, before the supervisor's main loop begins.
/// It contains no restore and no session start; the slowest rung is
/// `device-adapter`, which runs the facilities probe. Thirty seconds is an
/// order of magnitude more than that needs and is still a bound.
///
/// The upper limit is what matters and is not a matter of taste. A deadline at
/// or above the watchdog means systemd kills a stalled candidate — for not
/// petting a watchdog it never reached the main loop to pet — at the same
/// moment the updater is grading it. The rollback then happens either way, but
/// the reported reason becomes "the supervisor exited" rather than "it stalled
/// at `protocol`", and the useful half of the report is lost to a race. The VM
/// harness showed exactly that, with both stall cases taking 80.3s and 80.5s
/// against an 80s watchdog.
///
/// The updater decides, not systemd.
pub const CLIMB_DEADLINE: Duration = Duration::from_secs(30);

impl Default for Budget {
    fn default() -> Self {
        Self {
            deadline: CLIMB_DEADLINE,
            poll: Duration::from_millis(250),
        }
    }
}

/// One look at the session, from outside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// Whether the supervisor process is there at all.
    pub alive: bool,
    /// The highest rung it claims, if it has published one.
    pub reached: Option<Rung>,
    /// The protocol it says it speaks.
    pub protocol: Option<ProtocolVersion>,
    /// Whatever it said about why it stopped climbing.
    pub note: String,
}

impl Observation {
    /// Nothing there.
    pub fn absent() -> Self {
        Self {
            alive: false,
            reached: None,
            protocol: None,
            note: String::new(),
        }
    }
}

/// Everything the transaction does to the running system.
///
/// Deliberately small, and deliberately without a `restart`. §13's sequence is
/// *stand down to stock, then bring the new one up*, and a `restart` would let
/// a caller skip the stand-down — which on this device means swapping the
/// binary under a session that still owns the panel.
pub trait SessionControl: fmt::Debug {
    /// Stops the Paperclip session and returns the display to stock.
    ///
    /// Must **start** stock, never kill it, and must not retry into
    /// `StartLimitBurst=4`. A failed `xochitl.service` drops the tablet onto a
    /// serial console; `remarkable-fail.service`, which its `OnFailure=`
    /// names, does not exist on this image.
    ///
    /// # Errors
    ///
    /// If stock did not come back. The caller must not proceed.
    fn stand_down(&self) -> Result<(), String>;

    /// Starts the supervisor from whatever `current` now points at.
    ///
    /// # Errors
    ///
    /// If the unit could not be started at all. A unit that starts and then
    /// fails to climb is not an error here — that is what the ladder is for.
    fn bring_up(&self) -> Result<(), String>;

    /// One look at the supervisor.
    fn observe(&self) -> Observation;

    /// Whether stock owns the display.
    fn stock_is_up(&self) -> bool;

    /// Whether Paperclip's wakelock is held.
    fn wakelock_held(&self) -> bool;

    /// Releases Paperclip's wakelock by name.
    ///
    /// By name, with no handle: the process that took it is gone, which is the
    /// only situation in which this is called.
    fn release_wakelock(&self);
}

/// The clock the watch runs against.
///
/// A trait so a unit test can grade a candidate in microseconds. The device
/// and the VM both use [`SystemClock`].
pub trait Clock: fmt::Debug {
    /// Now.
    fn now(&self) -> Instant;
    /// Waits.
    fn sleep(&self, duration: Duration);
}

/// The real one.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// How a candidate did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// The highest rung observed.
    pub reached: Rung,
    /// Whether it got to [`Rung::Ready`].
    pub healthy: bool,
    /// Whether the supervisor was gone when the watch ended.
    pub died: bool,
    /// How long it took, or how long it was given.
    pub elapsed: Duration,
    /// What the supervisor said about the rung it stopped on.
    pub note: String,
}

impl HealthReport {
    /// The failure, if this is one.
    pub fn failure(&self) -> Option<Unhealthy> {
        if self.healthy {
            return None;
        }
        Some(Unhealthy {
            reached: self.reached,
            stalled_at: self.reached.next().unwrap_or(Rung::Ready),
            note: self.note.clone(),
            died: self.died,
        })
    }

    /// A line for a report.
    pub fn summary(&self) -> String {
        if self.healthy {
            return format!("ready in {:.1}s", self.elapsed.as_secs_f32());
        }
        match self.failure() {
            Some(failure) => format!("{failure} after {:.1}s", self.elapsed.as_secs_f32()),
            None => "ready".to_owned(),
        }
    }
}

/// Watches a candidate climb, once, until it is ready or out of time.
///
/// Returns as soon as [`Rung::Ready`] is observed, and as soon as the
/// supervisor is observed gone *after* having been seen — a process that has
/// exited will not climb further, and waiting out the deadline for it would
/// only delay the rollback.
///
/// Never retries and never restarts anything. Both are the caller's decision,
/// and the caller has exactly one of each to spend.
pub fn watch(session: &dyn SessionControl, clock: &dyn Clock, budget: Budget) -> HealthReport {
    let started = clock.now();
    let mut best = Rung::Process;
    let mut note = String::new();
    let mut seen_alive = false;

    loop {
        let observation = session.observe();
        if observation.alive {
            seen_alive = true;
        }
        if let Some(reached) = observation.reached
            && reached > best
        {
            best = reached;
        }
        if !observation.note.is_empty() {
            note = observation.note.clone();
        }

        if best == Rung::Ready {
            return HealthReport {
                reached: best,
                healthy: true,
                died: false,
                elapsed: clock.now().saturating_duration_since(started),
                note,
            };
        }

        if seen_alive && !observation.alive {
            return HealthReport {
                reached: best,
                healthy: false,
                died: true,
                elapsed: clock.now().saturating_duration_since(started),
                note: if note.is_empty() {
                    "the supervisor exited before reaching `ready`".to_owned()
                } else {
                    note
                },
            };
        }

        let elapsed = clock.now().saturating_duration_since(started);
        if elapsed >= budget.deadline {
            return HealthReport {
                reached: best,
                healthy: false,
                died: !observation.alive,
                elapsed,
                note: if note.is_empty() {
                    format!("deadline of {:.0}s expired", budget.deadline.as_secs_f32())
                } else {
                    note
                },
            };
        }

        clock.sleep(budget.poll);
    }
}
