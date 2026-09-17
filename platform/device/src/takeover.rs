//! Taking the display from stock, and giving it back.
//!
//! One type, [`Takeover`], which owns the ordering. The ordering is the whole
//! of the safety argument, so it lives in one place rather than in every
//! caller:
//!
//! | | Acquire | Release |
//! |---|---|---|
//! | 1 | arm the watchdog | clear the panel (the caller, before dropping it) |
//! | 2 | record the vendor lock state | **put the vendor locks back** |
//! | 3 | check the start budget | wait until the panel is genuinely free |
//! | 4 | take the wakelock | restore stock |
//! | 5 | stop stock cleanly | release the wakelock |
//!
//! Step 2 of the release is not optional and its absence is not survivable.
//! Leaving our PID in `/tmp/epframebuffer.lock` made Xochitl abort twice on
//! restart — see [`VendorLockState`] for the journal. It has to happen
//! *before* stock is started, because a restarting Xochitl reads that file
//! immediately.
//!
//! Budget first, because a refusal must leave the tablet untouched. Wakelock
//! before the stop, because the window where stock is down and nothing holds a
//! wakelock is the window a suspend resumes into a second Xochitl.
//!
//! ## Release order, and a discrepancy worth naming
//!
//! The `remarkable-device-session` skill lists releasing the wakelock *before*
//! starting Xochitl; `tools/device-probe/lib-stock.sh` starts Xochitl first and
//! releases after. This follows the script. A suspend in the gap is harmless
//! once stock is running and harmful while it is not, so "stock back, then stop
//! inhibiting sleep" is the order that is never wrong. Flagged rather than
//! quietly chosen.
//!
//! ## What guarantees the restore
//!
//! Three layers, because the first two cannot cover everything:
//!
//! 1. [`Drop`], which runs on the ordinary path and on a panic unwind.
//! 2. An interrupt flag set from a signal handler, which the session loop polls
//!    so `SIGINT`/`SIGTERM`/`SIGHUP` return through `Drop` rather than past it.
//! 3. A detached [`watchdog`](Takeover::arm_watchdog) that restores stock on its
//!    own schedule. This is the only layer that survives `SIGKILL`, a segfault,
//!    or the control channel dropping mid-session — which is exactly what the
//!    tablet's own autosleep does to an SSH connection.
//!
//! `Drop` cannot report a failed restore, so call [`Takeover::release`]
//! explicitly on the normal path and let the destructor be the net.

use std::fmt;
use std::time::{Duration, Instant, SystemTime};

use std::thread;

use crate::error::DeviceError;
use crate::session::{VendorLockState, WakeLock};
use crate::stock::{ServiceControl, Stock};

/// The wakelock tag a takeover holds, matching `lib-stock.sh`.
pub const WAKELOCK_TAG: &str = "paperclip-takeover";

/// How long a session may run before the watchdog restores stock regardless.
///
/// Generous, because the watchdog firing during healthy work would itself be a
/// hazard — it would start Xochitl underneath a running session, which is two
/// processes contending for the panel.
pub const DEFAULT_WATCHDOG: Duration = Duration::from_secs(300);

/// How long to wait for the panel to look unclaimed before starting stock
/// anyway and reporting that it was not confirmed.
pub const PANEL_FREE_TIMEOUT: Duration = Duration::from_secs(5);

/// Something that can arm an out-of-process restore.
///
/// A trait so the tests can observe that a watchdog was armed and disarmed
/// without spawning anything.
pub trait Watchdog: fmt::Debug {
    /// Arms a restore that must happen `within` from now, whatever becomes of
    /// this process.
    fn arm(&mut self, within: Duration) -> Result<(), DeviceError>;
    /// Cancels it, because the session released stock itself.
    fn disarm(&mut self) -> Result<(), DeviceError>;
}

/// A watchdog that does nothing, for a dry run on the Mac.
#[derive(Debug, Default)]
pub struct NoWatchdog;

