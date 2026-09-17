//! The foreground state machine (§10).
//!
//! Pure: no processes, no files, no clock of its own. Events go in, actions
//! come out, and `now` is always supplied by the caller. That is what lets the
//! §10 failure table be a table of unit tests as well as a table of VM cases —
//! and what stops "the code ran" from being mistaken for "the rule held".
//!
//! Three rules are structural rather than commented:
//!
//! 1. **Starting is not arriving.** A switch completes only when *both* the
//!    process reported ready and display ownership was observed. Either alone
//!    leaves the machine in [`SessionState::Switching`] until the deadline
//!    takes it to [`SessionState::Recovering`].
//! 2. **Exactly one foreground owner.** [`Machine::foreground`] is a single
//!    value, and a switch that has not completed has not changed it.
//! 3. **[`SessionState::Failed`] is terminal.** Nothing in this module moves
//!    out of it; only an explicit operator call does, and it is named
//!    [`Machine::operator_retry`] so it cannot happen by accident.

use std::fmt;
use std::time::{Duration, Instant};

use paper_packages::AppId;

/// Where the foreground is, or is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Stock Xochitl owns the display. The resting state, and the state a
    /// reboot returns to.
    Stock,
    /// A switch is in flight. Nothing owns the foreground yet.
    Switching,
    /// The Paperclip home screen owns the display.
    Home,
    /// An app owns the display.
    App,
    /// Something failed and stock is being restored.
    Recovering,
    /// Recovery did not work. Terminal, on purpose.
    Failed,
}

impl SessionState {
    /// The name used in logs, `paperctl status`, and harness assertions.
    pub fn name(self) -> &'static str {
        match self {
            SessionState::Stock => "stock",
            SessionState::Switching => "switching",
            SessionState::Home => "home",
            SessionState::App => "app",
            SessionState::Recovering => "recovering",
            SessionState::Failed => "failed",
        }
    }

    /// Whether a Paperclip process is expected to be holding the panel.
    pub fn owns_display(self) -> bool {
        matches!(self, SessionState::Home | SessionState::App)
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Who the foreground owner is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Foreground {
    /// Stock Xochitl.
    Stock,
    /// The Paperclip home screen.
    Home,
    /// A named app.
    App(AppId),
}

impl Foreground {
    /// The state this owner corresponds to once it has arrived.
    fn arrived_state(&self) -> SessionState {
        match self {
            Foreground::Stock => SessionState::Stock,
            Foreground::Home => SessionState::Home,
            Foreground::App(_) => SessionState::App,
        }
    }

    /// A stable label for units, log lines and lock files.
    pub fn label(&self) -> String {
        match self {
            Foreground::Stock => "stock".to_owned(),
            Foreground::Home => "home".to_owned(),
            Foreground::App(id) => format!("app:{id}"),
        }
    }
}

impl fmt::Display for Foreground {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// How a supervised process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// Exited zero, of its own accord.
    Clean,
    /// Exited non-zero.
    Error,
    /// Killed by a signal — a panic-driven abort, a segfault, an OOM kill.
    Signal,
    /// We killed it because it missed a deadline.
    ForcedByDeadline,
}

impl ExitKind {
    /// Whether this exit is a failure the ledger should count.
    fn is_failure(self) -> bool {
        !matches!(self, ExitKind::Clean)
    }
}

/// Something that happened, reported to the machine.
///
/// Every variant is an *observation*. There is deliberately no
/// `LaunchSucceeded`: the machine decides that from readiness and display
/// ownership, because the supervisor is not entitled to that opinion.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// Someone asked for a different foreground owner.
    SwitchRequested {
        /// Where to.
        to: Foreground,
    },
    /// The incoming foreground process reported itself ready.
    ProcessReady {
        /// Which owner reported it.
        owner: Foreground,
    },
    /// Display ownership was observed to have moved.
    DisplayOwned {
        /// Who holds it now.
        owner: Foreground,
    },
    /// The panel was observed to be released by Paperclip.
    DisplayReleased,
    /// Stock Xochitl was observed running and active.
    StockRunning,
    /// A supervised foreground session ended.
    SessionExited {
        /// Which owner.
        owner: Foreground,
        /// How it ended.
        status: ExitKind,
    },
    /// The foreground session is alive but its main loop stopped advancing.
    ///
    /// Distinct from [`Event::SessionExited`] on purpose: this is the hang
    /// row, and a process that is merely breathing does not clear it.
    ProgressStalled {
        /// Which owner.
        owner: Foreground,
        /// How long since the last main-loop tick.
        stalled_for: Duration,
    },
    /// The process that owns the panel on our behalf died.
    DisplayHostLost,
    /// An attempt to restore stock did not work.
    RestoreFailed {
        /// What went wrong, phrased for a human holding the tablet.
        reason: String,
    },
    /// Wall-clock advanced; check deadlines.
    Tick,
}

