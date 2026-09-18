//! Fakes for `paper_sys`'s effects.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use paper_sys::{
    BatteryReading, Clock, Network, NetworkReading, PowerSource, Process, ProcessCommand,
    ProcessError, ProcessOutput, Storage, StorageError, UnitControl, UnitError, WallClock,
};

/// A clock that only moves when [`Clock::sleep`] is called, so a test that
/// grades something against a real-world deadline still finishes instantly.
///
/// Moved out of `platform/updater/tests/upgrade.rs`, where this exact shape
/// backed all 34 of that crate's tests before WWW-46.
#[derive(Debug)]
pub struct FakeClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeClock {
    /// A clock reading `Instant::now()` and advancing only on [`Clock::sleep`].
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().expect("clock offset")
    }

    fn sleep(&self, duration: Duration) {
        *self.offset.lock().expect("clock offset") += duration;
    }
}

#[derive(Debug, Default)]
struct ProcessState {
    calls: Vec<ProcessCommand>,
    /// Responses handed out in order; the last one repeats once exhausted, so
    /// a caller that scripts one answer does not have to know how many times
    /// it will be asked.
    responses: VecDeque<ProcessOutput>,
}

/// A [`Process`] that records every command and answers from a script.
#[derive(Debug, Clone)]
pub struct FakeProcess {
    state: Rc<std::cell::RefCell<ProcessState>>,
}

impl Default for FakeProcess {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeProcess {
    /// A process that succeeds with empty output until scripted otherwise.
    pub fn new() -> Self {
        Self {
            state: Rc::new(std::cell::RefCell::new(ProcessState::default())),
        }
    }

    /// Every call succeeds, with `stdout` (trimmed by callers as they see fit).
    pub fn script_success(&self, stdout: impl Into<String>) {
        let mut state = self.state.borrow_mut();
        state.responses.clear();
        state.responses.push_back(ProcessOutput {
            success: true,
            code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        });
    }

    /// Every call fails, with `stderr` carrying what `systemctl` would have
    /// printed — the real adapter reads a failure's reason from stderr first
    /// (WWW-94), matching a real `systemctl start`/`stop` refusal, which
    /// writes there and leaves stdout empty.
    pub fn script_failure(&self, stderr: impl Into<String>) {
        let mut state = self.state.borrow_mut();
        state.responses.clear();
        state.responses.push_back(ProcessOutput {
            success: false,
            code: Some(1),
            stdout: String::new(),
            stderr: stderr.into(),
        });
    }

    /// Every call fails with no output on either stream, as `systemctl`
    /// leaves it when it is killed or refuses before printing anything.
    pub fn script_failure_silent(&self) {
        let mut state = self.state.borrow_mut();
        state.responses.clear();
        state.responses.push_back(ProcessOutput {
            success: false,
            code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
        });
    }

    /// Each call in turn gets the next line of `stdout`; the last repeats
    /// once the sequence is exhausted.
    pub fn script_sequence<I, S>(&self, outputs: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut state = self.state.borrow_mut();
        state.responses = outputs
            .into_iter()
            .map(|stdout| ProcessOutput {
                success: true,
                code: Some(0),
                stdout: stdout.into(),
                stderr: String::new(),
            })
            .collect();
    }

    /// Every command's arguments, in call order (the program name, e.g.
    /// `"systemctl"`, is left off — every call here is to the same program).
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.state
            .borrow()
            .calls
            .iter()
            .map(|command| command.args.clone())
            .collect()
    }
}

impl Process for FakeProcess {
    fn run(&self, command: &ProcessCommand) -> Result<ProcessOutput, ProcessError> {
        let mut state = self.state.borrow_mut();
        state.calls.push(command.clone());
        let next = if state.responses.len() > 1 {
            state.responses.pop_front()
        } else {
            state.responses.front().cloned()
        };
        Ok(next.unwrap_or(ProcessOutput {
            success: true,
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }))
    }
}

/// One unit's state in a [`FakeUnitControl`].
#[derive(Debug, Clone, Default)]
struct UnitState {
    active: bool,
    failed: bool,
    properties: HashMap<String, String>,
}

/// A [`UnitControl`] over an in-memory map of unit name to state.
#[derive(Debug, Default)]
pub struct FakeUnitControl {
    units: Mutex<HashMap<String, UnitState>>,
    refuse_start: Mutex<bool>,
    fail_start: Mutex<bool>,
}