impl Watchdog for NoWatchdog {
    fn arm(&mut self, _within: Duration) -> Result<(), DeviceError> {
        Ok(())
    }
    fn disarm(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}

/// A held takeover: stock is down, the wakelock is held, the panel is free.
///
/// Dropping this restores stock. That is the point of it.
#[derive(Debug)]
pub struct Takeover<S: ServiceControl, W: Watchdog> {
    stock: Stock<S>,
    wake: Option<WakeLock>,
    watchdog: W,
    locks: VendorLockState,
    released: bool,
}

impl<S: ServiceControl, W: Watchdog> Takeover<S, W> {
    /// Takes the display, in the order above.
    ///
    /// `wake` is the already-acquired wakelock. It is taken as an argument
    /// rather than acquired here so that a caller which cannot get one has to
    /// decide what to do about it explicitly — on this device, running without
    /// one is a correctness bug, not a degraded mode.
    pub fn acquire(
        mut stock: Stock<S>,
        wake: WakeLock,
        mut watchdog: W,
        locks: VendorLockState,
        budget: Duration,
        now: SystemTime,
    ) -> Result<Self, (DeviceError, WakeLock)> {
        // The watchdog is armed before the stop, so that even a failure inside
        // `stop_for_session` is covered by something outside this process.
        if let Err(error) = watchdog.arm(budget) {
            return Err((error, wake));
        }
        if let Err(error) = stock.stop_for_session(now) {
            let _ = watchdog.disarm();
            return Err((error, wake));
        }
        Ok(Self {
            stock,
            wake: Some(wake),
            watchdog,
            locks,
            released: false,
        })
    }

    /// Whether stock is currently stopped by this session.
    pub fn holds_display(&self) -> bool {
        !self.released && self.stock.owes_restore()
    }

    /// Access to stock, for reading health mid-session.
    pub fn stock_mut(&mut self) -> &mut Stock<S> {
        &mut self.stock
    }

    /// Polls until the vendor registry no longer names a holder we introduced.
    ///
    /// Bounded, and returns whether it got there rather than failing: the
    /// caller still has to bring stock back either way, and a tablet with no
    /// Xochitl is worse than one that had to retry.
    fn wait_for_free_panel(&self) -> bool {
        let deadline = Instant::now() + PANEL_FREE_TIMEOUT;
        loop {
            if self.locks.is_clear() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Gives the display back and reports whether it worked.
    ///
    /// Call this. [`Drop`] does the same thing but has nowhere to put an
    /// error, and "stock did not come back" is the one failure nobody may
    /// discover later.
    pub fn release(mut self, now: SystemTime) -> Result<(), DeviceError> {
        self.release_inner(now)
    }

    fn release_inner(&mut self, now: SystemTime) -> Result<(), DeviceError> {
        if self.released {
            return Ok(());
        }
        self.released = true;

        // The locks go back BEFORE stock is started. A restarting Xochitl reads
        // /tmp/epframebuffer.lock immediately; finding our PID there — from a
        // process that is necessarily still alive, because it is this one — is
        // what made it abort twice. Nothing about "wait for our process to
        // exit" can fix that, since the release path cannot exit before it
        // starts stock. Putting the file back is the whole fix.
        let locks_restored = self.locks.restore();

        // Then wait for the panel to actually look free, bounded. A fixed sleep
        // would be either too short on a slow release or wasted time on a fast
        // one, and this has a real answer to poll for.
        let freed = if locks_restored.is_ok() {
            self.wait_for_free_panel()
        } else {
            false
        };

        // Only now is it safe to bring stock back.
        let restored = if freed {
            self.stock.restore(now)
        } else {
            // Start anyway — a tablet with no Xochitl is worse than one that
            // may have to retry — but never claim this went well.
            let attempted = self.stock.restore(now);
            let reason = locks_restored
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "the vendor lock still names a foreign holder".to_owned());
            return match attempted {
                Ok(()) => Err(DeviceError::unexpected(format!(
                    "stock was started, but the panel was not confirmed free first: {reason}. \
                     Check `systemctl show -p NRestarts xochitl.service` before taking the \
                     display again."
                ))),
                Err(error) => Err(error),
            };
        };

        // The wakelock is released whatever the restore did. Leaking one blocks
        // suspend until the next reboot and drains the battery silently, and a
        // failed restore is not made better by also leaving that behind.
        drop(self.wake.take());

        // Only disarm once stock is genuinely back. If the restore failed, the
        // watchdog is the remaining hope and must be left armed.
        if restored.is_ok() {
            let _ = self.watchdog.disarm();
        }
        restored
    }
}

impl<S: ServiceControl, W: Watchdog> Drop for Takeover<S, W> {
    fn drop(&mut self) {
        // Runs on the ordinary path, and on a panic unwind. A restore failure
        // here has nowhere to go; the watchdog stays armed precisely because
        // this path cannot report.
        let _ = self.release_inner(SystemTime::now());
    }
}

#[cfg(target_os = "linux")]
pub use linux::DetachedWatchdog;

#[cfg(target_os = "linux")]
mod linux {
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use super::{WAKELOCK_TAG, Watchdog};
    use crate::error::DeviceError;
    use crate::session::WAKE_UNLOCK;
    use crate::stock::STOCK_UNIT;

