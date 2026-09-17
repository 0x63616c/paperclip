//! The supervisor loop: the part that actually does things.
//!
//! [`crate::state::Machine`] decides; this executes. Keeping the two apart is
//! what lets the §10 table be checked twice over — once as pure decisions on
//! any machine, and once as real signals and real units in a Linux VM — and
//! what stops the second from being confused with the first.
//!
//! The loop is deadline-driven, not poll-driven: it sleeps until the earliest
//! moment a deadline could fire, so a 5-second grace period is 5 seconds and
//! not "5 seconds, plus however long the scan interval happens to be".
//!
//! Control arrives through a file rather than a socket. That is deliberate:
//! §10 requires the recovery path to work without a healthy host socket, and
//! the simplest way to keep that true is for there not to be one. A file also
//! survives the supervisor, so a command written while it was dying is still
//! there afterwards.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use paper_packages::AppId;

use crate::linux::recovery::{LockPaths, RecoveryConfig, StockRecovery, WakeLockPaths};
use crate::linux::systemd::{self, Systemd};
use crate::progress::{Progress, ProgressWatch};
use crate::state::{
    Action, Budget, Diagnosis, Escalation, Event, ExitKind, FailurePolicy, Foreground, Machine,
    SessionState, StopReason,
};
use crate::units::{SESSION_TARGET, SessionPaths};

/// Everything the supervisor needs to know about where it is running.
///
/// Every path is overridable because the VM harness has no `/sys/power`, no
/// `xochitl.service` and no `/home/root`, and a harness that had to run
/// against hardcoded device paths could not exercise the real code at all.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Where session files live.
    pub paths: SessionPaths,
    /// Which unit is stock.
    pub stock_unit: String,
    /// The shared record of recent stock starts. Shared with
    /// `platform/device`'s takeover on purpose: systemd's rate limiter counts
    /// across processes, so the record has to as well.
    pub start_budget: PathBuf,
    /// Where the wakelock files are.
    pub wakelock: WakeLockPaths,
    /// The name the wakelock is taken under.
    pub wakelock_name: String,
    /// The advisory display locks.
    pub locks: LockPaths,
    /// Deadlines.
    pub budget: Budget,
    /// Retry limits.
    pub policy: FailurePolicy,
}

impl RuntimeConfig {
    /// The device configuration from WWW-11.
    pub fn device() -> Self {
        Self {
            paths: SessionPaths::device(),
            stock_unit: crate::units::XOCHITL_UNIT.to_owned(),
            start_budget: PathBuf::from("/tmp/paperclip-xochitl-starts"),
            wakelock: WakeLockPaths::default(),
            wakelock_name: "paperclip".to_owned(),
            locks: LockPaths::default(),
            budget: Budget::default(),
            policy: FailurePolicy::default(),
        }
    }