impl FakeUnitControl {
    /// No units known; every unit reads as inactive until started.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks `unit` active from the start, as if it were already running.
    pub fn set_active(&self, unit: &str, active: bool) {
        self.units
            .lock()
            .expect("units")
            .entry(unit.to_owned())
            .or_default()
            .active = active;
    }

    /// Sets a property `start`/`is_active` do not otherwise populate, e.g.
    /// `NRestarts`.
    pub fn set_property(&self, unit: &str, name: &str, value: impl Into<String>) {
        self.units
            .lock()
            .expect("units")
            .entry(unit.to_owned())
            .or_default()
            .properties
            .insert(name.to_owned(), value.into());
    }

    /// Makes every subsequent [`UnitControl::start`] a silent no-op — the
    /// unit accepts the start but never becomes active. Models a start that
    /// systemd accepted but the process failed to reach `active` from.
    pub fn refuse_start(&self) {
        *self.refuse_start.lock().expect("refuse_start") = true;
    }

    /// Makes every subsequent [`UnitControl::start`] return `Err` instead of
    /// accepting the start — `systemctl start` itself failing, distinct from
    /// [`Self::refuse_start`]'s "accepted but never came up". The two are
    /// different facts to a caller: this one is known the instant `start`
    /// returns, the other only after however long the caller waits.
    pub fn fail_start(&self) {
        *self.fail_start.lock().expect("fail_start") = true;
    }
}

impl UnitControl for FakeUnitControl {
    fn start(&self, unit: &str) -> Result<(), UnitError> {
        if *self.fail_start.lock().expect("fail_start") {
            return Err(UnitError {
                verb: "start",
                unit: unit.to_owned(),
                reason: "refused by FakeUnitControl::fail_start".to_owned(),
            });
        }
        let mut units = self.units.lock().expect("units");
        let entry = units.entry(unit.to_owned()).or_default();
        if !*self.refuse_start.lock().expect("refuse_start") {
            entry.active = true;
        }
        entry.failed = false;
        Ok(())
    }

    fn stop(&self, unit: &str) -> Result<(), UnitError> {
        self.units
            .lock()
            .expect("units")
            .entry(unit.to_owned())
            .or_default()
            .active = false;
        Ok(())
    }

    fn reset_failed(&self, unit: &str) -> Result<(), UnitError> {
        self.units
            .lock()
            .expect("units")
            .entry(unit.to_owned())
            .or_default()
            .failed = false;
        Ok(())
    }

    fn is_active(&self, unit: &str) -> bool {
        self.units
            .lock()
            .expect("units")
            .get(unit)
            .is_some_and(|state| state.active)
    }

    fn is_failed(&self, unit: &str) -> bool {
        self.units
            .lock()
            .expect("units")
            .get(unit)
            .is_some_and(|state| state.failed)
    }

    fn main_pid(&self, unit: &str) -> Option<u32> {
        if self.is_active(unit) { Some(1) } else { None }
    }

    fn property(&self, unit: &str, name: &str) -> Option<String> {
        self.units
            .lock()
            .expect("units")
            .get(unit)
            .and_then(|state| state.properties.get(name))
            .cloned()
    }
}

/// A [`Storage`] over an in-memory tree, able to fail on command.
///
/// This is the seam the WWW-46 ticket asks for in place of hand-deleting a
/// directory to simulate a power cut: a test can call
/// [`FakeStorage::fail_next_commit`] and see exactly how a caller reacts to
/// the commit rename itself failing, rather than only to a directory that was
/// never there.
#[derive(Debug, Default)]
pub struct FakeStorage {
    fail_next_commit: Mutex<bool>,
    real: paper_sys::Filesystem,
}

impl FakeStorage {
    /// A fake backed by a real temporary-directory filesystem underneath —
    /// callers still pass real paths (typically inside a `tempfile::TempDir`)
    /// so directory contents can be asserted on directly; what this adds is
    /// the ability to fail one operation on command.
    pub fn new() -> Self {
        Self::default()
    }

    /// The next [`Storage::commit_directory`] call fails without touching
    /// either path, simulating a power cut between staging and activation.
    pub fn fail_next_commit(&self) {
        *self.fail_next_commit.lock().expect("fail flag") = true;
    }
}

impl Storage for FakeStorage {
    fn create_dir_if_missing(&self, path: &std::path::Path) -> Result<(), StorageError> {
        self.real.create_dir_if_missing(path)
    }

    fn remove_tree(&self, path: &std::path::Path) -> Result<(), StorageError> {
        self.real.remove_tree(path)
    }

