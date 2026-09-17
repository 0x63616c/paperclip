//! The §10 table as decisions, and the §11 table as claims.
//!
//! **What this suite proves:** that the state machine reaches the right state
//! for each failure, that the generated units contain only directives the
//! target platform enforces, and that the isolation report refuses to claim
//! what is not in force.
//!
//! **What it does not prove, and must never be cited for:** that any of it is
//! enforced. These tests run on a developer's Mac. They send no signals, start
//! no units, and touch no device. The enforcement evidence is
//! `tests/failure-harness`, run in an aarch64 Linux VM with systemd — and a
//! test here asserts that this suite cannot quietly become the harness.

use std::time::{Duration, Instant, SystemTime};

use paper_host::facilities::{CgroupLayout, Controller, Facilities, FacilitySource, SandboxTool};
use paper_host::progress::{MainLoopProgress, Progress, ProgressWatch};
use paper_host::report::{IsolationReport, Mechanism, Verdict};
use paper_host::state::{
    Action, Budget, Diagnosis, Event, ExitKind, FailurePolicy, Foreground, Machine, SessionState,
    StopReason,
};
use paper_host::units::{
    APP_UNIT, HOST_UNIT, RESTORE_UNIT, SessionGrants, SessionSpec, UnitSet, XOCHITL_UNIT,
};
use paper_packages::{AppId, Capability, InstallPolicy, InstalledApp, Manifest};

fn machine() -> Machine {
    Machine::new(Budget::default(), FailurePolicy::default())
}

fn chess() -> AppId {
    "dev.calum.chess".parse().expect("a valid app id")
}

/// Drives a machine from `Stock` to a running `App`, the way the runtime does.
fn arrive_at_app(machine: &mut Machine, now: Instant) {
    let app = Foreground::App(chess());
    machine.handle(&Event::SwitchRequested { to: app.clone() }, now);
    machine.handle(&Event::ProcessReady { owner: app.clone() }, now);
    machine.handle(&Event::DisplayOwned { owner: app }, now);
    assert_eq!(machine.state(), SessionState::App);
}

fn restored(machine: &mut Machine, now: Instant) {
    machine.handle(&Event::StockRunning, now);
    machine.handle(&Event::DisplayReleased, now);
}

// --- transitions -----------------------------------------------------------

#[test]
fn a_started_process_has_not_arrived_until_it_owns_the_display() {
    let now = Instant::now();
    let mut machine = machine();
    let home = Foreground::Home;

    machine.handle(&Event::SwitchRequested { to: home.clone() }, now);
    assert_eq!(machine.state(), SessionState::Switching);

    machine.handle(
        &Event::ProcessReady {
            owner: home.clone(),
        },
        now,
    );
    assert_eq!(
        machine.state(),
        SessionState::Switching,
        "readiness alone completed the switch; starting a process is not success"
    );
    assert_eq!(machine.foreground(), &Foreground::Stock);

    machine.handle(
        &Event::DisplayOwned {
            owner: home.clone(),
        },
        now,
    );
    assert_eq!(machine.state(), SessionState::Home);
    assert_eq!(machine.foreground(), &home);
}

#[test]
fn display_ownership_without_readiness_is_not_arrival_either() {
    let now = Instant::now();
    let mut machine = machine();
    let home = Foreground::Home;
    machine.handle(&Event::SwitchRequested { to: home.clone() }, now);
    machine.handle(&Event::DisplayOwned { owner: home }, now);
    assert_eq!(machine.state(), SessionState::Switching);
}

#[test]
fn a_switch_that_never_completes_expires_into_recovery() {
    let now = Instant::now();
    let budget = Budget::default();
    let mut machine = machine();
    machine.handle(
        &Event::SwitchRequested {
            to: Foreground::Home,
        },
        now,
    );

    assert!(
        machine
            .handle(&Event::Tick, now + budget.switch / 2)
            .is_empty()
    );

    let actions = machine.handle(&Event::Tick, now + budget.switch + Duration::from_millis(1));
    assert_eq!(machine.state(), SessionState::Recovering);
    assert!(actions.iter().any(|action| matches!(
        action,
        Action::CaptureDiagnostics {
            reason: StopReason::SwitchTimedOut
        }
    )));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::RestoreStock { attempt: 1, .. }))
    );
}

