//! Stopping and restoring stock Xochitl, without ever letting it fail.
//!
//! This is the most dangerous code in the repository, so it is also the most
//! constrained. `xochitl.service` has `StartLimitBurst=4` over ten minutes and
//! an effective `OnFailure=emergency.target remarkable-fail.service` — and
//! **`remarkable-fail.service` does not exist on this image**. A start refused
//! by the rate limiter *fails the unit*, which fires `OnFailure`, which runs
//! `systemd-sulogin-shell` on a serial console. The tablet then looks dead and
//! needs a power cycle.
//!
//! So: never kill, only stop. `reset-failed` before every start, which clears
//! the rate-limit counter as well as the failed state. Keep a local record of
//! recent starts and refuse to run when it is near the limit. One guarded
//! retry, never a loop — hammering is precisely what trips the limiter.
//!
//! These semantics are `tools/device-probe/lib-stock.sh`'s, deliberately: two
//! implementations that disagree about how to restore stock would be worse
//! than either alone. Where this differs, it is stricter.
//!
//! Everything here is behind [`ServiceControl`] so the ordering and the
//! failure paths are testable on a Mac. The part that cannot be tested off the
//! tablet is whether `systemctl` does what its manual says.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::DeviceError;

/// The unit that must never fail.
pub const STOCK_UNIT: &str = "xochitl.service";

/// `StartLimitIntervalUSec` on this image.
pub const START_LIMIT_WINDOW: Duration = Duration::from_secs(600);

/// `StartLimitBurst` on this image.
pub const START_LIMIT_BURST: u32 = 4;

/// How many starts inside the window before a session refuses to begin.
///
/// Three, not four. The budget must leave room for the restore this session is
/// about to need *and* for a guarded retry of it — running down to the limit
/// and then failing to restore is the exact sequence that strands the tablet.
pub const START_BUDGET: u32 = 3;

/// How long to wait for Xochitl to come back before the one guarded retry.
pub const RESTORE_TIMEOUT: Duration = Duration::from_secs(25);

/// The systemd operations this crate performs on stock.
///
/// Note what is absent: there is no `kill`, and no `restart`. Neither is ever
/// correct here, so neither is expressible.
pub trait ServiceControl: fmt::Debug {
    /// Whether the unit is active right now.
    fn is_active(&mut self) -> Result<bool, DeviceError>;
    /// `systemctl stop`. Clean stop; never a signal.
    fn stop(&mut self) -> Result<(), DeviceError>;
    /// `systemctl reset-failed`, which also clears the start rate limiter.
    fn reset_failed(&mut self) -> Result<(), DeviceError>;
    /// `systemctl start`.
    fn start(&mut self) -> Result<(), DeviceError>;
    /// Blocks until the unit is active or `timeout` elapses.
    fn wait_active(&mut self, timeout: Duration) -> Result<bool, DeviceError>;
}

/// A record of when stock was last started, so a session can refuse to pile on.
///
/// Persisted rather than in-memory: the limiter is systemd's and counts across
/// processes, so a per-process counter would be a different number from the one
/// that matters. `/tmp` on this device is tmpfs, so the record clears at the
/// next boot — which is right, because so does the limiter.
#[derive(Debug, Clone)]
pub struct StartBudget {
    path: PathBuf,
}

impl StartBudget {
    /// The shared record, at the same path `lib-stock.sh` uses.
    pub fn shared() -> Self {
        Self::at(Path::new("/tmp/paperclip-xochitl-starts"))
    }