    fn commit_directory(
        &self,
        staged: &std::path::Path,
        destination: &std::path::Path,
    ) -> Result<(), StorageError> {
        let mut flag = self.fail_next_commit.lock().expect("fail flag");
        if *flag {
            *flag = false;
            return Err(StorageError {
                op: "commit",
                path: destination.to_path_buf(),
                source: std::io::Error::other("simulated power cut during commit"),
            });
        }
        drop(flag);
        self.real.commit_directory(staged, destination)
    }

    fn copy_tree(
        &self,
        source: &std::path::Path,
        destination: &std::path::Path,
    ) -> Result<(), StorageError> {
        self.real.copy_tree(source, destination)
    }
}

/// A [`WallClock`] set to whatever a test scripts, so a fact carrying a time
/// does not depend on when the test happened to run.
#[derive(Debug, Clone, Copy)]
pub struct FakeWallClock {
    millis: u64,
}

impl Default for FakeWallClock {
    fn default() -> Self {
        Self::at(0)
    }
}

impl FakeWallClock {
    /// A clock reading `millis` milliseconds since the Unix epoch.
    pub fn at(millis: u64) -> Self {
        Self { millis }
    }
}

impl WallClock for FakeWallClock {
    fn now_unix_millis(&self) -> u64 {
        self.millis
    }
}

/// A [`PowerSource`] that answers from a script rather than reading
/// `/sys/class/power_supply`.
#[derive(Debug, Clone, Default)]
pub struct FakePowerSource {
    reading: Option<BatteryReading>,
}

impl FakePowerSource {
    /// A source with no reading yet — [`PowerSource::read`] answers `None`
    /// until [`Self::set`] is called, the same as a device with no battery
    /// node this reader could find.
    pub fn new() -> Self {
        Self::default()
    }

    /// The next [`PowerSource::read`] answers `reading`.
    pub fn set(&mut self, reading: BatteryReading) {
        self.reading = Some(reading);
    }
}

impl PowerSource for FakePowerSource {
    fn read(&self) -> Option<BatteryReading> {
        self.reading
    }
}

/// A [`Network`] that answers from a script rather than shelling out to
/// `nmcli`.
#[derive(Debug, Clone, Default)]
pub struct FakeNetwork {
    reading: Option<NetworkReading>,
}

impl FakeNetwork {
    /// A source with no reading yet — [`Network::read`] answers `None` until
    /// [`Self::set`] is called, the same as `nmcli` failing to run at all.
    pub fn new() -> Self {
        Self::default()
    }

    /// The next [`Network::read`] answers `reading`.
    pub fn set(&mut self, reading: NetworkReading) {
        self.reading = Some(reading);
    }
}