    /// Reads overrides from a `key = value` file.
    ///
    /// Unknown keys are ignored rather than rejected: this file is written by
    /// `paperctl` and by the harness, and a supervisor that refused to start
    /// because of an unrecognised line would fail closed in the one direction
    /// that leaves the panel held.
    ///
    /// # Errors
    ///
    /// Propagates the read failure if the file cannot be opened.
    pub fn load(path: &PathBuf) -> std::io::Result<Self> {
        let mut config = Self::device();
        let raw = fs::read_to_string(path)?;
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "root" => config.paths.root = PathBuf::from(value),
                "runtime_units" => config.paths.runtime_units = PathBuf::from(value),
                "state" => config.paths.state = PathBuf::from(value),
                "storage" => config.paths.storage = PathBuf::from(value),
                "stock_unit" => config.stock_unit = value.to_owned(),
                "start_budget" => config.start_budget = PathBuf::from(value),
                "wakelock_dir" => {
                    config.wakelock = WakeLockPaths::under(std::path::Path::new(value));
                }
                "wakelock_name" => config.wakelock_name = value.to_owned(),
                "locks_dir" => config.locks = LockPaths::under(std::path::Path::new(value)),
                "switch_deadline_ms" => {
                    if let Ok(ms) = value.parse() {
                        config.budget.switch = Duration::from_millis(ms);
                    }
                }
                "progress_stall_ms" => {
                    if let Ok(ms) = value.parse() {
                        config.budget.progress_stall = Duration::from_millis(ms);
                    }
                }
                "graceful_stop_ms" => {
                    if let Ok(ms) = value.parse() {
                        config.budget.graceful_stop = Duration::from_millis(ms);
                    }
                }
                "force_stop_ms" => {
                    if let Ok(ms) = value.parse() {
                        config.budget.force_stop = Duration::from_millis(ms);
                    }
                }
                "restore_deadline_ms" => {
                    if let Ok(ms) = value.parse() {
                        config.budget.restore = Duration::from_millis(ms);
                    }
                }
                "session_failures" => {
                    if let Ok(count) = value.parse() {
                        config.policy.session_failures = count;
                    }
                }
                "restore_attempts" => {
                    if let Ok(count) = value.parse() {
                        config.policy.restore_attempts = count;
                    }
                }
                _ => {}
            }
        }
        Ok(config)
    }

    /// The file the supervisor reads commands from.
    pub fn command_file(&self) -> PathBuf {
        self.paths.state.join("command")
    }

    /// The file the supervisor writes its state to, and the only thing any
    /// other process should read to find out what it is doing.
    pub fn status_file(&self) -> PathBuf {
        self.paths.state.join("status")
    }

    /// The recovery configuration implied by this runtime.
    pub fn recovery(&self) -> RecoveryConfig {
        RecoveryConfig {
            stock_unit: self.stock_unit.clone(),
            start_budget: self.start_budget.clone(),
            wakelock: self.wakelock.clone(),
            wakelock_name: self.wakelock_name.clone(),
            locks: self.locks.clone(),
            diagnostics: self.paths.diagnostics(),
        }
    }
}

/// How the supervisor finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It was asked to stop and stock owns the display.
    StoppedAtStock,
    /// It halted. Terminal, with diagnostics on disk.
    ///
    /// The process still exits **zero**. Exiting non-zero would trip the
    /// unit's `OnFailure=` and start the recovery that has already failed —
    /// which is the restart loop §10 forbids, wearing a different hat.
    Halted {
        /// Why.
        diagnosis: Diagnosis,
        /// Where the evidence is.
        diagnostics: PathBuf,
    },
}

/// The supervisor.
#[derive(Debug)]
pub struct Supervisor {
    config: RuntimeConfig,
    machine: Machine,
    systemd: Systemd,
    recovery: StockRecovery,
    progress: Option<ProgressWatch>,
    session_unit: Option<String>,
    /// Observations produced while executing an action, waiting to be fed
    /// back through the machine on the next pass.
    ///
    /// A restore is a blocking call that produces an observation — stock came
    /// back, or it did not. Feeding that straight into
    /// [`Machine::handle`](crate::state::Machine::handle) from inside
    /// `execute` would be re-entrant, and would swallow the [`Action::Halt`]
    /// it can produce: the outer loop is what turns a halt into an exit, and a
    /// nested call has nowhere to return it to. That bug survived a VM run
    /// looking like "the supervisor is stuck in `switching`".
    queued: Vec<Event>,
    /// Who the supervisor is currently trying to bring up.
    ///
    /// Not the same as [`Machine::foreground`], and the difference is the
    /// point: during a switch the machine's foreground is still the *outgoing*
    /// owner, because nothing has arrived yet. Reporting readiness against the
    /// outgoing owner would be reporting it against the wrong session, and the
    /// machine would correctly ignore it forever.
    incoming: Option<Foreground>,
}

impl Supervisor {
    /// Builds a supervisor that believes stock currently owns the display.
    pub fn new(config: RuntimeConfig) -> Self {
        let systemd = Systemd::default();
        let recovery = StockRecovery::new(config.recovery());
        Self {
            machine: Machine::new(config.budget, config.policy),
            systemd,
            recovery,
            progress: None,
            session_unit: None,
            queued: Vec::new(),
            incoming: None,
            config,
        }
    }

    /// The state machine, for tests and for `paperctl status`.
    pub fn state(&self) -> SessionState {
        self.machine.state()
    }