#[test]
fn only_one_owner_holds_the_foreground_at_a_time() {
    let now = Instant::now();
    let mut machine = machine();
    arrive_at_app(&mut machine, now);

    let actions = machine.handle(
        &Event::SwitchRequested {
            to: Foreground::Home,
        },
        now,
    );
    assert!(
        actions.iter().any(|action| matches!(
            action,
            Action::TerminateSession {
                reason: StopReason::Switching,
                ..
            }
        )),
        "the outgoing session must be stopped before the incoming one arrives"
    );
    assert_eq!(machine.state(), SessionState::Switching);
    assert_eq!(
        machine.foreground(),
        &Foreground::App(chess()),
        "the foreground moved before the incoming owner arrived"
    );
}

// --- the §10 table ---------------------------------------------------------

#[test]
fn row_app_crash_terminates_the_rest_then_releases_and_restores() {
    let now = Instant::now();
    let mut machine = machine();
    arrive_at_app(&mut machine, now);

    let actions = machine.handle(
        &Event::SessionExited {
            owner: Foreground::App(chess()),
            status: ExitKind::Signal,
        },
        now,
    );

    let order: Vec<&str> = actions
        .iter()
        .map(|action| match action {
            Action::CaptureDiagnostics { .. } => "capture",
            Action::TerminateSession { .. } => "terminate",
            Action::ReleaseDisplay => "release",
            Action::RestoreStock { .. } => "restore",
            _ => "other",
        })
        .collect();
    assert_eq!(order, vec!["capture", "terminate", "release", "restore"]);
    assert_eq!(machine.state(), SessionState::Recovering);

    restored(&mut machine, now);
    assert_eq!(machine.state(), SessionState::Stock);
}

#[test]
fn row_app_hang_is_reached_by_progress_not_by_liveness() {
    let now = Instant::now();
    let mut machine = machine();
    arrive_at_app(&mut machine, now);

    let actions = machine.handle(
        &Event::ProgressStalled {
            owner: Foreground::App(chess()),
            stalled_for: Duration::from_secs(9),
        },
        now,
    );
    assert_eq!(machine.state(), SessionState::Recovering);

    let escalation = actions
        .iter()
        .find_map(|action| match action {
            Action::TerminateSession { escalation, .. } => Some(*escalation),
            _ => None,
        })
        .expect("a hang must produce a bounded termination");
    assert!(escalation.grace > Duration::ZERO, "no graceful stage");
    assert!(escalation.force > Duration::ZERO, "no bounded force stage");
}

#[test]
fn row_display_process_crash_terminates_the_dependent_session() {
    let now = Instant::now();
    let mut machine = machine();
    arrive_at_app(&mut machine, now);

    let actions = machine.handle(&Event::DisplayHostLost, now);
    assert_eq!(machine.state(), SessionState::Recovering);
    assert!(actions.iter().any(|action| matches!(
        action,
        Action::TerminateSession {
            reason: StopReason::DisplayLost,
            ..
        }
    )));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::RestoreStock { .. }))
    );
}

#[test]
fn row_repeated_failures_stops_retrying_and_stays_at_stock() {
    let now = Instant::now();
    let policy = FailurePolicy::default();
    let mut machine = Machine::new(Budget::default(), policy);

    for attempt in 1..=policy.session_failures {
        let at = now + Duration::from_secs(u64::from(attempt));
        arrive_at_app(&mut machine, at);
        machine.handle(
            &Event::SessionExited {
                owner: Foreground::App(chess()),
                status: ExitKind::Signal,
            },
            at,
        );
        restored(&mut machine, at);
        assert_eq!(machine.state(), SessionState::Stock);
    }

    assert!(
        !machine.may_relaunch(),
        "the machine is still willing to relaunch after its failure budget"
    );
    assert!(matches!(
        machine.diagnosis(),
        Some(Diagnosis::RepeatedSessionFailures { .. })
    ));

    // Recording exhaustion is not enough. A budget that still honours the
    // next request is a restart loop with extra steps.
    let later = now + Duration::from_secs(10);
    assert!(
        machine
            .handle(
                &Event::SwitchRequested {
                    to: Foreground::App(chess())
                },
                later
            )
            .is_empty(),
        "a request after the budget was honoured"
    );
    assert_eq!(machine.state(), SessionState::Stock);

    // A human can still say "try anyway", and only a human.
    machine.operator_retry(later);
    assert!(machine.may_relaunch());
    assert!(
        !machine
            .handle(
                &Event::SwitchRequested {
                    to: Foreground::Home
                },
                later
            )
            .is_empty()
    );
}