    /// A restore that happens whatever becomes of this process.
    ///
    /// The only layer that covers `SIGKILL`, a segfault, an OOM kill, or the
    /// control channel going away — and the last is not hypothetical: the
    /// tablet autosleeps on its own schedule and takes SSH with it, which is
    /// how a session ends up orphaned with stock still stopped.
    ///
    /// Deliberately a detached `sh`, not a thread: a thread dies with the
    /// process it is trying to outlive. `setsid` so it survives the session
    /// leader, and the sleep is its whole schedule so there is nothing to go
    /// wrong in it.
    #[derive(Debug, Default)]
    pub struct DetachedWatchdog {
        guardian: Option<Child>,
    }

    impl DetachedWatchdog {
        /// A watchdog that has not been armed.
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl Watchdog for DetachedWatchdog {
        fn arm(&mut self, within: Duration) -> Result<(), DeviceError> {
            self.disarm()?;
            // `reset-failed` before the start for the same reason it is there
            // everywhere else: a start refused by the rate limiter fails the
            // unit and drops the tablet to an emergency shell. The wakelock
            // release is last and unconditional — a leaked one blocks suspend
            // until the next reboot.
            let script = format!(
                "sleep {}; systemctl reset-failed {STOCK_UNIT} 2>/dev/null; \
                 systemctl is-active --quiet {STOCK_UNIT} || systemctl start {STOCK_UNIT}; \
                 echo {WAKELOCK_TAG} > {WAKE_UNLOCK} 2>/dev/null",
                within.as_secs()
            );
            let guardian = Command::new("setsid")
                .args(["sh", "-c", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|source| DeviceError::io("spawn a watchdog for", STOCK_UNIT, source))?;
            self.guardian = Some(guardian);
            Ok(())
        }

        fn disarm(&mut self) -> Result<(), DeviceError> {
            let Some(mut guardian) = self.guardian.take() else {
                return Ok(());
            };
            // Only ever this exact child, by the handle we hold — never by name.
            let _ = guardian.kill();
            let _ = guardian.wait();
            Ok(())
        }
    }

    impl Drop for DetachedWatchdog {
        fn drop(&mut self) {
            // A watchdog that outlives its session would start Xochitl
            // underneath the next one.
            let _ = self.disarm();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_WATCHDOG, NoWatchdog, Takeover, Watchdog};
    use crate::error::DeviceError;
    use crate::session::{VendorLockState, WakeLock};
    use crate::stock::{ServiceControl, StartBudget, Stock};
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime};

    /// Records every systemd call in order, and can be told to misbehave.
    #[derive(Debug, Default)]
    struct FakeService {
        log: Rc<RefCell<Vec<String>>>,
        active: bool,
        /// Starts that silently do not bring the unit up.
        failed_starts_remaining: u32,
        stop_fails: bool,
    }

    impl FakeService {
        fn new(log: &Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                log: Rc::clone(log),
                active: true,
                ..Self::default()
            }
        }
        fn note(&self, what: &str) {
            self.log.borrow_mut().push(what.to_owned());
        }
    }

    impl ServiceControl for FakeService {
        fn is_active(&mut self) -> Result<bool, DeviceError> {
            Ok(self.active)
        }
        fn stop(&mut self) -> Result<(), DeviceError> {
            self.note("stop");
            if self.stop_fails {
                return Err(DeviceError::unexpected("stop refused"));
            }
            self.active = false;
            Ok(())
        }
        fn reset_failed(&mut self) -> Result<(), DeviceError> {
            self.note("reset-failed");
            Ok(())
        }
        fn start(&mut self) -> Result<(), DeviceError> {
            self.note("start");
            if self.failed_starts_remaining > 0 {
                self.failed_starts_remaining -= 1;
            } else {
                self.active = true;
            }
            Ok(())
        }
        fn wait_active(&mut self, _timeout: Duration) -> Result<bool, DeviceError> {
            Ok(self.active)
        }
    }

    #[derive(Debug, Default)]
    struct FakeWatchdog {
        log: Rc<RefCell<Vec<String>>>,
        armed: bool,
    }

    impl FakeWatchdog {
        fn new(log: &Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                log: Rc::clone(log),
                armed: false,
            }
        }
    }