    /// A record at an explicit path.
    pub fn at(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    /// Starts recorded within [`START_LIMIT_WINDOW`] of `now`.
    pub fn recent(&self, now: SystemTime) -> u32 {
        let Ok(contents) = fs::read_to_string(&self.path) else {
            return 0;
        };
        let now = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        contents
            .lines()
            .filter_map(|line| line.trim().parse::<u64>().ok())
            .filter(|stamp| now.saturating_sub(*stamp) < START_LIMIT_WINDOW.as_secs())
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    /// Records a start that is about to happen.
    ///
    /// Recorded *before* the start, not after: a start that hangs still
    /// consumed a slot in systemd's limiter, and a record written afterwards
    /// would miss it.
    ///
    /// Entries older than the window are dropped on the way past. systemd has
    /// forgotten them, so keeping them would only grow a file nobody prunes.
    pub fn record(&self, now: SystemTime) -> Result<(), DeviceError> {
        let stamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let mut contents = String::new();
        let kept = fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse::<u64>().ok())
            .filter(|old| stamp.saturating_sub(*old) < START_LIMIT_WINDOW.as_secs())
            .chain(std::iter::once(stamp))
            .collect::<Vec<_>>();
        for stamp in kept {
            contents.push_str(&format!("{stamp}\n"));
        }
        fs::write(&self.path, contents)
            .map_err(|source| DeviceError::io("write to", &self.path, source))
    }

    /// Whether a session may begin.
    pub fn allows_session(&self, now: SystemTime) -> Result<(), DeviceError> {
        let recent = self.recent(now);
        if recent >= START_BUDGET {
            return Err(DeviceError::unexpected(format!(
                "{recent} xochitl starts in the last {}s, budget {START_BUDGET} of \
                 StartLimitBurst={START_LIMIT_BURST}. Refusing: a start refused by the \
                 rate limiter fails the unit and drops the tablet to an emergency shell. \
                 Wait for the window to clear, or reboot.",
                START_LIMIT_WINDOW.as_secs()
            )));
        }
        Ok(())
    }
}

/// What stock looked like at a moment in time.
///
/// Taken before a session and again after, and compared. A session is not
/// complete because it exited; it is complete because stock is demonstrably
/// where it was found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StockHealth {
    /// Whether `xochitl.service` is active.
    pub active: bool,
    /// Its main PID, if any.
    pub pid: Option<u32>,
    /// Units systemd reports as failed. **Must be empty.**
    pub failed_units: Vec<String>,
    /// Whether `/home` is a mountpoint. If this is false the tablet is locked
    /// and notebooks are not reachable.
    pub home_mounted: bool,
    /// How many notebook directories exist. Must not decrease.
    pub notebooks: Option<usize>,
    /// systemd's `NRestarts` for the unit.
    ///
    /// **The field whose absence let a broken release path report success.**
    /// A session that ends with Xochitl `active` and no failed units can still
    /// have crashed it twice on the way there: systemd's `Restart=` retries,
    /// and once a later start succeeds the unit is active and nothing is
    /// listed as failed. Only this counter shows it.
    pub n_restarts: Option<u32>,
    /// `ExecMainStartTimestamp`. Two sessions apart it should differ; within
    /// one restore it should be stable, because stock should start once.
    pub main_start: Option<String>,
}

impl StockHealth {
    /// Differences that mean this session damaged something.
    ///
    /// A changed PID is *expected* — stock was restarted. A lost notebook, a
    /// failed unit or an unmounted `/home` is not.
    pub fn regressions_from(&self, before: &Self) -> Vec<String> {
        let mut found = Vec::new();
        if before.active && !self.active {
            found.push("xochitl was active before this session and is not now".to_owned());
        }
        if before.home_mounted && !self.home_mounted {
            found.push("/home was mounted before this session and is not now".to_owned());
        }
        if !self.failed_units.is_empty() {
            found.push(format!("failed units: {}", self.failed_units.join(", ")));
        }
        if let (Some(before), Some(after)) = (before.notebooks, self.notebooks)
            && after < before
        {
            found.push(format!("notebook count fell from {before} to {after}"));
        }
        // A restore that needed systemd to retry is not a restore that worked.
        // Each retry is a Xochitl core dump and a step towards the start limit,
        // and past that limit the tablet drops to an emergency shell.
        if let (Some(before), Some(after)) = (before.n_restarts, self.n_restarts)
            && after > before
        {
            found.push(format!(
                "xochitl restarted {} time(s) during this session (NRestarts {before} -> {after}); \
                 each one is a crash, and {START_LIMIT_BURST} within {}s reaches OnFailure and a \
                 serial-console emergency shell",
                after - before,
                START_LIMIT_WINDOW.as_secs()
            ));
        }
        found
    }

    /// Whether this is a healthy tablet to walk away from.
    pub fn is_healthy(&self) -> bool {
        self.active && self.home_mounted && self.failed_units.is_empty()
    }

    /// How many more Xochitl failures this boot window can absorb before
    /// `OnFailure=` sends the tablet to an emergency shell.
    pub fn restarts_remaining(&self) -> Option<u32> {
        self.n_restarts
            .map(|used| START_LIMIT_BURST.saturating_sub(used))
    }