/// How hard to try when stopping a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Escalation {
    /// How long the session gets after `SIGTERM` before `SIGKILL`.
    pub grace: Duration,
    /// How long the tree gets to empty after `SIGKILL` before the supervisor
    /// reports the containment itself as failed.
    pub force: Duration,
}

/// Why a session is being stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// A switch to another owner.
    Switching,
    /// It crashed and the remainder of its tree has to go with it.
    Crashed,
    /// It stopped making progress.
    Hung,
    /// The display host underneath it died.
    DisplayLost,
    /// A switch did not complete inside its deadline.
    SwitchTimedOut,
}

impl StopReason {
    /// A short phrase for logs and the diagnostics bundle.
    pub fn description(self) -> &'static str {
        match self {
            StopReason::Switching => "switching foreground owner",
            StopReason::Crashed => "foreground session crashed",
            StopReason::Hung => "foreground session stopped making progress",
            StopReason::DisplayLost => "display host died under the session",
            StopReason::SwitchTimedOut => "switch did not complete inside its deadline",
        }
    }
}

/// Why the machine gave up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnosis {
    /// Stock could not be restored.
    StockUnavailable {
        /// The last reason a restore attempt gave.
        last_reason: String,
        /// How many attempts were made.
        attempts: u32,
    },
    /// The foreground kept failing, so the machine stopped relaunching it.
    RepeatedSessionFailures {
        /// How many failures inside the window.
        failures: u32,
        /// The window they fell inside.
        window: Duration,
    },
}

impl Diagnosis {
    /// One line, written for someone holding a tablet that is not behaving.
    pub fn summary(&self) -> String {
        match self {
            Diagnosis::StockUnavailable {
                last_reason,
                attempts,
            } => format!(
                "stock Xochitl could not be restored after {attempts} attempt(s): {last_reason}"
            ),
            Diagnosis::RepeatedSessionFailures { failures, window } => format!(
                "the foreground session failed {failures} times within {}s; not retrying",
                window.as_secs()
            ),
        }
    }
}

/// What the runtime must do next.
///
/// The machine never performs anything itself, so this list is the complete
/// set of side effects the supervisor is allowed to have.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Start the incoming foreground owner and wait for readiness.
    StartForeground {
        /// Who to start.
        target: Foreground,
        /// When to give up waiting.
        deadline: Instant,
    },
    /// Stop the current session and empty its process tree.
    TerminateSession {
        /// Why.
        reason: StopReason,
        /// How hard, and how long each stage gets.
        escalation: Escalation,
    },
    /// Hand the panel back: release locks and the wakelock owner record.
    ReleaseDisplay,
    /// Run the independent stock restore path.
    RestoreStock {
        /// 1-based attempt counter, so the runtime can space retries under
        /// Xochitl's `StartLimitBurst=4` inside 600s.
        attempt: u32,
        /// When to give up waiting for stock to come back.
        deadline: Instant,
    },
    /// Write a diagnostics bundle before anything else destroys the evidence.
    CaptureDiagnostics {
        /// What was happening.
        reason: StopReason,
    },
    /// Stop. Do not retry. Leave the diagnostics in place.
    Halt {
        /// Why, in terms a human can act on.
        diagnosis: Diagnosis,
    },
}