    /// Runs until asked to stop or until the machine halts.
    ///
    /// # Errors
    ///
    /// Propagates a failure to create the state directory, which is fatal:
    /// without it there is nowhere to record a diagnosis.
    pub fn run(&mut self) -> std::io::Result<Outcome> {
        fs::create_dir_all(&self.config.paths.state)?;
        self.write_status();
        systemd::notify_ready();

        loop {
            systemd::notify_watchdog();
            let now = Instant::now();

            for event in self.collect(now) {
                // Re-read the clock per event. An action can block for a long
                // time — a restore is two guarded starts with a 25 s wait each
                // — and judging a deadline against a `now` from before that
                // call would expire it on arrival.
                let now = Instant::now();
                let before = self.machine.state();
                let actions = self.machine.handle(&event, now);
                // Before executing, not after. A restore blocks for as long as
                // two guarded starts take, and during that time the status
                // file is the only thing telling anyone what is happening.
                self.write_status();
                // Every transition and every side effect goes to the journal.
                // §10 asks for diagnostics to be available after a failure,
                // and the journal is the one place that is still readable when
                // the state directory is gone and the supervisor is not.
                if !actions.is_empty() || self.machine.state() != before {
                    eprintln!(
                        "paperclip-host: {event:?} :: {before} -> {} :: {}",
                        self.machine.state(),
                        actions
                            .iter()
                            .map(|action| format!("{action:?}"))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    );
                }
                for action in actions {
                    if let Some(outcome) = self.execute(&action, now) {
                        self.write_status();
                        return Ok(outcome);
                    }
                }
            }
            self.write_status();

            if self.stop_requested() && self.machine.state() == SessionState::Stock {
                return Ok(Outcome::StoppedAtStock);
            }

            // Sleep to the next deadline, never past it, and never longer than
            // the watchdog interval.
            let cap = Duration::from_millis(500);
            let sleep = self
                .machine
                .next_deadline()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(cap)
                .min(cap)
                .max(Duration::from_millis(20));
            std::thread::sleep(sleep);
        }
    }

    /// The owner every observation this pass is about.
    fn owner(&self) -> Foreground {
        match self.machine.state() {
            SessionState::Switching => self
                .incoming
                .clone()
                .unwrap_or_else(|| self.machine.foreground().clone()),
            _ => self.machine.foreground().clone(),
        }
    }

    /// Everything that happened since the last pass.
    fn collect(&mut self, now: Instant) -> Vec<Event> {
        // Anything an action produced comes first: it is already older than
        // whatever this pass is about to read.
        let mut events: Vec<Event> = self.queued.drain(..).collect();
        events.push(Event::Tick);

        if let Some(command) = self.take_command() {
            match command.as_str() {
                "stock" | "stop" => events.push(Event::SwitchRequested {
                    to: Foreground::Stock,
                }),
                "home" => events.push(Event::SwitchRequested {
                    to: Foreground::Home,
                }),
                other => {
                    if let Some(id) = other.strip_prefix("app ")
                        && let Ok(app) = id.trim().parse::<AppId>()
                    {
                        events.push(Event::SwitchRequested {
                            to: Foreground::App(app),
                        });
                    }
                }
            }
        }

        if let Some(unit) = self.session_unit.clone() {
            let owner = self.owner();
            if self.systemd.is_active(&unit) {
                if !self.machine.state().owns_display() {
                    // The unit came up. Readiness is systemd's `active` under
                    // `Type=notify`, which means the app sent READY=1 — not
                    // that a process exists.
                    events.push(Event::ProcessReady {
                        owner: owner.clone(),
                    });
                    if self.display_owned_by_session(&unit) {
                        events.push(Event::DisplayOwned { owner });
                    }
                } else if !self.display_owned_by_session(&unit) {
                    // The session is alive and no longer owns the panel: the
                    // process that was holding it on our behalf is gone. §10's
                    // "display process crash" row, and it is deliberately not
                    // inferred from the session's own exit — the session is
                    // still running.
                    events.push(Event::DisplayHostLost);
                } else if let Some(progress) = self.progress.as_mut() {
                    match progress.poll(SystemTime::now(), self.config.budget.progress_stall) {
                        Ok(Progress::Stalled { stalled_for }) => {
                            events.push(Event::ProgressStalled { owner, stalled_for });
                        }
                        Ok(_) => {}
                        Err(_) => {
                            // A witness we cannot read is not a witness. Treat
                            // it as no progress rather than as progress.
                        }
                    }
                }
            } else if self.machine.state().owns_display()
                || self.machine.state() == SessionState::Switching
            {
                events.push(Event::SessionExited {
                    owner,
                    status: self.exit_kind(&unit),
                });
            }
        }

        if self.machine.state() == SessionState::Switching
            && matches!(
                self.machine.foreground(),
                Foreground::Home | Foreground::App(_)
            )
            && self.systemd.is_active(&self.config.stock_unit)
        {
            events.push(Event::StockRunning);
        }
        let _ = now;
        events
    }