    /// Whether a takeover may begin at all.
    ///
    /// Refuses within two of the limit. The start budget counts *our* starts;
    /// this counts systemd's, and the incident that motivated it consumed two
    /// restarts we never asked for.
    pub fn allows_takeover(&self) -> Result<(), DeviceError> {
        let Some(remaining) = self.restarts_remaining() else {
            return Ok(());
        };
        if remaining <= 2 {
            return Err(DeviceError::unexpected(format!(
                "xochitl has {} of {START_LIMIT_BURST} restarts left in this window. \
                 Refusing to take the display: two more failures reach OnFailure and a \
                 serial-console emergency shell. Wait {}s for the window to clear, or reboot.",
                remaining,
                START_LIMIT_WINDOW.as_secs()
            )));
        }
        Ok(())
    }
}

/// Stock Xochitl, and the only two things Paperclip may do to it.
#[derive(Debug)]
pub struct Stock<S: ServiceControl> {
    service: S,
    budget: StartBudget,
    stopped_by_us: bool,
}

impl<S: ServiceControl> Stock<S> {
    /// Wraps a service control and a start record.
    pub fn new(service: S, budget: StartBudget) -> Self {
        Self {
            service,
            budget,
            stopped_by_us: false,
        }
    }

    /// Whether this code stopped stock and still owes it a restore.
    pub fn owes_restore(&self) -> bool {
        self.stopped_by_us
    }

    /// The service control, for callers that need to read health.
    pub fn service_mut(&mut self) -> &mut S {
        &mut self.service
    }

    /// Stops stock for a session, refusing when the start budget is spent.
    ///
    /// The budget is checked *before* the stop, so a refusal leaves the tablet
    /// exactly as it was found.
    pub fn stop_for_session(&mut self, now: SystemTime) -> Result<(), DeviceError> {
        self.budget.allows_session(now)?;
        if !self.service.is_active()? {
            // Already down. Someone else's session, or a crash. Either way this
            // code did not stop it and must not claim the restore.
            return Ok(());
        }
        self.service.stop()?;
        self.stopped_by_us = true;
        Ok(())
    }

    /// Brings stock back. Idempotent, and safe to call from a failure path.
    ///
    /// One guarded retry, never a loop. Every start is preceded by
    /// `reset-failed` so it can never itself be the start that trips the
    /// limiter, and recorded in the budget so the *next* session can see it.
    pub fn restore(&mut self, now: SystemTime) -> Result<(), DeviceError> {
        if self.service.is_active()? {
            self.stopped_by_us = false;
            return Ok(());
        }

        self.start_once(now)?;
        if self.service.wait_active(RESTORE_TIMEOUT)? {
            self.stopped_by_us = false;
            return Ok(());
        }

        self.start_once(now)?;
        if self.service.wait_active(RESTORE_TIMEOUT)? {
            self.stopped_by_us = false;
            return Ok(());
        }

        // Do not try again. Beyond this point another attempt is more likely to
        // strand the tablet than to rescue it, and a human with the device in
        // their hands has options this process does not.
        Err(DeviceError::unexpected(format!(
            "{STOCK_UNIT} did not come back after two guarded starts. Do NOT retry: \
             further starts risk StartLimitBurst and an emergency shell. Power-cycle \
             the tablet, which always returns it to stock."
        )))
    }

    fn start_once(&mut self, now: SystemTime) -> Result<(), DeviceError> {
        // Order matters: clear the limiter, record the attempt, then start.
        self.service.reset_failed()?;
        self.budget.record(now)?;
        self.service.start()
    }
}

#[cfg(target_os = "linux")]
pub use linux::Systemctl;

#[cfg(target_os = "linux")]
mod linux {
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{STOCK_UNIT, ServiceControl, StockHealth};
    use crate::error::DeviceError;

    /// [`ServiceControl`] over the real `systemctl`.
    #[derive(Debug, Default)]
    pub struct Systemctl;

    impl Systemctl {
        fn run(&self, args: &[&str]) -> Result<(bool, String), DeviceError> {
            let output = Command::new("systemctl")
                .args(args)
                .output()
                .map_err(|source| DeviceError::io("run systemctl for", STOCK_UNIT, source))?;
            let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            Ok((output.status.success(), text))
        }

        /// Stock's main PID, or `None` when it has none.
        ///
        /// Separate from [`Self::health`] because a poll wants this and
        /// nothing else: a health read shells out five times and counts the
        /// notebook directory, which is the wrong thing to do four times a
        /// second while waiting for Xochitl to reclaim the display lock.
        pub fn main_pid(&self) -> Option<u32> {
            self.run(&["show", "-p", "MainPID", "--value", STOCK_UNIT])
                .ok()
                .and_then(|(_, text)| text.parse().ok())
                .filter(|pid| *pid != 0)
        }