/// Deadlines, in one place, each with a reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Ready *and* owning the display, from the moment a switch begins.
    pub switch: Duration,
    /// `SIGTERM` to `SIGKILL`.
    pub graceful_stop: Duration,
    /// `SIGKILL` to an empty tree.
    pub force_stop: Duration,
    /// Restore start to stock observed active.
    pub restore: Duration,
    /// Main-loop tick age that counts as hung.
    pub progress_stall: Duration,
}

impl Default for Budget {
    /// Values chosen against measured device behaviour, not taste.
    ///
    /// * `switch` — WWW-20 measured ~77 ms of kernel bridge reprogramming
    ///   after every resume before the panel can be presented to, and a
    ///   session may start straight out of one. 8 s leaves room for a cold
    ///   start plus several resumes without ever being long enough that a
    ///   person decides the tablet is dead.
    /// * `graceful_stop` — long enough for a save-and-return-to-stock write
    ///   (§5), short enough that a hang does not look like a lock-up.
    /// * `force_stop` — a `SIGKILL` sweep with no freezer available has to
    ///   re-read `cgroup.procs` a few times; this is the budget for that loop,
    ///   not for one signal.
    /// * `restore` — Xochitl unlocks dm-crypt and mounts `/home` on start
    ///   (WWW-1 §5), so it is genuinely slow. Under-budgeting here would make
    ///   the supervisor declare a working restore failed.
    /// * `progress_stall` — three refreshes' worth. Below that, a slow
    ///   waveform would read as a hang.
    fn default() -> Self {
        Self {
            switch: Duration::from_secs(8),
            graceful_stop: Duration::from_secs(5),
            force_stop: Duration::from_secs(5),
            restore: Duration::from_secs(30),
            progress_stall: Duration::from_secs(6),
        }
    }
}

impl Budget {
    /// The escalation ladder these deadlines imply.
    pub fn escalation(self) -> Escalation {
        Escalation {
            grace: self.graceful_stop,
            force: self.force_stop,
        }
    }
}

/// How many times to try before stopping.
///
/// The restore number is not a tuning preference, and it is 1 rather than 2
/// for a specific reason: **the retry lives one layer down.**
/// `paper_device::stock::Stock::restore` already does two guarded starts and
/// then refuses, so a host-level retry multiplies rather than adds — two
/// attempts here would be four starts, which is exactly Xochitl's
/// `StartLimitBurst=4` inside 600 s. Crossing that fails the unit, and its
/// `OnFailure=` lands on `emergency.target` whose companion unit does not
/// exist on this image: a tablet on a serial console with no screen.
///
/// So the host asks once and believes the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailurePolicy {
    /// Restore attempts before [`SessionState::Failed`].
    pub restore_attempts: u32,
    /// Foreground failures inside `failure_window` before the machine stops
    /// relaunching and goes back to stock for good.
    pub session_failures: u32,
    /// The window `session_failures` is counted over.
    pub failure_window: Duration,
}

impl Default for FailurePolicy {
    fn default() -> Self {
        Self {
            restore_attempts: 1,
            session_failures: 3,
            failure_window: Duration::from_secs(120),
        }
    }
}

/// A switch that has begun and not yet arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    target: Foreground,
    deadline: Instant,
    process_ready: bool,
    display_owned: bool,
    stock_running: bool,
    display_released: bool,
}

impl Pending {
    fn new(target: Foreground, deadline: Instant) -> Self {
        Self {
            target,
            deadline,
            process_ready: false,
            display_owned: false,
            stock_running: false,
            display_released: false,
        }
    }