    impl Watchdog for FakeWatchdog {
        fn arm(&mut self, _within: Duration) -> Result<(), DeviceError> {
            self.log.borrow_mut().push("arm".to_owned());
            self.armed = true;
            Ok(())
        }
        fn disarm(&mut self) -> Result<(), DeviceError> {
            self.log.borrow_mut().push("disarm".to_owned());
            self.armed = false;
            Ok(())
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("paper-takeover-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("creates a scratch directory");
        dir
    }

    fn wakelock(dir: &std::path::Path) -> WakeLock {
        WakeLock::acquire_at(
            "paperclip-test",
            &dir.join("wake_lock"),
            &dir.join("wake_unlock"),
        )
        .expect("takes the lock")
    }

    fn budget(dir: &std::path::Path) -> StartBudget {
        StartBudget::at(&dir.join("starts"))
    }

    /// Lock state over scratch files. `registry` is what the vendor registry
    /// held before the takeover, mirroring `/tmp/epframebuffer.lock`.
    fn locks(dir: &std::path::Path, registry: Option<&str>) -> VendorLockState {
        let registry_path = dir.join("epframebuffer.lock");
        let epd_path = dir.join("epd.lock");
        fs::write(&epd_path, "").expect("creates the epd lock");
        match registry {
            Some(contents) => fs::write(&registry_path, contents).expect("seeds the registry"),
            None => {
                let _ = fs::remove_file(&registry_path);
            }
        }
        VendorLockState::capture_at(&registry_path, &epd_path).expect("captures")
    }

    /// What a takeover does to the registry: claims it with its own PID.
    fn claim(dir: &std::path::Path, pid: u32) {
        fs::write(
            dir.join("epframebuffer.lock"),
            format!("{pid}\npaperclip\nimx8mm-ferrari\nmachine\nboot\n"),
        )
        .expect("claims the registry");
    }

    #[test]
    fn a_session_stops_stock_and_restores_it_in_the_right_order() {
        let dir = scratch("order");
        let log = Rc::new(RefCell::new(Vec::new()));
        let stock = Stock::new(FakeService::new(&log), budget(&dir));

        let session = Takeover::acquire(
            stock,
            wakelock(&dir),
            FakeWatchdog::new(&log),
            locks(&dir, Some("before\n")),
            DEFAULT_WATCHDOG,
            SystemTime::now(),
        )
        .map_err(|(error, _)| error)
        .expect("takes the display");
        assert!(session.holds_display());

        session.release(SystemTime::now()).expect("restores stock");

        assert_eq!(
            log.borrow().as_slice(),
            // Armed before the stop; reset-failed before every start; disarmed
            // only once stock is genuinely back.
            ["arm", "stop", "reset-failed", "start", "disarm"]
        );
        // And the wakelock was released.
        assert_eq!(
            fs::read_to_string(dir.join("wake_unlock")).expect("released"),
            "paperclip-test"
        );
    }

    #[test]
    fn dropping_a_session_restores_stock_even_though_it_cannot_report() {
        let dir = scratch("drop");
        let log = Rc::new(RefCell::new(Vec::new()));
        let stock = Stock::new(FakeService::new(&log), budget(&dir));

        {
            let _session = Takeover::acquire(
                stock,
                wakelock(&dir),
                FakeWatchdog::new(&log),
                locks(&dir, Some("before\n")),
                DEFAULT_WATCHDOG,
                SystemTime::now(),
            )
            .map_err(|(error, _)| error)
            .expect("takes the display");
        }

        assert!(
            log.borrow().contains(&"start".to_owned()),
            "{:?}",
            log.borrow()
        );
        assert!(fs::read_to_string(dir.join("wake_unlock")).is_ok());
    }

    #[test]
    fn a_panic_mid_session_still_gives_the_display_back() {
        // The property that matters most: a bug in screen code must not strand
        // the tablet. Unwinding runs Drop, and Drop restores.
        let dir = scratch("panic");
        let log = Rc::new(RefCell::new(Vec::new()));
        let stock = Stock::new(FakeService::new(&log), budget(&dir));
        let inner = Rc::clone(&log);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _session = Takeover::acquire(
                stock,
                wakelock(&dir),
                FakeWatchdog::new(&inner),
                locks(&dir, Some("before\n")),
                DEFAULT_WATCHDOG,
                SystemTime::now(),
            )
            .map_err(|(error, _)| error)
            .expect("takes the display");
            panic!("a screen did something stupid");
        }));