#[test]
fn row_xochitl_fails_to_start_reaches_failed_and_never_claims_recovery() {
    let now = Instant::now();
    let policy = FailurePolicy::default();
    let mut machine = Machine::new(Budget::default(), policy);
    arrive_at_app(&mut machine, now);
    machine.handle(&Event::DisplayHostLost, now);

    let mut actions = Vec::new();
    for attempt in 0..policy.restore_attempts {
        actions = machine.handle(
            &Event::RestoreFailed {
                reason: format!("attempt {attempt}: stock did not become active"),
            },
            now,
        );
    }

    assert_eq!(machine.state(), SessionState::Failed);
    let halt = actions
        .iter()
        .find_map(|action| match action {
            Action::Halt { diagnosis } => Some(diagnosis),
            _ => None,
        })
        .expect("running out of restore attempts must halt");
    assert!(matches!(halt, Diagnosis::StockUnavailable { .. }));
    assert!(halt.summary().contains("could not be restored"));
}

#[test]
fn failed_is_terminal_and_only_a_human_leaves_it() {
    let now = Instant::now();
    let mut machine = Machine::new(
        Budget::default(),
        FailurePolicy {
            restore_attempts: 1,
            ..FailurePolicy::default()
        },
    );
    arrive_at_app(&mut machine, now);
    machine.handle(&Event::DisplayHostLost, now);
    machine.handle(
        &Event::RestoreFailed {
            reason: "stock is not coming back".to_owned(),
        },
        now,
    );
    assert_eq!(machine.state(), SessionState::Failed);

    for event in [
        Event::Tick,
        Event::StockRunning,
        Event::SwitchRequested {
            to: Foreground::Home,
        },
        Event::DisplayHostLost,
        Event::SessionExited {
            owner: Foreground::Home,
            status: ExitKind::Signal,
        },
    ] {
        assert!(
            machine
                .handle(&event, now + Duration::from_secs(60))
                .is_empty(),
            "{event:?} produced an action from the terminal state; that is a restart loop"
        );
        assert_eq!(machine.state(), SessionState::Failed);
    }

    let actions = machine.operator_retry(now + Duration::from_secs(120));
    assert_eq!(machine.state(), SessionState::Recovering);
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::RestoreStock { attempt: 1, .. }))
    );
}

#[test]
fn a_clean_exit_is_a_request_for_stock_not_a_failure() {
    let now = Instant::now();
    let mut machine = machine();
    arrive_at_app(&mut machine, now);

    let actions = machine.handle(
        &Event::SessionExited {
            owner: Foreground::App(chess()),
            status: ExitKind::Clean,
        },
        now,
    );
    assert_eq!(machine.state(), SessionState::Switching);
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::CaptureDiagnostics { .. })),
        "a clean exit captured a failure bundle"
    );
    restored(&mut machine, now);
    assert_eq!(machine.state(), SessionState::Stock);
    assert!(machine.may_relaunch());
}

#[test]
fn a_restore_that_never_lands_expires_rather_than_hanging() {
    let now = Instant::now();
    let budget = Budget::default();
    let mut machine = Machine::new(
        budget,
        FailurePolicy {
            restore_attempts: 2,
            ..FailurePolicy::default()
        },
    );
    arrive_at_app(&mut machine, now);
    machine.handle(&Event::DisplayHostLost, now);

    let actions = machine.handle(
        &Event::Tick,
        now + budget.restore + Duration::from_millis(1),
    );
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::RestoreStock { attempt: 2, .. })),
        "a restore with no answer must expire into a second attempt, not wait forever"
    );
}