    /// Performs one action. `Some(..)` ends the run.
    fn execute(&mut self, action: &Action, now: Instant) -> Option<Outcome> {
        match action {
            Action::StartForeground { target, .. } => {
                self.start_foreground(target, now);
                None
            }
            Action::TerminateSession { reason, escalation } => {
                self.terminate(*reason, *escalation);
                None
            }
            Action::ReleaseDisplay => {
                self.release_display();
                None
            }
            Action::RestoreStock { attempt, .. } => {
                self.restore(*attempt);
                None
            }
            Action::CaptureDiagnostics { reason } => {
                self.recovery.capture(reason.description());
                None
            }
            Action::Halt { diagnosis } => {
                let diagnostics = self.recovery.capture(&diagnosis.summary());
                systemd::notify_status(&diagnosis.summary());
                Some(Outcome::Halted {
                    diagnosis: diagnosis.clone(),
                    diagnostics,
                })
            }
        }
    }

    fn start_foreground(&mut self, target: &Foreground, now: Instant) {
        match target {
            Foreground::Stock => {
                self.session_unit = None;
                self.progress = None;
                self.incoming = Some(Foreground::Stock);
                self.restore(1);
            }
            other => {
                let unit = session_unit_name(other);
                self.incoming = Some(other.clone());
                self.progress = Some(ProgressWatch::new(
                    self.config.paths.progress().join(other.label()),
                ));
                self.session_unit = Some(unit.clone());
                // Starting the session target is how stock is taken down: the
                // target `Conflicts=` the stock unit, so systemd stops it
                // cleanly and in the right order. Paperclip never issues a
                // stop against stock from inside the supervisor, and never a
                // signal — the ordering is systemd's to enforce, and a clean
                // stop is the only kind that keeps the tablet off a serial
                // console.
                let _ = self.systemd.start(SESSION_TARGET);
                // A start systemd refused is not a session. Nothing is
                // invented here: the switch deadline takes it to recovery,
                // because the machine has no evidence the session arrived.
                let _ = self.systemd.start(&unit);
            }
        }
    }

    fn terminate(&mut self, reason: StopReason, escalation: Escalation) {
        let Some(unit) = self.session_unit.clone() else {
            return;
        };
        // Ask systemd first — a clean stop lets the app save (§5) — then sweep
        // the cgroup for anything that outlived it. The sweep is the part that
        // makes "child survives parent" a non-event.
        let _ = self.systemd.stop(&unit);
        let protected: Vec<u32> = self
            .systemd
            .main_pid(&self.config.stock_unit)
            .into_iter()
            .collect();
        let tree = match self.systemd.cgroup_of(&unit) {
            Some(cgroup) => crate::linux::process::SessionTree::in_cgroup(cgroup),
            None => crate::linux::process::SessionTree::below(std::process::id()),
        }
        .protecting(protected);
        if let Ok(result) = tree.terminate(escalation)
            && !result.emptied
        {
            // Survivors are recorded, not retried forever. §10 asks for
            // *bounded* force termination.
            let _ = fs::write(
                self.config.paths.state.join("survivors"),
                format!(
                    "{reason:?}: {} process(es) survived SIGKILL: {:?}\n",
                    result.survivors.len(),
                    result.survivors
                ),
            );
        }
        self.progress = None;
    }