        assert!(result.is_err(), "the panic propagated");
        assert_eq!(
            log.borrow().as_slice(),
            ["arm", "stop", "reset-failed", "start", "disarm"]
        );
    }

    #[test]
    fn a_spent_start_budget_refuses_before_touching_anything() {
        let dir = scratch("budget");
        let log = Rc::new(RefCell::new(Vec::new()));
        let starts = budget(&dir);
        let now = SystemTime::now();
        for _ in 0..3 {
            starts.record(now).expect("records");
        }

        let stock = Stock::new(FakeService::new(&log), starts);
        let (error, _wake) = Takeover::acquire(
            stock,
            wakelock(&dir),
            FakeWatchdog::new(&log),
            locks(&dir, Some("before\n")),
            DEFAULT_WATCHDOG,
            now,
        )
        .expect_err("refuses");

        assert!(error.to_string().contains("emergency shell"), "{error}");
        // Armed, then disarmed. Stock was never stopped.
        assert_eq!(log.borrow().as_slice(), ["arm", "disarm"]);
    }

    #[test]
    fn a_failed_stop_leaves_stock_running_and_returns_the_wakelock() {
        let dir = scratch("stopfail");
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut service = FakeService::new(&log);
        service.stop_fails = true;

        let (error, wake) = Takeover::acquire(
            Stock::new(service, budget(&dir)),
            wakelock(&dir),
            FakeWatchdog::new(&log),
            locks(&dir, Some("before\n")),
            DEFAULT_WATCHDOG,
            SystemTime::now(),
        )
        .expect_err("refuses");

        assert!(error.to_string().contains("stop refused"), "{error}");
        // The caller gets the wakelock back rather than it being dropped inside
        // a failed constructor, so releasing it stays their decision.
        assert_eq!(wake.tag(), "paperclip-test");
        assert_eq!(log.borrow().as_slice(), ["arm", "stop", "disarm"]);
    }

    #[test]
    fn one_failed_start_is_retried_exactly_once() {
        let dir = scratch("retry");
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut service = FakeService::new(&log);
        service.failed_starts_remaining = 1;

        let session = Takeover::acquire(
            Stock::new(service, budget(&dir)),
            wakelock(&dir),
            FakeWatchdog::new(&log),
            locks(&dir, Some("before\n")),
            DEFAULT_WATCHDOG,
            SystemTime::now(),
        )
        .map_err(|(error, _)| error)
        .expect("takes the display");
        session
            .release(SystemTime::now())
            .expect("restores on the retry");

        assert_eq!(
            log.borrow().as_slice(),
            [
                "arm",
                "stop",
                "reset-failed",
                "start",
                "reset-failed",
                "start",
                "disarm"
            ]
        );
    }

    #[test]
    fn a_restore_that_never_works_reports_it_and_leaves_the_watchdog_armed() {
        let dir = scratch("hopeless");
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut service = FakeService::new(&log);
        service.failed_starts_remaining = 99;

        let session = Takeover::acquire(
            Stock::new(service, budget(&dir)),
            wakelock(&dir),
            FakeWatchdog::new(&log),
            locks(&dir, Some("before\n")),
            DEFAULT_WATCHDOG,
            SystemTime::now(),
        )
        .map_err(|(error, _)| error)
        .expect("takes the display");

        let error = session
            .release(SystemTime::now())
            .expect_err("reports failure");
        assert!(error.to_string().contains("Power-cycle"), "{error}");
        // Exactly two starts, and the watchdog is NOT disarmed — it is the only
        // thing left that might rescue the tablet.
        let log = log.borrow();
        assert_eq!(log.iter().filter(|entry| *entry == "start").count(), 2);
        assert!(!log.contains(&"disarm".to_owned()), "{log:?}");
        // The wakelock is still released: leaking one drains the battery.
        assert!(fs::read_to_string(dir.join("wake_unlock")).is_ok());
    }