// --- progress, not liveness ------------------------------------------------

#[test]
fn a_stalled_main_loop_is_distinguishable_from_a_live_process() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("progress");
    let mut published = MainLoopProgress::new(path.clone());
    let mut watch = ProgressWatch::new(path);
    let stall = Duration::from_secs(5);
    let start = SystemTime::now();

    published.tick().expect("first tick publishes");
    assert!(matches!(
        watch.poll(start, stall).expect("a readable witness"),
        Progress::Advancing { .. }
    ));

    // The process is still very much alive here. Nothing ticks.
    assert!(matches!(
        watch
            .poll(start + Duration::from_secs(1), stall)
            .expect("a readable witness"),
        Progress::Quiet { .. }
    ));
    assert!(matches!(
        watch
            .poll(start + Duration::from_secs(6), stall)
            .expect("a readable witness"),
        Progress::Stalled { .. }
    ));

    published.tick().expect("the loop recovers");
    assert!(matches!(
        watch
            .poll(start + Duration::from_secs(7), stall)
            .expect("a readable witness"),
        Progress::Advancing { .. }
    ));
}

#[test]
fn a_missing_witness_is_never_read_as_progress() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let mut watch = ProgressWatch::new(directory.path().join("absent"));
    let start = SystemTime::now();
    assert!(matches!(
        watch.poll(start, Duration::from_secs(1)).expect("no error"),
        Progress::Quiet { .. }
    ));
    assert!(matches!(
        watch
            .poll(start + Duration::from_secs(2), Duration::from_secs(1))
            .expect("no error"),
        Progress::Stalled { .. }
    ));
}

// --- unit generation -------------------------------------------------------

fn installed(capabilities: &[Capability]) -> InstalledApp {
    let manifest = Manifest::parse(
        r#"
        [app]
        id = "dev.calum.chess"
        name = "Chess"
        version = "0.1.0"
        protocol = "1.0"
        entrypoint = "bin/chess"
        "#,
    )
    .expect("a valid manifest");
    let mut policy = InstallPolicy::deny_all();
    for capability in capabilities {
        policy.allow(manifest.id(), *capability);
    }
    InstalledApp::install(manifest, &policy)
}

fn units_for(facilities: &Facilities, capabilities: &[Capability]) -> UnitSet {
    let spec = SessionSpec::device();
    let app = installed(capabilities);
    let grants = SessionGrants::derive(app.manifest().id(), app.capabilities(), &spec.paths);
    UnitSet::plan(&spec, &grants, facilities)
}

#[test]
fn no_generated_unit_is_ever_installable() {
    // The reboot row of §10: stock startup stays the default because there is
    // nothing to enable. If an `[Install]` section appears here, a reboot can
    // come up into a custom foreground, and the guarantee is gone.
    let units = units_for(&Facilities::paper_pro(), &[Capability::Storage]);
    for file in &units.files {
        assert!(
            !file.contents.lines().any(|line| line.trim() == "[Install]"),
            "{} carries an [Install] section",
            file.name
        );
        assert!(
            !file.contents.contains("WantedBy="),
            "{} can be enabled",
            file.name
        );
    }
}

#[test]
fn the_device_never_gets_a_memory_limit_that_does_nothing() {
    let units = units_for(&Facilities::paper_pro(), &[Capability::Storage]);
    let app = units.get(APP_UNIT).expect("an app unit");

    assert!(
        !app.contents
            .lines()
            .any(|line| line.trim_start().starts_with("MemoryMax=")),
        "MemoryMax= reached a Paper Pro unit, where it is accepted and enforces nothing"
    );
    assert!(
        !app.contents
            .lines()
            .any(|line| line.trim_start().starts_with("MemoryAccounting=")),
        "MemoryAccounting= reached a Paper Pro unit; it reports MemoryCurrent=[not set]"
    );
    assert!(
        app.contents.contains("LimitAS="),
        "no memory bound of any kind was written"
    );
}

