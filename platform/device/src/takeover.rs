//! Taking the display from stock, and giving it back.
//!
//! One type, [`Takeover`], which owns the ordering. The ordering is the whole
//! of the safety argument, so it lives in one place rather than in every
//! caller:
//!
//! | | Acquire | Release |
//! |---|---|---|
//! | 1 | check the start budget | clear the panel (the caller, before dropping it) |
//! | 2 | take the wakelock | restore stock |
//! | 3 | stop stock cleanly | release the wakelock |
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
use std::time::{Duration, SystemTime};

use crate::error::DeviceError;
use crate::session::WakeLock;
use crate::stock::{ServiceControl, Stock};

/// The wakelock tag a takeover holds, matching `lib-stock.sh`.
pub const WAKELOCK_TAG: &str = "paperclip-takeover";

/// How long a session may run before the watchdog restores stock regardless.
///
/// Generous, because the watchdog firing during healthy work would itself be a
/// hazard — it would start Xochitl underneath a running session, which is two
/// processes contending for the panel.
pub const DEFAULT_WATCHDOG: Duration = Duration::from_secs(300);

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

        // Stock first, then the wakelock: a suspend is harmless once Xochitl is
        // running and harmful while it is not.
        let restored = self.stock.restore(now);

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
    use crate::session::WakeLock;
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

    #[test]
    fn a_session_stops_stock_and_restores_it_in_the_right_order() {
        let dir = scratch("order");
        let log = Rc::new(RefCell::new(Vec::new()));
        let stock = Stock::new(FakeService::new(&log), budget(&dir));

        let session = Takeover::acquire(
            stock,
            wakelock(&dir),
            FakeWatchdog::new(&log),
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
    fn releasing_twice_is_harmless() {
        let dir = scratch("twice");
        let log = Rc::new(RefCell::new(Vec::new()));
        let session = Takeover::acquire(
            Stock::new(FakeService::new(&log), budget(&dir)),
            wakelock(&dir),
            NoWatchdog,
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