    /// Whether everything this switch promised has actually been observed.
    ///
    /// Two clauses for a Paperclip owner, two for stock. A process that
    /// started and never took the panel does not satisfy either.
    fn arrived(&self) -> bool {
        match self.target {
            Foreground::Stock => self.stock_running && self.display_released,
            _ => self.process_ready && self.display_owned,
        }
    }
}

/// A recovery in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recovery {
    attempt: u32,
    deadline: Instant,
    last_reason: String,
}

/// Recent foreground failures, for the "repeated failures" row of §10.
#[derive(Debug, Clone, Default)]
struct Ledger {
    failures: Vec<Instant>,
}

impl Ledger {
    fn record(&mut self, at: Instant, window: Duration) -> u32 {
        self.failures.push(at);
        self.failures
            .retain(|seen| at.duration_since(*seen) <= window);
        u32::try_from(self.failures.len()).unwrap_or(u32::MAX)
    }

    fn clear(&mut self) {
        self.failures.clear();
    }
}

/// The foreground state machine.
#[derive(Debug)]
pub struct Machine {
    state: SessionState,
    foreground: Foreground,
    budget: Budget,
    policy: FailurePolicy,
    pending: Option<Pending>,
    recovery: Option<Recovery>,
    stopping: Option<Instant>,
    ledger: Ledger,
    diagnosis: Option<Diagnosis>,
}

impl Machine {
    /// A machine that believes stock owns the display, which is what a fresh
    /// boot looks like and what §10 requires a reboot to return to.
    pub fn new(budget: Budget, policy: FailurePolicy) -> Self {
        Self {
            state: SessionState::Stock,
            foreground: Foreground::Stock,
            budget,
            policy,
            pending: None,
            recovery: None,
            stopping: None,
            ledger: Ledger::default(),
            diagnosis: None,
        }
    }

    /// The current state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The confirmed foreground owner. A switch in flight has not changed it.
    pub fn foreground(&self) -> &Foreground {
        &self.foreground
    }

    /// Why the machine halted, if it has.
    pub fn diagnosis(&self) -> Option<&Diagnosis> {
        self.diagnosis.as_ref()
    }

    /// The next moment at which [`Event::Tick`] could change anything.
    ///
    /// The runtime sleeps until this rather than polling, so a deadline is a
    /// real deadline and not a scan interval.
    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.pending.as_ref().map(|p| p.deadline),
            self.recovery.as_ref().map(|r| r.deadline),
            self.stopping
                .map(|since| since + self.budget.graceful_stop + self.budget.force_stop),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Feeds an observation in and gets the side effects back.
    pub fn handle(&mut self, event: &Event, now: Instant) -> Vec<Action> {
        // Terminal means terminal. Recording the event is fine; acting on it
        // is exactly the restart loop §10 forbids.
        if self.state == SessionState::Failed {
            return Vec::new();
        }
        match event {
            Event::SwitchRequested { to } => self.begin_switch(to.clone(), now),
            Event::ProcessReady { owner } => self.observe(now, |p| {
                if &p.target == owner {
                    p.process_ready = true;
                }
            }),
            Event::DisplayOwned { owner } => self.observe(now, |p| {
                if &p.target == owner {
                    p.display_owned = true;
                }
            }),
            Event::DisplayReleased => self.observe(now, |p| p.display_released = true),
            Event::StockRunning => self.stock_running(now),
            Event::SessionExited { owner, status } => self.session_exited(owner, *status, now),
            Event::ProgressStalled { owner, .. } => self.progress_stalled(owner, now),
            Event::DisplayHostLost => self.display_host_lost(now),
            Event::RestoreFailed { reason } => self.restore_failed(reason, now),
            Event::Tick => self.tick(now),
        }
    }