#[test]
fn a_machine_with_a_delegated_memory_controller_does_get_one() {
    let mut facilities = Facilities::paper_pro();
    facilities.memory = Controller::Delegated;
    let units = units_for(&facilities, &[Capability::Storage]);
    let app = units.get(APP_UNIT).expect("an app unit");
    assert!(app.contents.contains("MemoryMax="));
    assert!(
        app.contents.contains("LimitAS="),
        "the rlimit should still be there as a per-process backstop"
    );
}

#[test]
fn seccomp_directives_are_written_only_where_systemd_has_seccomp() {
    let device = units_for(&Facilities::paper_pro(), &[]);
    assert!(
        !device
            .get(APP_UNIT)
            .expect("an app unit")
            .contents
            .lines()
            .any(|line| line.trim_start().starts_with("SystemCallFilter=")),
        "systemd is built without seccomp on the device; the directive would be ignored"
    );

    let mut with_seccomp = Facilities::paper_pro();
    with_seccomp.systemd_seccomp = true;
    let vm = units_for(&with_seccomp, &[]);
    assert!(
        vm.get(APP_UNIT)
            .expect("an app unit")
            .contents
            .contains("SystemCallFilter=@system-service")
    );
}

/// Every `ReadWritePaths=` in a unit, in order.
fn writable_paths(unit: &str) -> Vec<&str> {
    unit.lines()
        .filter_map(|line| line.trim_start().strip_prefix("ReadWritePaths="))
        .collect()
}

#[test]
fn a_session_with_no_storage_grant_can_write_only_its_progress_witness() {
    let units = units_for(&Facilities::paper_pro(), &[]);
    let app = units.get(APP_UNIT).expect("an app unit");
    // Exactly one, and it is the supervisor's own channel rather than
    // anything the app asked for: ProtectSystem=strict makes /run read-only,
    // and a session that cannot publish progress is indistinguishable from a
    // session that has hung.
    assert_eq!(
        writable_paths(&app.contents),
        vec!["/run/paperclip/progress", "/tmp"],
        "an app with no storage capability was given somewhere else to write"
    );
    assert!(app.contents.contains("ProtectSystem=strict"));
    assert!(app.contents.contains("InaccessiblePaths=-/data"));
}

#[test]
fn a_storage_grant_opens_exactly_one_directory() {
    let units = units_for(&Facilities::paper_pro(), &[Capability::Storage]);
    let app = units.get(APP_UNIT).expect("an app unit");
    let writable = writable_paths(&app.contents);
    assert_eq!(
        writable,
        vec![
            "/home/root/paperclip/storage/dev.calum.chess",
            "/run/paperclip/progress",
            "/tmp"
        ],
        "a storage grant should add one directory, named for the app, and nothing else"
    );
}

#[test]
fn network_denial_is_written_only_where_a_mechanism_exists() {
    // The device is the awkward case and the reason this is a test rather than
    // a line of code: `IPAddressDeny=` is BPF, `RestrictAddressFamilies=` is
    // seccomp, and WWW-11 established that this systemd has no seccomp and
    // said nothing about BPF. So neither is written, and §11 reports that an
    // ungranted app can still open a socket rather than implying a boundary.
    let device = units_for(&Facilities::paper_pro(), &[]);
    let unit = &device.get(APP_UNIT).expect("an app unit").contents;
    assert!(
        !unit
            .lines()
            .any(|line| line.trim_start().starts_with("IPAddressDeny=")),
        "IPAddressDeny= was written without establishing that systemd has BPF"
    );
    assert!(
        !unit
            .lines()
            .any(|line| line.trim_start().starts_with("RestrictAddressFamilies=")),
        "RestrictAddressFamilies= was written on a systemd built without seccomp"
    );
    assert_eq!(
        IsolationReport::derive(&Facilities::paper_pro())
            .finding(Mechanism::NetworkReach)
            .expect("a network row")
            .verdict,
        Verdict::Ineffective
    );

    let mut with_bpf = Facilities::paper_pro();
    with_bpf.systemd_bpf = Some(true);
    let denied = units_for(&with_bpf, &[]);
    assert!(
        denied
            .get(APP_UNIT)
            .expect("an app unit")
            .contents
            .contains("IPAddressDeny=any")
    );
    let granted = units_for(&with_bpf, &[Capability::Network]);
    assert!(
        !granted
            .get(APP_UNIT)
            .expect("an app unit")
            .contents
            .contains("IPAddressDeny=any")
    );
}