impl Network for FakeNetwork {
    fn read(&self) -> Option<NetworkReading> {
        self.reading.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paper_sys::{ChargeDirection, Systemctl, UnitControl, wait_active};

    #[test]
    fn systemctl_start_builds_the_expected_argv() {
        let process = FakeProcess::new();
        let control = Systemctl::new(process.clone());
        control.start("xochitl.service").expect("starts");
        assert_eq!(
            process.calls(),
            vec![vec!["start".to_owned(), "xochitl.service".to_owned()]]
        );
    }

    #[test]
    fn systemctl_is_active_reads_the_trimmed_stdout() {
        let process = FakeProcess::new();
        process.script_success("active\n");
        let control = Systemctl::new(process);
        assert!(control.is_active("xochitl.service"));
    }

    #[test]
    fn systemctl_a_failed_stop_is_an_error_naming_the_unit() {
        let process = FakeProcess::new();
        process.script_failure("Unit not found.");
        let control = Systemctl::new(process);
        let error = control.stop("xochitl.service").expect_err("refuses");
        assert!(error.to_string().contains("xochitl.service"));
    }

    /// The WWW-94 regression: a failed `systemctl start` must carry systemd's
    /// own explanation, not an empty reason. `Systemctl::run` used to read
    /// only stdout, and a real refusal — e.g. `226/NAMESPACE` for a missing
    /// `ReadWritePaths=` directory — is written to stderr, so the old adapter
    /// reported "systemctl start paperclip-app@home.service failed: " with
    /// nothing after the colon.
    #[test]
    fn a_failed_start_carries_systemds_stderr_as_a_non_empty_reason() {
        let process = FakeProcess::new();
        process.script_failure(
            "Job for paperclip-app@home.service failed because the control process \
             exited with error code.\nSee \"systemctl status paperclip-app@home.service\" \
             and \"journalctl -xeu paperclip-app@home.service\" for details.",
        );
        let control = Systemctl::new(process);
        let error = control
            .start("paperclip-app@home.service")
            .expect_err("refuses");
        assert!(!error.reason.is_empty(), "reason must not be empty");
        assert!(error.reason.contains("control process"));
    }

    #[test]
    fn a_failed_start_falls_back_to_the_exit_code_when_systemctl_wrote_nothing() {
        let process = FakeProcess::new();
        process.script_failure_silent();
        let control = Systemctl::new(process);
        let error = control.start("a.service").expect_err("refuses");
        assert!(!error.reason.is_empty(), "reason must not be empty");
        assert!(error.reason.contains('1'), "should mention the exit code");
    }

    #[test]
    fn wait_active_polls_until_active_or_the_deadline() {
        let process = FakeProcess::new();
        process.script_sequence(["inactive", "inactive", "active"]);
        let control = Systemctl::new(process);
        let clock = FakeClock::new();

        assert!(wait_active(
            &control,
            &clock,
            "xochitl.service",
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn wait_active_gives_up_at_the_deadline() {
        let process = FakeProcess::new();
        process.script_success("inactive\n");
        let control = Systemctl::new(process);
        let clock = FakeClock::new();

        assert!(!wait_active(
            &control,
            &clock,
            "xochitl.service",
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn fake_clock_only_advances_on_sleep() {
        let clock = FakeClock::new();
        let start = clock.now();
        assert_eq!(clock.now(), start);
        clock.sleep(Duration::from_secs(30));
        assert_eq!(clock.now(), start + Duration::from_secs(30));
    }

    #[test]
    fn fake_process_records_calls_and_answers_from_the_script() {
        let process = FakeProcess::new();
        process.script_success("active");
        let output = process
            .run(&ProcessCommand::new("systemctl").arg("is-active"))
            .expect("runs");
        assert!(output.success);
        assert_eq!(output.stdout, "active");
        assert_eq!(process.calls(), vec![vec!["is-active".to_owned()]]);
    }

    #[test]
    fn fake_process_sequence_is_consumed_in_order_then_repeats() {
        let process = FakeProcess::new();
        process.script_sequence(["one", "two"]);
        let cmd = ProcessCommand::new("x");
        assert_eq!(process.run(&cmd).unwrap().stdout, "one");
        assert_eq!(process.run(&cmd).unwrap().stdout, "two");
        assert_eq!(process.run(&cmd).unwrap().stdout, "two");
    }

    #[test]
    fn fake_unit_control_tracks_active_state_across_start_and_stop() {
        let control = FakeUnitControl::new();
        assert!(!control.is_active("a.service"));
        control.start("a.service").expect("starts");
        assert!(control.is_active("a.service"));
        control.stop("a.service").expect("stops");
        assert!(!control.is_active("a.service"));
    }

    #[test]
    fn refusing_a_start_leaves_the_unit_inactive() {
        let control = FakeUnitControl::new();
        control.refuse_start();
        control.start("a.service").expect("accepted");
        assert!(
            !control.is_active("a.service"),
            "start was accepted but never came up"
        );
    }

    #[test]
    fn fake_wall_clock_reads_whatever_it_was_set_to() {
        let clock = FakeWallClock::at(1_700_000_000_000);
        assert_eq!(clock.now_unix_millis(), 1_700_000_000_000);
    }

    #[test]
    fn fake_power_source_answers_none_until_set() {
        let mut source = FakePowerSource::new();
        assert!(source.read().is_none());
        source.set(BatteryReading {
            percent: 42,
            direction: ChargeDirection::Charging,
        });
        assert_eq!(source.read().unwrap().percent, 42);
    }

    #[test]
    fn fake_network_answers_none_until_set() {
        let mut network = FakeNetwork::new();
        assert!(network.read().is_none());
        network.set(paper_sys::NetworkReading::DISCONNECTED);
        assert_eq!(
            network.read(),
            Some(paper_sys::NetworkReading::DISCONNECTED)
        );
    }

    #[test]
    fn fake_storage_fails_exactly_the_next_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let staged = dir.path().join("staged");
        let destination = dir.path().join("dest");
        std::fs::create_dir_all(&staged).expect("stage");

        let storage = FakeStorage::new();
        storage.fail_next_commit();
        storage
            .commit_directory(&staged, &destination)
            .expect_err("the injected failure fires");
        assert!(
            staged.exists(),
            "a failed commit must not have moved anything"
        );

        storage
            .commit_directory(&staged, &destination)
            .expect("the failure was one-shot");
        assert!(destination.exists());
    }
}