    #[test]
    fn the_vendor_lock_is_put_back_before_stock_is_started() {
        // The regression test for the 04:05 incident. Xochitl aborted twice
        // because the registry still held our PID when it restarted, so the
        // ordering here is the assertion: the registry must read what it read
        // before the takeover *by the time* `start` happens.
        let dir = scratch("lockorder");
        let log = Rc::new(RefCell::new(Vec::new()));
        let registry = dir.join("epframebuffer.lock");
        let state = locks(
            &dir,
            Some("35366\nxochitl\nimx8mm-ferrari\nmachine\nboot\n"),
        );

        // A service whose `start` records what the registry held at that moment.
        #[derive(Debug)]
        struct WatchingService {
            inner: FakeService,
            registry: PathBuf,
            seen_at_start: Rc<RefCell<Option<String>>>,
        }
        impl ServiceControl for WatchingService {
            fn is_active(&mut self) -> Result<bool, DeviceError> {
                self.inner.is_active()
            }
            fn stop(&mut self) -> Result<(), DeviceError> {
                self.inner.stop()
            }
            fn reset_failed(&mut self) -> Result<(), DeviceError> {
                self.inner.reset_failed()
            }
            fn start(&mut self) -> Result<(), DeviceError> {
                *self.seen_at_start.borrow_mut() = fs::read_to_string(&self.registry).ok();
                self.inner.start()
            }
            fn wait_active(&mut self, timeout: Duration) -> Result<bool, DeviceError> {
                self.inner.wait_active(timeout)
            }
        }

        let seen = Rc::new(RefCell::new(None));
        let service = WatchingService {
            inner: FakeService::new(&log),
            registry: registry.clone(),
            seen_at_start: Rc::clone(&seen),
        };

        let session = Takeover::acquire(
            Stock::new(service, budget(&dir)),
            wakelock(&dir),
            FakeWatchdog::new(&log),
            state,
            DEFAULT_WATCHDOG,
            SystemTime::now(),
        )
        .map_err(|(error, _)| error)
        .expect("takes the display");

        // The vendor engine claims the panel, exactly as it does on device.
        claim(&dir, std::process::id());
        assert_ne!(
            fs::read_to_string(&registry).expect("claimed"),
            "35366\nxochitl\nimx8mm-ferrari\nmachine\nboot\n"
        );

        session.release(SystemTime::now()).expect("restores");

        assert_eq!(
            seen.borrow().as_deref(),
            Some("35366\nxochitl\nimx8mm-ferrari\nmachine\nboot\n"),
            "xochitl must not see our PID in the registry when it starts"
        );
    }

    #[test]
    fn a_registry_that_was_absent_before_is_removed_rather_than_left_behind() {
        let dir = scratch("lockabsent");
        let log = Rc::new(RefCell::new(Vec::new()));
        let registry = dir.join("epframebuffer.lock");
        let state = locks(&dir, None);

        let session = Takeover::acquire(
            Stock::new(FakeService::new(&log), budget(&dir)),
            wakelock(&dir),
            FakeWatchdog::new(&log),
            state,
            DEFAULT_WATCHDOG,
            SystemTime::now(),
        )
        .map_err(|(error, _)| error)
        .expect("takes the display");

        claim(&dir, std::process::id());
        session.release(SystemTime::now()).expect("restores");

        assert!(
            !registry.exists(),
            "a lock we introduced must not survive us"
        );
    }

    #[test]
    fn releasing_twice_is_harmless() {
        let dir = scratch("twice");
        let log = Rc::new(RefCell::new(Vec::new()));
        let session = Takeover::acquire(
            Stock::new(FakeService::new(&log), budget(&dir)),
            wakelock(&dir),
            NoWatchdog,
            locks(&dir, Some("before\n")),
            DEFAULT_WATCHDOG,
            SystemTime::now(),
        )
        .map_err(|(error, _)| error)
        .expect("takes the display");

        session.release(SystemTime::now()).expect("restores");
        // The value was consumed, so its Drop ran too. One start, not two.
        assert_eq!(log.borrow().iter().filter(|e| *e == "start").count(), 1);
    }

    #[test]
    fn a_session_that_finds_stock_already_down_does_not_claim_the_restore() {
        let dir = scratch("alreadydown");
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut service = FakeService::new(&log);
        service.active = false;

        let session = Takeover::acquire(
            Stock::new(service, budget(&dir)),
            wakelock(&dir),
            FakeWatchdog::new(&log),
            locks(&dir, Some("before\n")),
            DEFAULT_WATCHDOG,
            SystemTime::now(),
        )
        .map_err(|(error, _)| error)
        .expect("proceeds");

        assert!(!session.holds_display(), "someone else stopped it");
        // But it still brings stock back on release — leaving the tablet dark
        // because another session stopped it would be the same outcome for the
        // person holding it.
        session.release(SystemTime::now()).expect("restores");
        assert!(log.borrow().contains(&"start".to_owned()));
    }
}