    fn release_display(&mut self) {
        // Only the registry entries, and only ours. The wakelock is released
        // by the recovery path, after stock is confirmed up.
        let _ = fs::remove_file(&self.config.locks.registry);
        let _ = fs::remove_file(&self.config.locks.epd);
    }

    /// Runs the independent restore and queues what it observed.
    ///
    /// Blocking, and it can block for a while: `platform/device` gives stock
    /// two guarded starts with a 25 s wait each before it refuses. Nothing
    /// here shortens that — a restore declared failed early is worse than a
    /// slow one — so the status file is written before this is called, and it
    /// says `recovering` for the duration.
    fn restore(&mut self, attempt: u32) {
        match self.recovery.restore() {
            Ok(outcome) => {
                eprintln!(
                    "paperclip-host: restore attempt {attempt}: {}",
                    outcome.summary()
                );
                self.queued.push(Event::StockRunning);
                self.queued.push(Event::DisplayReleased);
            }
            Err(error) => {
                eprintln!("paperclip-host: restore attempt {attempt} FAILED: {error}");
                self.recovery
                    .capture(&format!("restore attempt {attempt}: {error}"));
                self.queued.push(Event::RestoreFailed {
                    reason: error.to_string(),
                });
            }
        }
    }

    /// Whether the session actually took the panel, rather than merely
    /// starting. Ownership is the lock registry naming a pid inside the
    /// session's own cgroup.
    fn display_owned_by_session(&self, unit: &str) -> bool {
        let Ok(raw) = fs::read_to_string(&self.config.locks.registry) else {
            return false;
        };
        let Some(holder) = raw
            .split_whitespace()
            .next()
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            return false;
        };
        let Some(cgroup) = self.systemd.cgroup_of(unit) else {
            return false;
        };
        crate::linux::process::SessionTree::in_cgroup(cgroup)
            .members()
            .map(|members| members.contains(&holder))
            .unwrap_or(false)
    }

    fn exit_kind(&self, unit: &str) -> ExitKind {
        let result = self.systemd.property(unit, "Result").unwrap_or_default();
        let code = self
            .systemd
            .property(unit, "ExecMainCode")
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0);
        let status = self
            .systemd
            .property(unit, "ExecMainStatus")
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0);
        match result.as_str() {
            // `CLD_KILLED`/`CLD_DUMPED` — a panic that aborted, a segfault, an
            // OOM kill, or our own SIGKILL.
            _ if code == 2 || code == 3 => ExitKind::Signal,
            "signal" | "core-dump" | "watchdog" | "oom-kill" => ExitKind::Signal,
            "timeout" => ExitKind::ForcedByDeadline,
            "success" if status == 0 => ExitKind::Clean,
            _ => ExitKind::Error,
        }
    }

    fn take_command(&self) -> Option<String> {
        let path = self.config.command_file();
        let raw = fs::read_to_string(&path).ok()?;
        let _ = fs::remove_file(&path);
        let command = raw.trim().to_owned();
        (!command.is_empty()).then_some(command)
    }

    fn stop_requested(&self) -> bool {
        self.config.paths.state.join("stop").exists()
    }

    fn write_status(&self) {
        let status = format!(
            "state={}\nforeground={}\nmay_relaunch={}\ndiagnosis={}\n",
            self.machine.state(),
            self.machine.foreground(),
            self.machine.may_relaunch(),
            self.machine
                .diagnosis()
                .map_or_else(String::new, Diagnosis::summary),
        );
        let _ = fs::write(self.config.status_file(), status);
    }
}

/// The unit name for a foreground owner.
///
/// One template instance per owner, so systemd tracks each in its own cgroup
/// and a sweep has an exact subtree to empty.
pub fn session_unit_name(owner: &Foreground) -> String {
    match owner {
        Foreground::Stock => SESSION_TARGET.to_owned(),
        Foreground::Home => "paperclip-app@home.service".to_owned(),
        Foreground::App(id) => format!("paperclip-app@{id}.service"),
    }
}