#[test]
fn nothing_generated_can_stop_or_kill_stock() {
    let units = units_for(&Facilities::paper_pro(), &[Capability::Storage]);
    for file in &units.files {
        for line in file.contents.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            assert!(
                !(line.starts_with("ExecStop") && line.contains(XOCHITL_UNIT)),
                "{} has an ExecStop touching stock: {line}",
                file.name
            );
            assert!(
                !line.contains("kill") || !line.contains(XOCHITL_UNIT),
                "{} mentions killing stock: {line}",
                file.name
            );
        }
    }
}

#[test]
fn recovery_is_reachable_from_a_dead_supervisor() {
    let units = units_for(&Facilities::paper_pro(), &[]);
    let host = units.get(HOST_UNIT).expect("a host unit");
    assert!(
        host.contents.contains(&format!("OnFailure={RESTORE_UNIT}")),
        "a supervisor that dies must trigger recovery from outside itself"
    );

    let app = units.get(APP_UNIT).expect("an app unit");
    assert!(
        app.contents.contains(&format!("OnFailure={RESTORE_UNIT}")),
        "an app crash must return the panel even with no supervisor watching"
    );

    let restore = units.get(RESTORE_UNIT).expect("a restore unit");
    assert!(
        restore.contents.contains("stock --from-unit"),
        "recovery must run paperctl, not talk to the host"
    );
    assert!(
        restore.contents.contains("StartLimitBurst=2"),
        "recovery must be bounded; stock's own start limit is four in ten minutes"
    );
}

#[test]
fn the_watchdog_outlasts_a_whole_restore() {
    // The supervisor restores stock synchronously, and `platform/device` gives
    // stock two guarded starts with a wait each. A watchdog shorter than that
    // kills the supervisor mid-recovery: systemd fires OnFailure, the restore
    // begins again from nothing, and the machine never reaches `Failed` — so
    // no diagnosis is recorded and §10's "never claim recovery succeeded"
    // becomes "never say anything at all". The VM found this the hard way.
    let worst_case = paper_device::stock::RESTORE_TIMEOUT * 2;
    assert!(
        paper_host::units::HOST_WATCHDOG > worst_case,
        "the watchdog ({:?}) is shorter than a restore ({worst_case:?})",
        paper_host::units::HOST_WATCHDOG
    );

    let units = units_for(&Facilities::paper_pro(), &[]);
    let host = units.get(HOST_UNIT).expect("a host unit");
    assert!(host.contents.contains(&format!(
        "WatchdogSec={}s",
        paper_host::units::HOST_WATCHDOG.as_secs()
    )));

    // And the restore unit must not time out while the restore is still
    // working, for the same reason.
    let restore = units.get(RESTORE_UNIT).expect("a restore unit");
    assert!(restore.contents.contains(&format!(
        "TimeoutStartSec={}s",
        paper_host::units::HOST_WATCHDOG.as_secs()
    )));
}

#[test]
fn systemd_never_restarts_anything_paperclip_owns() {
    let units = units_for(&Facilities::paper_pro(), &[]);
    for file in &units.files {
        for line in file.contents.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("Restart=") {
                assert_eq!(
                    value, "no",
                    "{} lets systemd restart it: {line}; the supervisor owns that decision",
                    file.name
                );
            }
        }
    }
}

#[test]
fn units_are_runtime_only() {
    let spec = SessionSpec::device();
    assert_eq!(
        spec.paths.runtime_units,
        std::path::Path::new("/run/systemd/system"),
        "units must land on a tmpfs so a reboot forgets them"
    );
    assert!(
        spec.paths.root.starts_with("/home/root"),
        "binaries must stay off the root filesystem, which the A/B updater replaces"
    );
}

// --- the isolation report --------------------------------------------------