    /// Clears a terminal state, by explicit human instruction only.
    ///
    /// Separate from [`Machine::handle`] because no observation should ever be
    /// able to do this — that separation is what makes "stop retrying" hold.
    /// `paperctl stock --force` is the caller.
    ///
    /// From [`SessionState::Failed`] it starts a fresh recovery. From a spent
    /// failure budget it simply clears the budget, because the device is
    /// already at stock and there is nothing to recover.
    pub fn operator_retry(&mut self, now: Instant) -> Vec<Action> {
        let was_failed = self.state == SessionState::Failed;
        self.diagnosis = None;
        self.ledger.clear();
        if was_failed {
            self.enter_recovery(StopReason::Crashed, now, None)
        } else {
            Vec::new()
        }
    }

    fn begin_switch(&mut self, to: Foreground, now: Instant) -> Vec<Action> {
        if matches!(self.state, SessionState::Recovering) {
            // A switch request during recovery is ignored rather than queued:
            // a queue here is how two owners end up racing for the panel.
            return Vec::new();
        }
        if !matches!(to, Foreground::Stock) && !self.may_relaunch() {
            // The failure budget is spent. §10 says stop retrying, and a
            // budget that only records its own exhaustion while still
            // honouring the next request is a restart loop with extra steps.
            // Stock is where this stays until a human says otherwise.
            return Vec::new();
        }
        if self.foreground == to && self.pending.is_none() {
            return Vec::new();
        }
        let deadline = now + self.budget.switch;
        let mut actions = Vec::new();
        if self.state.owns_display() {
            actions.push(Action::TerminateSession {
                reason: StopReason::Switching,
                escalation: self.budget.escalation(),
            });
            self.stopping = Some(now);
        }
        if matches!(to, Foreground::Stock) {
            actions.push(Action::ReleaseDisplay);
        }
        actions.push(Action::StartForeground {
            target: to.clone(),
            deadline,
        });
        self.pending = Some(Pending::new(to, deadline));
        self.state = SessionState::Switching;
        actions
    }

    fn observe(&mut self, now: Instant, mark: impl FnOnce(&mut Pending)) -> Vec<Action> {
        let Some(pending) = self.pending.as_mut() else {
            return Vec::new();
        };
        mark(pending);
        self.settle(now)
    }

    fn stock_running(&mut self, now: Instant) -> Vec<Action> {
        if self.state == SessionState::Recovering {
            self.recovery = None;
            self.stopping = None;
            self.pending = None;
            self.state = SessionState::Stock;
            self.foreground = Foreground::Stock;
            return Vec::new();
        }
        self.observe(now, |p| p.stock_running = true)
    }

    /// Completes a switch, but only when every clause has been observed.
    fn settle(&mut self, _now: Instant) -> Vec<Action> {
        let Some(pending) = self.pending.as_ref() else {
            return Vec::new();
        };
        if !pending.arrived() {
            return Vec::new();
        }
        self.foreground = pending.target.clone();
        self.state = pending.target.arrived_state();
        self.pending = None;
        self.stopping = None;
        if self.state == SessionState::Stock {
            self.ledger.clear();
        }
        Vec::new()
    }

    fn session_exited(
        &mut self,
        owner: &Foreground,
        status: ExitKind,
        now: Instant,
    ) -> Vec<Action> {
        let ours =
            &self.foreground == owner || self.pending.as_ref().is_some_and(|p| &p.target == owner);
        if !ours || matches!(owner, Foreground::Stock) {
            return Vec::new();
        }
        if !status.is_failure() {
            // A clean exit is a request to go back to stock, not a failure.
            // It still goes through a full switch, deadline and all.
            return self.begin_switch(Foreground::Stock, now);
        }
        let failures = self.ledger.record(now, self.policy.failure_window);
        let exhausted = failures >= self.policy.session_failures;
        let mut actions = self.enter_recovery(StopReason::Crashed, now, None);
        if exhausted {
            // Still recover — the panel must go back — but record that the
            // foreground is not to be relaunched afterwards.
            self.diagnosis = Some(Diagnosis::RepeatedSessionFailures {
                failures,
                window: self.policy.failure_window,
            });
            actions.push(Action::CaptureDiagnostics {
                reason: StopReason::Crashed,
            });
        }
        actions
    }

