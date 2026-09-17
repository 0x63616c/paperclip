//! Giving the display back to stock, without the host (§10).
//!
//! This is the path §10 calls "independent". Independent of what, precisely:
//!
//! * of the supervisor — it runs in its own process, started by systemd's
//!   `OnFailure=`, so a supervisor killed with `SIGKILL` still triggers it;
//! * of Home, the App Store, and every Paperclip IPC socket — it reads sysfs
//!   and asks systemd, and talks to nothing of ours;
//! * of the network and the Mac — it is on the device, and `paperctl stock`
//!   run over SSH is a *second* way in, not the only one.
//!
//! It is **not** independent of `platform/device`, on purpose.
//! [`paper_device::stock::Stock`] owns what may be done to Xochitl and
//! [`StartBudget`] records the starts, and that record is shared across
//! processes because systemd's rate limiter is. Two private counters would
//! each believe they were inside `StartLimitBurst=4` while together exceeding
//! it — and exceeding it drops the tablet onto a serial console with a blank
//! screen. One budget, one policy, several callers.
//!
//! What this module adds on top is the ordering, chosen against two device
//! facts:
//!
//! 1. **The advisory locks are cleared only while stock is confirmed down.**
//!    `/tmp/epframebuffer.lock` is a registry keyed on pid, and a stale entry
//!    is exactly what stops Xochitl taking the panel back. Removing one while
//!    something still holds the panel would be the opposite of helpful.
//! 2. **The wakelock is released only after stock is confirmed up.** If the
//!    restore failed the device stays awake on purpose: an awake tablet is
//!    reachable over SSH and diagnosable, and a suspended one in a failed
//!    state is a power cycle.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use paper_device::session::{DisplayLocks, EPD_LOCK, EPFRAMEBUFFER_LOCK};
use paper_device::stock::{STOCK_UNIT, StartBudget, Stock};

use crate::linux::process::{self, SessionTree};
use crate::linux::systemd::Systemd;
use crate::linux::unit::UnitControl;
use crate::state::Budget;

/// Substring `journalctl` shows when Xochitl comes up unable to own the
/// panel. The exact failure `stock` reported success against in WWW-35:
/// `systemctl` said `active`, the vendor lock file had been handed back, and
/// the panel stayed white because both of those checks are blind to a stray
/// file descriptor. Recorded verbatim from the incident's journal.
const ATOMIC_COMMIT_FAILURE: &str = "Atomic commit failed";

/// Where the advisory display-ownership locks are.
///
/// Paths rather than the constants because the VM harness has no `/tmp/epd.lock`
/// it may disturb, and because a test that writes to the real ones would be
/// interfering with whatever is on the panel.
#[derive(Debug, Clone)]
pub struct LockPaths {
    /// `/tmp/epd.lock`.
    pub epd: PathBuf,
    /// `/tmp/epframebuffer.lock`.
    pub registry: PathBuf,
}

impl Default for LockPaths {
    fn default() -> Self {
        Self {
            epd: PathBuf::from(EPD_LOCK),
            registry: PathBuf::from(EPFRAMEBUFFER_LOCK),
        }
    }
}

impl LockPaths {
    /// The same two files under `prefix`, for the harness.
    pub fn under(prefix: &Path) -> Self {
        Self {
            epd: prefix.join("epd.lock"),
            registry: prefix.join("epframebuffer.lock"),
        }
    }
}

/// Where the kernel wakelock files are.
#[derive(Debug, Clone)]
pub struct WakeLockPaths {
    /// `/sys/power/wake_lock`.
    pub lock: PathBuf,
    /// `/sys/power/wake_unlock`.
    pub unlock: PathBuf,
}

impl Default for WakeLockPaths {
    fn default() -> Self {
        Self {
            lock: PathBuf::from(paper_device::session::WAKE_LOCK),
            unlock: PathBuf::from(paper_device::session::WAKE_UNLOCK),
        }
    }
}

impl WakeLockPaths {
    /// The same two files under `prefix`, for the harness, which has no
    /// `/sys/power` of its own.
    pub fn under(prefix: &Path) -> Self {
        Self {
            lock: prefix.join("wake_lock"),
            unlock: prefix.join("wake_unlock"),
        }
    }
}