        /// Reads everything a health comparison needs.
        pub fn health(&self) -> StockHealth {
            let active = self
                .run(&["is-active", STOCK_UNIT])
                .map(|(_, text)| text == "active")
                .unwrap_or(false);
            let pid = self.main_pid();
            let failed_units = self
                .run(&["--failed", "--no-legend", "--plain"])
                .map(|(_, text)| {
                    text.lines()
                        .filter_map(|line| line.split_whitespace().next())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let home_mounted = Command::new("mountpoint")
                .args(["-q", "/home"])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            let notebooks = std::fs::read_dir("/home/root/.local/share/remarkable/xochitl")
                .map(|entries| entries.count())
                .ok();
            let n_restarts = self
                .run(&["show", "-p", "NRestarts", "--value", STOCK_UNIT])
                .ok()
                .and_then(|(_, text)| text.parse().ok());
            let main_start = self
                .run(&[
                    "show",
                    "-p",
                    "ExecMainStartTimestamp",
                    "--value",
                    STOCK_UNIT,
                ])
                .ok()
                .map(|(_, text)| text)
                .filter(|text| !text.is_empty());

            StockHealth {
                active,
                pid,
                failed_units,
                home_mounted,
                notebooks,
                n_restarts,
                main_start,
            }
        }
    }

    impl ServiceControl for Systemctl {
        fn is_active(&mut self) -> Result<bool, DeviceError> {
            Ok(self.run(&["is-active", STOCK_UNIT])?.1 == "active")
        }

        fn stop(&mut self) -> Result<(), DeviceError> {
            let (ok, text) = self.run(&["stop", STOCK_UNIT])?;
            if ok {
                Ok(())
            } else {
                Err(DeviceError::unexpected(format!(
                    "systemctl stop {STOCK_UNIT} failed: {text}"
                )))
            }
        }

        fn reset_failed(&mut self) -> Result<(), DeviceError> {
            // A failure here is not fatal: the unit may simply not be failed.
            let _ = self.run(&["reset-failed", STOCK_UNIT])?;
            Ok(())
        }

        fn start(&mut self) -> Result<(), DeviceError> {
            // Deliberately not an error: `start` can report failure while the
            // unit still comes up, and the caller decides on `wait_active`.
            // Treating it as fatal here would tempt a retry we must not make.
            let _ = self.run(&["start", STOCK_UNIT])?;
            Ok(())
        }

        fn wait_active(&mut self, timeout: Duration) -> Result<bool, DeviceError> {
            let deadline = Instant::now() + timeout;
            loop {
                if self.is_active()? {
                    return Ok(true);
                }
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{START_BUDGET, START_LIMIT_WINDOW, StartBudget, StockHealth};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("paper-stock-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("creates a scratch directory");
        dir.join("starts")
    }

    fn health(active: bool, notebooks: usize) -> StockHealth {
        StockHealth {
            active,
            pid: Some(1234),
            failed_units: Vec::new(),
            home_mounted: true,
            notebooks: Some(notebooks),
            n_restarts: Some(0),
            main_start: Some("Wed 2026-09-17 04:00:00 UTC".to_owned()),
        }
    }

    #[test]
    fn an_absent_record_counts_as_no_recent_starts() {
        let budget = StartBudget::at(&scratch("absent"));
        assert_eq!(budget.recent(SystemTime::now()), 0);
        budget.allows_session(SystemTime::now()).expect("allows");
    }

    #[test]
    fn starts_outside_the_window_do_not_count_against_the_budget() {
        let budget = StartBudget::at(&scratch("window"));
        let now = SystemTime::now();
        let old = now - START_LIMIT_WINDOW - Duration::from_secs(60);
        for _ in 0..START_BUDGET + 2 {
            budget.record(old).expect("records");
        }
        assert_eq!(budget.recent(now), 0);
        budget.allows_session(now).expect("the window has cleared");
    }

    #[test]
    fn the_budget_refuses_one_start_short_of_the_systemd_limit() {
        let path = scratch("refuse");
        let budget = StartBudget::at(&path);
        let now = SystemTime::now();
        for _ in 0..START_BUDGET - 1 {
            budget.record(now).expect("records");
        }
        budget
            .allows_session(now)
            .expect("still room for this session");

        budget.record(now).expect("records");
        let error = budget.allows_session(now).expect_err("refuses");
        // The message has to say why, because whoever reads it is deciding
        // whether to try again on a tablet they may be holding.
        assert!(error.to_string().contains("emergency shell"), "{error}");
    }

    #[test]
    fn recording_prunes_entries_the_limiter_has_already_forgotten() {
        let path = scratch("prune");
        let budget = StartBudget::at(&path);
        let now = SystemTime::now();
        let old = now - START_LIMIT_WINDOW - Duration::from_secs(60);
        for _ in 0..5 {
            budget.record(old).expect("records");
        }
        budget.record(now).expect("records");

        let lines = fs::read_to_string(&path).expect("written");
        let kept = lines.lines().filter(|line| !line.is_empty()).count();
        assert_eq!(kept, 1, "stale entries should not accumulate: {lines:?}");
        assert_eq!(budget.recent(now), 1);
    }

    #[test]
    fn a_restarted_xochitl_with_a_new_pid_is_not_a_regression() {
        let before = health(true, 42);
        let mut after = health(true, 42);
        after.pid = Some(9999);
        assert!(after.regressions_from(&before).is_empty());
        assert!(after.is_healthy());
    }

    #[test]
    fn a_lost_notebook_is_a_regression_and_a_gained_one_is_not() {
        let before = health(true, 42);
        assert_eq!(health(true, 41).regressions_from(&before).len(), 1);
        assert!(health(true, 41).regressions_from(&before)[0].contains("fell from 42 to 41"));
        assert!(health(true, 43).regressions_from(&before).is_empty());
    }

    #[test]
    fn stock_left_down_or_home_unmounted_are_both_regressions() {
        let before = health(true, 42);

        let down = health(false, 42);
        assert!(down.regressions_from(&before)[0].contains("xochitl"));
        assert!(!down.is_healthy());

        let mut locked = health(true, 42);
        locked.home_mounted = false;
        assert!(locked.regressions_from(&before)[0].contains("/home"));
        assert!(!locked.is_healthy());
    }

    #[test]
    fn any_failed_unit_is_a_regression_even_if_xochitl_is_fine() {
        let before = health(true, 42);
        let mut after = health(true, 42);
        after.failed_units = vec!["something-else.service".to_owned()];
        assert!(after.regressions_from(&before)[0].contains("something-else.service"));
        assert!(!after.is_healthy());
    }

    #[test]
    fn a_restart_during_the_session_is_a_regression_even_though_stock_ends_active() {
        // The 04:05 incident in one assertion. Xochitl ended `active` with no
        // failed units, because the third start worked — and the session had
        // core-dumped it twice on the way there. `is_healthy` cannot see that;
        // only the counter can.
        let before = health(true, 54);
        let mut after = health(true, 54);
        after.n_restarts = Some(2);

        assert!(
            after.is_healthy(),
            "this is exactly why is_healthy is not enough"
        );
        let regressions = after.regressions_from(&before);
        assert_eq!(regressions.len(), 1);
        assert!(
            regressions[0].contains("restarted 2 time(s)"),
            "{regressions:?}"
        );
        assert!(
            regressions[0].contains("emergency shell"),
            "{regressions:?}"
        );
    }

    #[test]
    fn a_takeover_is_refused_when_too_few_restarts_remain() {
        let mut health = health(true, 54);

        health.n_restarts = Some(0);
        assert_eq!(health.restarts_remaining(), Some(4));
        health.allows_takeover().expect("plenty of room");

        health.n_restarts = Some(1);
        health.allows_takeover().expect("three left is still room");

        // Two used, two left: one more failed release could reach the limit.
        health.n_restarts = Some(2);
        let error = health.allows_takeover().expect_err("refuses");
        assert!(error.to_string().contains("emergency shell"), "{error}");

        health.n_restarts = Some(4);
        assert_eq!(health.restarts_remaining(), Some(0));
        health.allows_takeover().expect_err("refuses");
    }

    #[test]
    fn a_device_that_does_not_report_restarts_is_not_blocked_by_the_guard() {
        let mut health = health(true, 54);
        health.n_restarts = None;
        assert_eq!(health.restarts_remaining(), None);
        health.allows_takeover().expect("unknown is not a refusal");
    }

    #[test]
    fn unknown_notebook_counts_are_not_reported_as_a_loss() {
        // `None` means the directory could not be read, which is not evidence
        // that anything was lost.
        let before = health(true, 42);
        let mut after = health(true, 42);
        after.notebooks = None;
        assert!(after.regressions_from(&before).is_empty());
    }
}
