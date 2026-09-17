//! Talking to systemd about Paperclip's own units, and the readiness protocol.
//!
//! Three things worth noticing.
//!
//! **There is no `kill`.** Not for our units and not for anything else; the
//! type has no method that sends a signal.
//!
//! **It cannot touch stock.** [`Systemd::start`] and [`Systemd::stop`] refuse
//! any unit whose name is not Paperclip's. Xochitl goes through
//! [`paper_device::stock::Stock`], which owns the start budget and the guarded
//! retry — and which has no `kill` and no `restart` either, because neither is
//! ever correct. That matters because Xochitl's effective `OnFailure=` is
//! `emergency.target remarkable-fail.service` and `remarkable-fail.service`
//! does not exist on this image: a *failed* Xochitl is a tablet on a serial
//! console with a blank screen.
//!
//! **Readiness is the real protocol, not a sleep.** `sd_notify` is a datagram
//! on `$NOTIFY_SOCKET` containing `READY=1`, so it needs no libsystemd and no
//! FFI — [`notify`] writes it with `std::os::unix::net`.

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Why a systemd interaction failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SystemdError {
    /// `systemctl` could not be run at all.
    #[error("cannot run {binary} {arguments}")]
    Spawn {
        /// The binary that was tried.
        binary: PathBuf,
        /// The arguments it was given.
        arguments: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// A caller tried to start or stop something that is not ours.
    #[error(
        "refusing to {verb} `{unit}`: this type only operates Paperclip's own units. \
         Stock goes through paper_device::stock::Stock, which owns the start budget."
    )]
    NotOurs {
        /// What was attempted.
        verb: &'static str,
        /// Which unit.
        unit: String,
    },

    /// `systemctl` ran and refused.
    #[error("`systemctl {arguments}` failed ({status}): {stderr}")]
    Rejected {
        /// The arguments it was given.
        arguments: String,
        /// Its exit status.
        status: String,
        /// What it said.
        stderr: String,
    },
}

/// A handle on the service manager.
#[derive(Debug, Clone)]
pub struct Systemd {
    binary: PathBuf,
    journal: PathBuf,
}

impl Default for Systemd {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("systemctl"),
            journal: PathBuf::from("journalctl"),
        }
    }
}

/// Whether `unit` is one of Paperclip's own.
///
/// Name-based, which is enough: the only thing on the other side of this check
/// that matters is `xochitl.service`, and it does not start with `paperclip`.
/// The VM harness's stand-in stock unit deliberately does, so the harness can
/// set it up — a hole in the fixture, not on the device.
fn is_ours(unit: &str) -> bool {
    unit.starts_with("paperclip")
}