/// Everything the recovery path needs, all of it overridable so the VM
/// harness can exercise the real code against stand-in files.
#[derive(Debug, Clone)]
pub struct RecoveryConfig {
    /// Which unit counts as stock.
    pub stock_unit: String,
    /// Where the shared start record lives. Shared with `platform/device`'s
    /// takeover, deliberately.
    pub start_budget: PathBuf,
    /// Where the wakelock files are.
    pub wakelock: WakeLockPaths,
    /// The name Paperclip's wakelock is taken under.
    pub wakelock_name: String,
    /// The advisory display locks.
    pub locks: LockPaths,
    /// Where to write the diagnostics bundle.
    pub diagnostics: PathBuf,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            stock_unit: STOCK_UNIT.to_owned(),
            start_budget: PathBuf::from("/tmp/paperclip-xochitl-starts"),
            wakelock: WakeLockPaths::default(),
            // Must be the tag a takeover actually acquires under
            // (`paper_device::takeover::WAKELOCK_TAG`), not a name that
            // merely sounds right. WWW-35: this used to be `"paperclip"`,
            // which released nothing — the wakelock a takeover holds is
            // `"paperclip-takeover"` — so `stock` reported success while
            // leaving the tablet unable to sleep.
            wakelock_name: paper_device::takeover::WAKELOCK_TAG.to_owned(),
            locks: LockPaths::default(),
            diagnostics: PathBuf::from("/run/paperclip/diagnostics"),
        }
    }
}

/// What a restore achieved. Every variant is an observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restored {
    /// Stock was already active; nothing was done to it.
    AlreadyRunning,
    /// Stock was started and observed active.
    Started,
}

impl Restored {
    /// One line for a human.
    pub fn summary(&self) -> String {
        match self {
            Restored::AlreadyRunning => "stock Xochitl was already running".to_owned(),
            Restored::Started => "stock Xochitl restored".to_owned(),
        }
    }
}

/// Why a restore did not succeed.
///
/// Never reported as a partial success. §10 is explicit: never claim recovery
/// succeeded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecoveryError {
    /// `platform/device`'s stock policy refused, or ran out of guarded starts.
    #[error("stock could not be brought back")]
    Stock(#[from] paper_device::error::DeviceError),
    /// Xochitl is `active`, but its own journal says it never got the panel.
    /// Reported as a failure precisely because every cheaper check — unit
    /// state, the vendor lock file — would call this session recovered.
    #[error(
        "xochitl.service is active but its journal shows '{ATOMIC_COMMIT_FAILURE}'; it does \
         not own the display. Something still holds DRM master."
    )]
    DisplayNotOwned,
}

/// The independent recovery path.
#[derive(Debug)]
pub struct StockRecovery {
    config: RecoveryConfig,
    systemd: Systemd,
}

impl StockRecovery {
    /// Builds a recovery over `config`.
    pub fn new(config: RecoveryConfig) -> Self {
        let systemd = Systemd::default();
        Self { config, systemd }
    }

    /// The configuration in force, for diagnostics and tests.
    pub fn config(&self) -> &RecoveryConfig {
        &self.config
    }

    /// Hands the display back to stock.
    ///
    /// # Errors
    ///
    /// Any [`RecoveryError`]. On every error path the wakelock is *kept*, on
    /// purpose, so the tablet stays awake and reachable rather than suspending
    /// into a state nobody can diagnose.
    pub fn restore(&self) -> Result<Restored, RecoveryError> {
        let mut control = UnitControl::new(&self.config.stock_unit);
        let mut stock = Stock::new(control.clone(), StartBudget::at(&self.config.start_budget));

        if <UnitControl as paper_device::stock::ServiceControl>::is_active(&mut control)? {
            // Stock is already up, so the panel is registered to it and the
            // locks are its own. Touching them here would be a bug.
            self.release_wakelock();
            return Ok(Restored::AlreadyRunning);
        }

        // Stock being down does not mean the registry describes nobody: an
        // interrupted `paperctl open --hold` (WWW-35) can outlive the SSH
        // session that started it and still hold DRM master. Reap whatever
        // is still holding the device *before* trusting the registry as
        // stale — clearing it out from under a live holder would just hide
        // the conflict `stock` is about to run into.
        self.reap_stray_drm_holders();

        // Now the registry describes a process that is actually gone. This
        // is the only moment it is safe to clear it.
        self.clear_stale_locks();

        // `platform/device` owns what happens next: reset-failed, record the
        // attempt in the shared budget, start, wait, one guarded retry, then
        // stop and say to power-cycle. None of that policy is re-implemented
        // here.
        stock.restore(SystemTime::now())?;

        // `systemctl` saying `active` is not the same claim as "owns the
        // panel" (WWW-35): a start that raced a holder we failed to reap
        // still comes up active and fails every atomic commit. Read the
        // journal rather than trust the restart alone.
        if self
            .systemd
            .journal(&self.config.stock_unit, 50)
            .contains(ATOMIC_COMMIT_FAILURE)
        {
            // Wakelock stays held, on purpose, same as any other failure
            // path here: an awake tablet is a diagnosable one.
            return Err(RecoveryError::DisplayNotOwned);
        }

        // Only now: a suspend between the start and stock actually owning the
        // panel is the race this ordering exists to avoid.
        self.release_wakelock();
        Ok(Restored::Started)
    }

