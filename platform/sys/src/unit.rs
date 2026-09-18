//! One systemd unit, behind a seam.
//!
//! Generalises `platform/device`'s `stock::ServiceControl` (one implicit
//! unit, `xochitl.service`) and `platform/host`'s concrete `Systemd` (every
//! method already took a unit name, and had zero tests) into one trait, named
//! by unit, with one real adapter. The adapter is built on [`Process`] rather
//! than shelling out itself, so its own argv-building can be checked with a
//! fake process instead of a real `systemctl`.

use std::fmt;
use std::time::Duration;

use crate::clock::Clock;
use crate::process::{Process, ProcessCommand, SystemProcess};

/// Why a unit operation could not even be attempted.
///
/// Distinct from the unit refusing the operation, which is a `false` return
/// or an inactive unit, not an error — see the trait's own methods.
#[derive(Debug, thiserror::Error)]
#[error("systemctl {verb} {unit} failed: {reason}")]
pub struct UnitError {
    /// `start`, `stop`, or `reset-failed`.
    pub verb: &'static str,
    /// The unit.
    pub unit: String,
    /// What `systemctl` said, or why it could not be run at all.
    pub reason: String,
}

/// The systemd operations this workspace performs on a named unit.
///
/// Deliberately no `kill` and no `restart` — `platform/device::stock`'s own
/// doc comment on `ServiceControl` explains why neither is ever correct for
/// `xochitl.service`, and nothing else in this workspace needs them either.
pub trait UnitControl: fmt::Debug {
    /// `systemctl start <unit>`.
    ///
    /// # Errors
    ///
    /// If `systemctl` could not be run at all. A start that is accepted but
    /// does not bring the unit up is not an error — see [`UnitControl::is_active`].
    fn start(&self, unit: &str) -> Result<(), UnitError>;

    /// `systemctl stop <unit>`. Clean stop; never a signal.
    ///
    /// # Errors
    ///
    /// If `systemctl` refused, or could not be run at all.
    fn stop(&self, unit: &str) -> Result<(), UnitError>;

    /// `systemctl reset-failed <unit>`, which also clears the start rate
    /// limiter.
    ///
    /// # Errors
    ///
    /// If `systemctl` could not be run at all. A unit that was not failed is
    /// not an error.
    fn reset_failed(&self, unit: &str) -> Result<(), UnitError>;

    /// Whether the unit is active right now.
    fn is_active(&self, unit: &str) -> bool;

    /// Whether systemd reports the unit as failed.
    fn is_failed(&self, unit: &str) -> bool;

    /// The unit's main PID, if it has one.
    fn main_pid(&self, unit: &str) -> Option<u32>;

    /// One property, by name, e.g. `NRestarts` or `ExecMainStartTimestamp`.
    fn property(&self, unit: &str, name: &str) -> Option<String>;
}

/// Blocks until `unit` is active or `timeout` elapses.
///
/// A free function, not a trait method: it composes a [`UnitControl`] with a
/// [`Clock`], and every caller that wants a bounded wait already has both.
pub fn wait_active(
    control: &dyn UnitControl,
    clock: &dyn Clock,
    unit: &str,
    timeout: Duration,
) -> bool {
    let deadline = clock.now() + timeout;
    loop {
        if control.is_active(unit) {
            return true;
        }
        if clock.now() >= deadline {
            return false;
        }
        clock.sleep(Duration::from_millis(500).min(timeout));
    }
}

/// [`UnitControl`] over the real `systemctl`, via an injected [`Process`].
///
/// Its argv-building is exercised against `paper_testing::sys::FakeProcess`
/// rather than here: that fake is a dev-dependency of `paper_testing`'s own
/// consumers, and `paper_testing` depends on this crate (not the other way
/// round), so a test here that needed the fake would create the dependency
/// cycle Cargo's dev-dependency support does not extend to a package
/// depending on itself through a middleman. See
/// `platform/testing/src/sys.rs`'s `systemctl` tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct Systemctl<P: Process = SystemProcess> {
    process: P,
}

impl<P: Process> Systemctl<P> {
    /// A `systemctl` adapter that spawns through `process`.
    pub fn new(process: P) -> Self {
        Self { process }
    }

    fn run(&self, args: &[&str]) -> Result<(bool, String), String> {
        let command = ProcessCommand::new("systemctl").args(args.iter().copied());
        let output = self
            .process
            .run(&command)
            .map_err(|error| error.to_string())?;
        Ok((output.success, output.stdout.trim().to_owned()))
    }
}

impl<P: Process> UnitControl for Systemctl<P> {
    fn start(&self, unit: &str) -> Result<(), UnitError> {
        let (ok, text) = self.run(&["start", unit]).map_err(|reason| UnitError {
            verb: "start",
            unit: unit.to_owned(),
            reason,
        })?;
        if ok {
            Ok(())
        } else {
            Err(UnitError {
                verb: "start",
                unit: unit.to_owned(),
                reason: text,
            })
        }
    }

    fn stop(&self, unit: &str) -> Result<(), UnitError> {
        let (ok, text) = self.run(&["stop", unit]).map_err(|reason| UnitError {
            verb: "stop",
            unit: unit.to_owned(),
            reason,
        })?;
        if ok {
            Ok(())
        } else {
            Err(UnitError {
                verb: "stop",
                unit: unit.to_owned(),
                reason: text,
            })
        }
    }

    fn reset_failed(&self, unit: &str) -> Result<(), UnitError> {
        self.run(&["reset-failed", unit])
            .map(|_| ())
            .map_err(|reason| UnitError {
                verb: "reset-failed",
                unit: unit.to_owned(),
                reason,
            })
    }

    fn is_active(&self, unit: &str) -> bool {
        self.run(&["is-active", unit])
            .map(|(_, text)| text == "active")
            .unwrap_or(false)
    }

    fn is_failed(&self, unit: &str) -> bool {
        self.run(&["is-failed", unit])
            .map(|(_, text)| text == "failed")
            .unwrap_or(false)
    }

    fn main_pid(&self, unit: &str) -> Option<u32> {
        self.run(&["show", "-p", "MainPID", "--value", unit])
            .ok()
            .and_then(|(_, text)| text.parse().ok())
            .filter(|pid| *pid != 0)
    }

    fn property(&self, unit: &str, name: &str) -> Option<String> {
        self.run(&["show", "-p", name, "--value", unit])
            .ok()
            .map(|(_, text)| text)
            .filter(|text| !text.is_empty())
    }
}
