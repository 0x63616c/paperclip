//! Driving the real session: systemd, the status file and `/sys/power`.
//!
//! Compiled only for Linux, and that is a refusal rather than a build
//! convenience. A macOS build of this module would compile, run, and prove
//! nothing — and the project's standing rule is that a passing local test is
//! not qualification. If it is not here, it cannot accidentally be cited as
//! evidence that the transaction works on a device.
//!
//! # Stand-down is [`StockRecovery`], not a second copy of it
//!
//! Handing the display back is `paper_host`'s recovery path, called as it
//! stands. Writing a second one here would mean two processes with two private
//! ideas of how many times `xochitl.service` has been started recently — and
//! systemd's rate limiter counts across processes. Two counters each believing
//! they are inside `StartLimitBurst=4` is how a tablet ends up on a serial
//! console with a blank screen. One budget, one policy, several callers.
//!
//! The supervisor is **stopped**, never killed, and stock is **started**,
//! never restarted. There is no method here that can do otherwise, because
//! [`Systemd`] has none.

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use paper_host::linux::recovery::{RecoveryConfig, StockRecovery};
use paper_host::linux::systemd::Systemd;
use paper_host::readiness::{self, Rung};
use paper_host::units::{HOST_UNIT, SESSION_TARGET};
use paper_protocol::ProtocolVersion;

use crate::health::{Observation, SessionControl};

/// How long the supervisor is given to exit after being asked to.
///
/// It exits at stock, which means it may be part-way through a restore when
/// the request arrives. Anything shorter than the restore's own deadline would
/// give up on a supervisor that is doing exactly what it should.
const STOP_BUDGET: Duration =
    Duration::from_secs(paper_device::stock::RESTORE_TIMEOUT.as_secs() * 2 + 10);

/// How long the supervisor is given to notice the stop request and exit on its
/// own before it is stopped.
///
/// Short: this is the difference between the tidy exit and the merely correct
/// one, not between working and not. The restore below covers either.
const SETTLE_BUDGET: Duration = Duration::from_secs(5);

/// The real session.
#[derive(Debug)]
pub struct SystemdSession {
    systemd: Systemd,
    recovery: StockRecovery,
    /// Where the supervisor's status and command files are. Under `/run`.
    state: PathBuf,
    /// Which unit is stock.
    stock_unit: String,
    /// The name Paperclip's wakelock is taken under.
    wakelock_name: String,
    /// `/sys/power/wake_lock` and `wake_unlock`.
    wake_lock: PathBuf,
    wake_unlock: PathBuf,
}

impl SystemdSession {
    /// A session described by a recovery configuration and a state directory.
    ///
    /// Both come from the supervisor's own `RuntimeConfig`, so the updater and
    /// the supervisor cannot disagree about which unit is stock or where the
    /// status file is.
    pub fn new(recovery: RecoveryConfig, state: impl Into<PathBuf>) -> Self {
        Self {
            systemd: Systemd::default(),
            state: state.into(),
            stock_unit: recovery.stock_unit.clone(),
            wakelock_name: recovery.wakelock_name.clone(),
            wake_lock: recovery.wakelock.lock.clone(),
            wake_unlock: recovery.wakelock.unlock.clone(),
            recovery: StockRecovery::new(recovery),
        }
    }

    /// The supervisor's status file.
    pub fn status_file(&self) -> PathBuf {
        self.state.join("status")
    }

    /// The file whose existence asks the supervisor to exit at stock.
    pub fn stop_file(&self) -> PathBuf {
        self.state.join("stop")
    }

    /// One field out of the status file.
    fn field(&self, status: &str, key: &str) -> Option<String> {
        status.lines().find_map(|line| {
            line.split_once('=')
                .filter(|(name, _)| name.trim() == key)
                .map(|(_, value)| value.trim().to_owned())
        })
    }
}

impl SessionControl for SystemdSession {
    fn stand_down(&self) -> Result<(), String> {
        // Ask first. A supervisor that exits on its own has already released
        // the panel, cleared its own locks and put stock back — which is the
        // path with the fewest surprises in it.
        let _ = fs::write(self.stop_file(), "upgrade\n");
        let _ = self.systemd.stop(SESSION_TARGET);
        // Give it a moment to act on the request. A supervisor that exits by
        // itself has restored stock, released the panel and cleared its own
        // locks on the way — the path with the fewest surprises in it. Going
        // straight to `systemctl stop` would race that, and win, and then the
        // cleanup would fall to the restore below rather than to the process
        // that knew what it was holding.
        let _ = wait(SETTLE_BUDGET, || !self.systemd.is_active(HOST_UNIT));
        if self.systemd.is_active(HOST_UNIT) {
            // A clean stop. Never a kill: the supervisor's `OnFailure=` starts
            // the restore unit, and a restore racing an upgrade is two things
            // trying to own the display.
            self.systemd
                .stop(HOST_UNIT)
                .map_err(|error| format!("stopping the supervisor: {error}"))?;
        }
        if !wait(STOP_BUDGET, || !self.systemd.is_active(HOST_UNIT)) {
            return Err(format!("{HOST_UNIT} is still active"));
        }
        let _ = self.systemd.reset_failed(HOST_UNIT);

        // Whatever the supervisor managed on its way out, stock is required to
        // be up before anything is swapped.
        self.recovery
            .restore()
            .map(|_| ())
            .map_err(|error| format!("restoring stock: {error}"))
    }

    fn bring_up(&self) -> Result<(), String> {
        let _ = fs::remove_file(self.stop_file());
        let _ = fs::remove_file(self.status_file());
        let _ = self.systemd.reset_failed(HOST_UNIT);
        // No `daemon-reload`: the unit file has not changed. `ExecStart=`
        // names `current/bin/paperclip-host`, and the symlink is resolved when
        // systemd execs, so the swap is already in effect.
        self.systemd
            .start(HOST_UNIT)
            .map_err(|error| format!("starting the supervisor: {error}"))
    }

    fn observe(&self) -> Observation {
        let alive = self.systemd.main_pid(HOST_UNIT).is_some()
            && (self.systemd.is_active(HOST_UNIT)
                || self
                    .systemd
                    .property(HOST_UNIT, "ActiveState")
                    .is_some_and(|state| state == "activating"));
        let Ok(status) = fs::read_to_string(self.status_file()) else {
            return Observation {
                alive,
                reached: None,
                protocol: None,
                note: String::new(),
            };
        };
        let reached = self
            .field(&status, readiness::STATUS_FIELD)
            .and_then(|value| value.parse::<Rung>().ok());
        let protocol = self
            .field(&status, "protocol")
            .and_then(|value| value.parse::<ProtocolVersion>().ok());
        Observation {
            alive,
            reached,
            protocol,
            note: self.field(&status, "ready_note").unwrap_or_default(),
        }
    }

    fn stock_is_up(&self) -> bool {
        self.systemd.is_active(&self.stock_unit)
    }

    fn wakelock_held(&self) -> bool {
        fs::read_to_string(&self.wake_lock)
            .is_ok_and(|held| held.split_whitespace().any(|tag| tag == self.wakelock_name))
    }

    fn release_wakelock(&self) {
        // By name, with no handle: the process that took it is gone, which is
        // the only situation this is called in. Best effort — a failure here
        // is reported by the caller's re-check, not by an error nobody sees.
        if let Ok(mut file) = fs::OpenOptions::new().write(true).open(&self.wake_unlock) {
            let _ = file.write_all(self.wakelock_name.as_bytes());
        }
    }
}

/// Polls `predicate` until it holds or `budget` runs out.
fn wait(budget: Duration, predicate: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if predicate() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