    fn progress_stalled(&mut self, owner: &Foreground, now: Instant) -> Vec<Action> {
        if &self.foreground != owner || !self.state.owns_display() {
            return Vec::new();
        }
        self.enter_recovery(StopReason::Hung, now, None)
    }

    fn display_host_lost(&mut self, now: Instant) -> Vec<Action> {
        if matches!(self.state, SessionState::Stock | SessionState::Failed) {
            return Vec::new();
        }
        self.enter_recovery(StopReason::DisplayLost, now, None)
    }

    fn restore_failed(&mut self, reason: &str, now: Instant) -> Vec<Action> {
        let attempt = self.recovery.as_ref().map_or(1, |r| r.attempt);
        if attempt >= self.policy.restore_attempts {
            let diagnosis = Diagnosis::StockUnavailable {
                last_reason: reason.to_owned(),
                attempts: attempt,
            };
            return self.halt(diagnosis);
        }
        let next = attempt + 1;
        let deadline = now + self.budget.restore;
        self.recovery = Some(Recovery {
            attempt: next,
            deadline,
            last_reason: reason.to_owned(),
        });
        vec![Action::RestoreStock {
            attempt: next,
            deadline,
        }]
    }

    fn tick(&mut self, now: Instant) -> Vec<Action> {
        if let Some(pending) = self.pending.as_ref()
            && now >= pending.deadline
        {
            return self.enter_recovery(StopReason::SwitchTimedOut, now, None);
        }
        if let Some(recovery) = self.recovery.as_ref()
            && now >= recovery.deadline
        {
            let reason = format!(
                "stock did not come back inside {}s",
                self.budget.restore.as_secs()
            );
            return self.restore_failed(&reason, now);
        }
        Vec::new()
    }

    /// The single door into recovery, so every path through it is identical.
    fn enter_recovery(
        &mut self,
        reason: StopReason,
        now: Instant,
        already_stopped: Option<()>,
    ) -> Vec<Action> {
        if self.state == SessionState::Recovering {
            return Vec::new();
        }
        self.state = SessionState::Recovering;
        self.pending = None;
        self.stopping = None;
        let deadline = now + self.budget.restore;
        self.recovery = Some(Recovery {
            attempt: 1,
            deadline,
            last_reason: String::new(),
        });
        let mut actions = vec![Action::CaptureDiagnostics { reason }];
        if already_stopped.is_none() {
            // Terminate first, always. §10 asks for the *remaining* app
            // processes to go, which is the part a crashed process cannot do
            // for itself — and the reason none of this hangs off a signal
            // handler or a destructor.
            actions.push(Action::TerminateSession {
                reason,
                escalation: self.budget.escalation(),
            });
        }
        actions.push(Action::ReleaseDisplay);
        actions.push(Action::RestoreStock {
            attempt: 1,
            deadline,
        });
        actions
    }

    fn halt(&mut self, diagnosis: Diagnosis) -> Vec<Action> {
        self.state = SessionState::Failed;
        self.recovery = None;
        self.pending = None;
        self.stopping = None;
        self.diagnosis = Some(diagnosis.clone());
        vec![Action::Halt { diagnosis }]
    }

    /// Whether the foreground may be relaunched after this recovery.
    ///
    /// False once the repeated-failure budget is spent: the machine goes back
    /// to stock and stays there. Stock is a good place to be stuck.
    pub fn may_relaunch(&self) -> bool {
        !matches!(
            self.diagnosis,
            Some(Diagnosis::RepeatedSessionFailures { .. })
        ) && self.state != SessionState::Failed
    }
}