#[test]
fn the_device_report_refuses_to_claim_a_memory_limit() {
    let report = IsolationReport::derive(&Facilities::paper_pro());
    let memory = report
        .finding(Mechanism::MemoryBound)
        .expect("a memory row");
    assert_eq!(memory.verdict, Verdict::Partial);
    assert!(!memory.verdict.may_be_claimed());
    assert!(memory.caveat.contains("MemoryMax= is NOT in force"));
    assert!(memory.caveat.contains("OOM killer"));
}

#[test]
fn the_device_report_marks_syscall_filtering_ineffective_and_mac_absent() {
    let report = IsolationReport::derive(&Facilities::paper_pro());
    assert_eq!(
        report
            .finding(Mechanism::SyscallFilter)
            .expect("a syscall row")
            .verdict,
        Verdict::Ineffective
    );
    assert_eq!(
        report
            .finding(Mechanism::MandatoryAccessControl)
            .expect("a MAC row")
            .verdict,
        Verdict::Absent
    );
}

#[test]
fn every_unclaimable_protection_says_why() {
    let report = IsolationReport::derive(&Facilities::paper_pro());
    let unclaimable: Vec<_> = report.unclaimable().collect();
    assert!(
        !unclaimable.is_empty(),
        "a report with nothing unclaimable on this device is not describing this device"
    );
    for finding in unclaimable {
        assert!(
            finding.caveat.len() > 40,
            "{:?} is unclaimable with no explanation",
            finding.mechanism
        );
    }
}

#[test]
fn the_report_names_where_its_facts_came_from() {
    let device = IsolationReport::derive(&Facilities::paper_pro());
    assert!(device.source.contains("recorded from reMarkable Paper Pro"));
    assert!(device.to_markdown().contains("recorded from"));

    let probed = Facilities {
        source: FacilitySource::Probed {
            kernel: "7.0.5".to_owned(),
            arch: "aarch64",
        },
        ..Facilities::paper_pro()
    };
    assert!(
        IsolationReport::derive(&probed)
            .source
            .contains("probed on")
    );
}

#[test]
fn tree_containment_is_honest_about_not_being_atomic() {
    let device = Facilities::paper_pro();
    assert!(!device.cgroup_freeze);
    assert!(!device.cgroup_kill);
    let finding = IsolationReport::derive(&device)
        .finding(Mechanism::TreeContainment)
        .expect("a containment row")
        .clone();
    assert_eq!(finding.verdict, Verdict::Enforced);
    assert!(finding.caveat.contains("cannot be atomic"));

    let mut no_cgroup = device.clone();
    no_cgroup.pids = Controller::Absent;
    no_cgroup.cgroup = CgroupLayout::None;
    assert!(!no_cgroup.tree_termination().is_complete());
    assert_eq!(
        IsolationReport::derive(&no_cgroup)
            .finding(Mechanism::TreeContainment)
            .expect("a containment row")
            .verdict,
        Verdict::Partial
    );
}

#[test]
fn the_recorded_device_profile_matches_what_www_11_established() {
    let device = Facilities::paper_pro();
    assert_eq!(device.cgroup, CgroupLayout::Hybrid);
    assert_eq!(device.memory, Controller::NotDelegated);
    assert!(!device.systemd_seccomp);
    assert!(!device.has_lsm_confinement());
    assert!(!device.has_tool(SandboxTool::Unshare));
    assert!(!device.has_tool(SandboxTool::Bwrap));
    assert!(device.has_tool(SandboxTool::Chroot));
    assert!(matches!(
        device.source,
        FacilitySource::Recorded {
            device: "reMarkable Paper Pro",
            ..
        }
    ));
}

// --- the boundary of this suite --------------------------------------------

#[test]
fn nothing_in_this_suite_can_be_mistaken_for_enforcement() {
    // A probe only answers on Linux. Everywhere else it refuses, rather than
    // returning a plausible default that would read like a measurement.
    let probed = paper_host::probe::probe();
    if cfg!(target_os = "linux") {
        assert!(probed.is_ok());
    } else {
        let error = probed.expect_err("a non-Linux probe must refuse to answer");
        assert!(error.to_string().contains("only be probed on Linux"));
    }
}