    /// Terminates any process still holding a DRM device, bounded and
    /// best-effort.
    ///
    /// A failure to fully empty the set is not raised here: the journal
    /// check right after `stock.restore` is what actually decides whether
    /// the recovery worked, and it decides correctly whether or not this
    /// reaped anything.
    fn reap_stray_drm_holders(&self) {
        let Ok(holders) = process::drm_holders(Path::new("/proc")) else {
            return;
        };
        if holders.is_empty() {
            return;
        }
        let _ = SessionTree::of(holders).terminate(Budget::default().escalation());
    }

    /// Writes everything worth keeping about a failure, before anything
    /// overwrites it.
    ///
    /// Returns the directory it wrote, so a caller can print it. Best effort
    /// by design: a diagnostics failure must never turn a recoverable state
    /// into an unrecoverable one.
    pub fn capture(&self, reason: &str) -> PathBuf {
        let dir = &self.config.diagnostics;
        let _ = fs::create_dir_all(dir);
        let unit = &self.config.stock_unit;
        let mut bundle = String::new();
        bundle.push_str("# Paperclip recovery diagnostics\n\n");
        bundle.push_str(&format!("reason: {reason}\n"));
        bundle.push_str(&format!("stock unit: {unit}\n"));
        for property in ["ActiveState", "Result", "NRestarts"] {
            bundle.push_str(&format!(
                "stock {property}: {}\n",
                self.systemd
                    .property(unit, property)
                    .unwrap_or_else(|| "unknown".to_owned())
            ));
        }
        bundle.push_str(&format!(
            "starts recorded in the shared budget: {}\n",
            StartBudget::at(&self.config.start_budget).recent(SystemTime::now())
        ));
        bundle.push_str(&format!(
            "wakelock `{}` active: {}\n",
            self.config.wakelock_name,
            fs::read_to_string(&self.config.wakelock.lock)
                .map(|raw| raw
                    .split_whitespace()
                    .any(|tag| tag == self.config.wakelock_name))
                .unwrap_or(false)
        ));
        bundle.push_str(&format!(
            "display registry: {}\n",
            match DisplayLocks::inspect_at(&self.config.locks.epd, &self.config.locks.registry) {
                Ok(locks) => locks
                    .holder
                    .map_or_else(|| "unheld".to_owned(), |holder| holder.describe()),
                Err(error) => format!("unreadable: {error}"),
            }
        ));
        bundle.push_str("\n## systemctl status\n\n");
        bundle.push_str(&self.systemd.status_text(unit));
        bundle.push_str("\n\n## journal\n\n");
        bundle.push_str(&self.systemd.journal(unit, 200));
        // Named without a timestamp so the *latest* failure is always at a
        // path a person can be told over the phone.
        let _ = fs::write(dir.join("last-failure.txt"), bundle);
        dir.clone()
    }

    /// Removes the advisory locks left behind by a process that is gone.
    ///
    /// Only ever called with stock confirmed down.
    fn clear_stale_locks(&self) {
        for lock in [&self.config.locks.registry, &self.config.locks.epd] {
            let _ = fs::remove_file(lock);
        }
    }

    /// Releases the wakelock by name, with no handle on it.
    ///
    /// `paper_device::WakeLock` deliberately models a lock this process holds
    /// and releases on `Drop`. Recovery is the other case: the process that
    /// took the lock is gone and left no value behind, which is exactly the
    /// situation §10 says a destructor cannot cover. The kernel interface is a
    /// name, so releasing someone else's is a single write — and releasing a
    /// lock that is not held is a harmless `EINVAL`.
    fn release_wakelock(&self) {
        use std::io::Write as _;
        let Ok(mut file) = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.config.wakelock.unlock)
        else {
            return;
        };
        let _ = file.write_all(self.config.wakelock_name.as_bytes());
    }
}