impl Systemd {
    /// Runs `systemctl` with `arguments`.
    ///
    /// # Errors
    ///
    /// [`SystemdError::Spawn`] if it could not be run, [`SystemdError::Rejected`]
    /// if it ran and failed.
    pub fn run(&self, arguments: &[&str]) -> Result<String, SystemdError> {
        let output = Command::new(&self.binary)
            .args(arguments)
            .output()
            .map_err(|source| SystemdError::Spawn {
                binary: self.binary.clone(),
                arguments: arguments.join(" "),
                source,
            })?;
        if !output.status.success() {
            return Err(SystemdError::Rejected {
                arguments: arguments.join(" "),
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Reads one property, returning `None` when systemd has no answer.
    ///
    /// Never an error: "I could not ask" and "the answer is no" have to stay
    /// distinguishable, and every caller here treats an unknown as "assume
    /// nothing".
    pub fn property(&self, unit: &str, name: &str) -> Option<String> {
        let value = self
            .run(&["show", unit, "--property", name, "--value"])
            .ok()?;
        (!value.is_empty()).then_some(value)
    }

    /// Whether `unit` is active right now.
    pub fn is_active(&self, unit: &str) -> bool {
        self.property(unit, "ActiveState")
            .is_some_and(|state| state == "active")
    }

    /// Whether `unit` is in the failed state.
    pub fn is_failed(&self, unit: &str) -> bool {
        self.property(unit, "ActiveState")
            .is_some_and(|state| state == "failed")
    }

    /// The main pid of `unit`, when it has one.
    pub fn main_pid(&self, unit: &str) -> Option<u32> {
        self.property(unit, "MainPID")
            .and_then(|pid| pid.parse().ok())
            .filter(|pid| *pid > 1)
    }

    /// Re-reads unit files after writing into `/run/systemd/system`.
    ///
    /// # Errors
    ///
    /// Propagates whatever `systemctl daemon-reload` said.
    pub fn daemon_reload(&self) -> Result<(), SystemdError> {
        self.run(&["daemon-reload"]).map(|_| ())
    }

    /// Starts one of Paperclip's units.
    ///
    /// # Errors
    ///
    /// [`SystemdError::NotOurs`] for anything else; otherwise whatever
    /// `systemctl start` said.
    pub fn start(&self, unit: &str) -> Result<(), SystemdError> {
        if !is_ours(unit) {
            return Err(SystemdError::NotOurs {
                verb: "start",
                unit: unit.to_owned(),
            });
        }
        self.run(&["start", unit]).map(|_| ())
    }

    /// Stops one of Paperclip's units.
    ///
    /// # Errors
    ///
    /// [`SystemdError::NotOurs`] for anything else; otherwise whatever
    /// `systemctl stop` said.
    pub fn stop(&self, unit: &str) -> Result<(), SystemdError> {
        if !is_ours(unit) {
            return Err(SystemdError::NotOurs {
                verb: "stop",
                unit: unit.to_owned(),
            });
        }
        self.run(&["stop", unit]).map(|_| ())
    }

    /// Clears one of our units' failed state, so a later start is not refused
    /// by its own rate limiter.
    ///
    /// # Errors
    ///
    /// [`SystemdError::NotOurs`] for anything else; otherwise whatever
    /// `systemctl reset-failed` said.
    pub fn reset_failed(&self, unit: &str) -> Result<(), SystemdError> {
        if !is_ours(unit) {
            return Err(SystemdError::NotOurs {
                verb: "reset-failed",
                unit: unit.to_owned(),
            });
        }
        self.run(&["reset-failed", unit]).map(|_| ())
    }

    /// Waits for `unit` to become active, or gives up.
    pub fn wait_active(&self, unit: &str, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if self.is_active(unit) {
                return true;
            }
            if self.is_failed(unit) || Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// A unit's status text, for the diagnostics bundle.
    pub fn status_text(&self, unit: &str) -> String {
        self.run(&["status", unit, "--no-pager", "--full"])
            .unwrap_or_else(|error| format!("systemctl status {unit} failed: {error}"))
    }

    /// The last `lines` journal entries for `unit`.
    pub fn journal(&self, unit: &str, lines: u32) -> String {
        Command::new(&self.journal)
            .args([
                "-u",
                unit,
                "-n",
                &lines.to_string(),
                "--no-pager",
                "--output",
                "short-iso",
            ])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_else(|error| format!("journalctl -u {unit} failed: {error}"))
    }

    /// The cgroup directory systemd put `unit` in.
    pub fn cgroup_of(&self, unit: &str) -> Option<PathBuf> {
        let path = self.property(unit, "ControlGroup")?;
        Some(Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')))
    }
}

/// Sends one `sd_notify` message on `$NOTIFY_SOCKET`.
///
/// Returns `false` when there is no socket, which is the normal case outside
/// systemd and must not be an error: the harness runs pieces of this by hand.
pub fn notify(message: &str) -> bool {
    let Some(address) = std::env::var_os("NOTIFY_SOCKET") else {
        return false;
    };
    let Ok(socket) = UnixDatagram::unbound() else {
        return false;
    };
    let path = Path::new(&address);
    // systemd uses a filesystem socket for services. An abstract socket (a
    // leading '@') is spelled with a leading NUL on the wire; the std API
    // cannot address that portably, so it is reported as "not notified"
    // rather than silently dropped.
    if path.to_string_lossy().starts_with('@') {
        return false;
    }
    socket.send_to(message.as_bytes(), path).is_ok()
}

/// Tells systemd the service is up. Only meaningful under `Type=notify`.
pub fn notify_ready() -> bool {
    notify("READY=1")
}

/// Pets the watchdog. Called from the supervisor's own main loop, so a
/// supervisor that stops supervising is noticed by systemd rather than by
/// nobody.
pub fn notify_watchdog() -> bool {
    notify("WATCHDOG=1")
}

/// Tells systemd why the service is stopping, so it lands in the journal.
pub fn notify_status(status: &str) -> bool {
    notify(&format!("STATUS={status}"))
}
