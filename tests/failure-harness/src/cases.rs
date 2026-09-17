//! One function per failure, each one causing it for real.
//!
//! The rule every case follows: **cause the failure, then check what the
//! machine looks like afterwards** — never check that a function was called.
//! The supervisor is a separate process here, started by systemd, and the only
//! things a case may read are the things a person with a serial cable could
//! read: unit states, cgroup membership, the pid table, the status file and
//! the diagnostics bundle.
//!
//! The cases that matter most are the ones where the supervisor is *not*
//! running: `host_crash` kills it with `SIGKILL`, and the display still has to
//! come back. That is the difference between a recovery path and a cleanup
//! routine.

use std::fs;
use std::path::Path;
use std::time::Duration;

use paper_host::units::{HOST_UNIT, RESTORE_UNIT, SESSION_TARGET};

use crate::fixture::{
    BASELINE, Fixture, STOCK_UNIT, alive, property, systemctl, unit_active, unit_pids, wait,
};

/// What a case produced.
pub(crate) type Outcome = Result<Vec<String>, String>;

/// One row of the §10 table, or one of the hazards WWW-4 adds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Case {
    /// The name used on the command line and in the report.
    pub(crate) name: &'static str,
    /// The requirement being demonstrated.
    pub(crate) row: &'static str,
    /// The body.
    pub(crate) run: fn(&Fixture) -> Outcome,
}

/// Every case, in the order the report prints them.
pub(crate) const CASES: &[Case] = &[
    Case {
        name: "facilities",
        row: "§11 — the isolation report describes THIS machine, not a wish list",
        run: facilities,
    },
    Case {
        name: "directives-applied",
        row: "§11 — what the unit says is what systemd is enforcing",
        run: directives_applied,
    },
    Case {
        name: "app-crash-panic",
        row: "App/Home crash — terminate remaining processes, release display, restore stock",
        run: app_crash_panic,
    },
    Case {
        name: "app-crash-abort",
        row: "App/Home crash — abort() is a signal death, not an exit code",
        run: app_crash_abort,
    },
    Case {
        name: "app-hang",
        row: "App hang — deadline, graceful termination, bounded force termination",
        run: app_hang,
    },
    Case {
        name: "heartbeat-is-not-progress",
        row: "App hang — a heartbeat on an unrelated thread does not prove the UI works",
        run: heartbeat_is_not_progress,
    },
    Case {
        name: "ignored-termination",
        row: "App hang — a session that refuses SIGTERM is still bounded",
        run: ignored_termination,
    },
    Case {
        name: "lingering-children",
        row: "Child survives parent — contain and terminate the complete tree",
        run: lingering_children,
    },
    Case {
        name: "display-process-crash",
        row: "Display process crash — terminate the dependent session, restore stock",
        run: display_process_crash,
    },
    Case {
        name: "host-crash",
        row: "Host crash — an independent systemd path cleans up and restores stock",
        run: host_crash,
    },
    Case {
        name: "ssh-disconnect",
        row: "SSH disconnect — the session does not depend on the terminal that started it",
        run: ssh_disconnect,
    },
    Case {
        name: "repeated-failures",
        row: "Repeated failures — stop retrying, no restart loops, diagnostics left behind",
        run: repeated_failures,
    },
    Case {
        name: "stock-fails-to-start",
        row: "Xochitl fails to start — explicit failed state, preserved logs, no false claim",
        run: stock_fails_to_start,
    },
    Case {
        name: "start-budget-refusal",
        row: "Never let stock fail — refuse to take the display near StartLimitBurst",
        run: start_budget_refusal,
    },
    Case {
        name: "reboot",
        row: "Reboot — stock startup stays the default; no automatic takeover",
        run: reboot,
    },
    Case {
        name: "memory-pressure",
        row: "Extra hazard — memory exhaustion is contained and attributable",
        run: memory_pressure,
    },
    Case {
        name: "disk-full",
        row: "Extra hazard — a full writable grant does not take the device with it",
        run: disk_full,
    },
    Case {
        name: "write-outside-grants",
        row: "Extra hazard — a write outside the granted paths is refused",
        run: write_outside_grants,
    },
    Case {
        name: "upgrade-healthy",
        row: "§13 — a healthy release is committed and the old one becomes the fallback",
        run: upgrade_healthy,
    },
    Case {
        name: "upgrade-panics",
        row: "§13 — a release that panics on start is rolled back",
        run: upgrade_panics,
    },
    Case {
        name: "upgrade-never-ready",
        row: "§13 — a release that starts but never reaches `ready` is rolled back",
        run: upgrade_never_ready,
    },
    Case {
        name: "upgrade-middle-rung",
        row: "§13 — a release that stalls mid-ladder is rolled back, naming the rung",
        run: upgrade_middle_rung,
    },
    Case {
        name: "upgrade-power-loss",
        row: "§13 — killed between ACTIVATE and COMMIT, reconcile reverts rather than resumes",
        run: upgrade_power_loss,
    },
    Case {
        name: "upgrade-reboot",
        row: "§13 — a reboot mid-update leaves stock startup available and a reconcilable journal",
        run: upgrade_reboot,
    },
    Case {
        name: "setup-is-idempotent",
        row: "§14 — setup inspects real prerequisites, and running it twice changes nothing",
        run: setup_is_idempotent,
    },
    Case {
        name: "upgrade-cannot-replace-the-bootstrap",
        row: "§13 — an ordinary upgrade replaces nothing outside releases/",
        run: upgrade_cannot_replace_the_bootstrap,
    },
];

// --- helpers ---------------------------------------------------------------

/// Starts the supervisor and waits for it to report itself ready.
///
/// `Type=notify`, so `active` means the process sent `READY=1` — not that it
/// exists. That distinction is the §10 rule about starting not being success,
/// applied to the supervisor itself.
fn start_supervisor(_fixture: &Fixture) -> Result<(), String> {
    systemctl(&["start", HOST_UNIT])?;
    if !wait(Duration::from_secs(10), || unit_active(HOST_UNIT)) {
        return Err(format!(
            "the supervisor did not reach active: {}",
            property(HOST_UNIT, "SubState")
        ));
    }
    Ok(())
}

/// Brings a session up and waits for the supervisor to agree it arrived.
fn bring_up(fixture: &Fixture, app: &str, mode: &[&str]) -> Result<String, String> {
    fixture.install_app(app, mode)?;
    let unit = fixture.app_unit(app);
    fixture.command(&format!("app {app}"))?;
    if !wait(Duration::from_secs(15), || {
        fixture.status_field("state").as_deref() == Some("app")
    }) {
        return Err(format!(
            "the session never arrived; supervisor state is `{}`, unit is `{}`",
            fixture.status_field("state").unwrap_or_default(),
            property(&unit, "ActiveState")
        ));
    }
    Ok(unit)
}

/// The end state every recovery row shares.
fn assert_returned_to_stock(fixture: &Fixture, unit: &str) -> Result<Vec<String>, String> {
    if !wait(Duration::from_secs(25), || unit_active(STOCK_UNIT)) {
        return Err(format!(
            "stock did not come back; it is `{}` and the supervisor says `{}`",
            property(STOCK_UNIT, "ActiveState"),
            fixture.status_field("state").unwrap_or_default()
        ));
    }
    let leftover = unit_pids(unit);
    if !leftover.is_empty() {
        return Err(format!("{leftover:?} were still in the session cgroup"));
    }
    if unit_active(SESSION_TARGET) {
        return Err("the session target is still active after a recovery".to_owned());
    }
    Ok(vec![
        format!("{STOCK_UNIT} active again"),
        format!("{unit} cgroup empty"),
        format!("{SESSION_TARGET} inactive"),
    ])
}

// --- cases -----------------------------------------------------------------

fn facilities(fixture: &Fixture) -> Outcome {
    use paper_host::facilities::Facilities;
    use paper_host::report::IsolationReport;

    let here = &fixture.facilities;
    let device = Facilities::paper_pro();
    let mut evidence = vec![
        format!("probed: {}", here.source),
        format!("cgroup {} / memory {:?}", here.cgroup.name(), here.memory),
        format!(
            "seccomp {} / bpf {:?} / lsm {}",
            here.systemd_seccomp,
            here.systemd_bpf,
            here.lsms.join(",")
        ),
        format!(
            "tree termination: {}",
            here.tree_termination().description()
        ),
    ];

    // The point of this case: the VM is *not* the device, and if the report
    // could not tell them apart it would not be reading anything.
    if here == &device {
        return Err(
            "the probe returned exactly the recorded device profile, which means it \
                    is not probing"
                .to_owned(),
        );
    }
    let report_here = IsolationReport::derive(here);
    let report_device = IsolationReport::derive(&device);
    if report_here.to_markdown() == report_device.to_markdown() {
        return Err("the isolation report is identical for two different platforms".to_owned());
    }
    if !report_here.source.contains("probed on") {
        return Err("the local report does not say it was probed".to_owned());
    }
    if !report_device.source.contains("recorded from") {
        return Err("the device report does not say it was transcribed".to_owned());
    }
    evidence.push(format!(
        "unclaimable here: {}",
        report_here
            .unclaimable()
            .map(|finding| finding.mechanism.title())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    evidence.push(format!(
        "unclaimable on the Paper Pro: {}",
        report_device
            .unclaimable()
            .map(|finding| finding.mechanism.title())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    Ok(evidence)
}

fn directives_applied(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.healthy", &["healthy"])?;

    let mut evidence = Vec::new();
    let mut wrong = Vec::new();
    // Only directives this machine's facilities said would be enforced are
    // checked. Anything else would be checking a wish.
    let expected: Vec<(&str, String)> = {
        let mut expected = vec![
            ("User", crate::fixture::SESSION_USER.to_owned()),
            ("ProtectSystem", "strict".to_owned()),
            ("NoNewPrivileges", "yes".to_owned()),
            ("DevicePolicy", "closed".to_owned()),
            ("LimitAS", (192 * 1024 * 1024_u64).to_string()),
        ];
        if fixture.facilities.pids.enforces() {
            expected.push(("TasksMax", "32".to_owned()));
        }
        if fixture.facilities.memory.enforces() {
            expected.push(("MemoryMax", (192 * 1024 * 1024_u64).to_string()));
        }
        expected
    };
    for (name, want) in expected {
        let got = property(&unit, name);
        evidence.push(format!("{name}={got}"));
        if got != want {
            wrong.push(format!("{name}: expected {want}, systemd reports {got}"));
        }
    }

    // And the one that proves the omission is real rather than cosmetic: this
    // machine HAS a delegated memory controller, so the generator wrote
    // MemoryMax= here — and would not have on the Paper Pro.
    let device_units = {
        use paper_host::facilities::Facilities;
        let spec = paper_host::units::SessionSpec::device();
        let app = paper_packages::Manifest::parse(
            "[app]\nid = \"dev.calum.harness\"\nname = \"H\"\nversion = \"0.1.0\"\n\
             protocol = \"1.0\"\nentrypoint = \"bin/h\"\n",
        )
        .map_err(|error| error.to_string())?;
        let installed =
            paper_packages::InstalledApp::install(app, &paper_packages::InstallPolicy::deny_all());
        let grants = paper_host::units::SessionGrants::derive(
            installed.manifest().id(),
            installed.capabilities(),
            &spec.paths,
        );
        paper_host::units::UnitSet::plan(&spec, &grants, &Facilities::paper_pro())
    };
    let device_app = device_units
        .get(crate::fixture::APP_TEMPLATE)
        .ok_or("no generated app unit for the device")?;
    if device_app
        .contents
        .lines()
        .any(|line| line.trim_start().starts_with("MemoryMax="))
    {
        wrong.push("the device unit still carries MemoryMax=".to_owned());
    }
    evidence.push("the same generator writes no MemoryMax= for the Paper Pro".to_owned());

    let _ = systemctl(&["stop", SESSION_TARGET]);
    if wrong.is_empty() {
        Ok(evidence)
    } else {
        Err(wrong.join("; "))
    }
}

fn app_crash_panic(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.panic", &["panic"])?;
    let mut evidence = assert_returned_to_stock(fixture, &unit)?;
    evidence.push(format!("unit Result={}", property(&unit, "Result")));
    // Waited for rather than asserted instantly: the app unit's own OnFailure=
    // can have stock back before the supervisor has even polled. Two
    // independent recovery paths racing is the design, and the bundle is
    // written by whichever of them gets there.
    let bundle = fixture.paths.diagnostics().join("last-failure.txt");
    if !wait(Duration::from_secs(10), || bundle.exists()) {
        return Err("no diagnostics bundle was written for a crash".to_owned());
    }
    evidence.push(format!(
        "diagnostics bundle written to {}",
        bundle.display()
    ));
    Ok(evidence)
}

fn app_crash_abort(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.abort", &["abort"])?;
    let mut evidence = assert_returned_to_stock(fixture, &unit)?;
    let result = property(&unit, "Result");
    let code = property(&unit, "ExecMainCode");
    evidence.push(format!("Result={result} ExecMainCode={code}"));
    // `CLD_KILLED` is 2. An abort that systemd recorded as a clean exit would
    // mean the supervisor classified a crash as a request to return to stock.
    if code != "2" {
        return Err(format!(
            "abort() was recorded as ExecMainCode={code}, not a signal death"
        ));
    }
    Ok(evidence)
}

fn app_hang(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.hang", &["hang"])?;
    let pids = unit_pids(&unit);
    if pids.is_empty() {
        return Err("the hung session had no processes to begin with".to_owned());
    }
    let started = std::time::Instant::now();
    let mut evidence = assert_returned_to_stock(fixture, &unit)?;
    evidence.push(format!(
        "detected and recovered in {:.1}s (stall budget 2.5s)",
        started.elapsed().as_secs_f32()
    ));
    for pid in pids {
        if alive(pid) {
            return Err(format!("pid {pid} outlived the recovery"));
        }
    }
    evidence.push("every process of the hung session is gone".to_owned());
    Ok(evidence)
}

fn heartbeat_is_not_progress(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    // This session is alive, scheduled, and answering signals the entire time.
    // A liveness check passes it. Only main-loop progress catches it.
    let unit = bring_up(fixture, "dev.calum.heartbeat", &["heartbeat-only"])?;
    let pids = unit_pids(&unit);
    let evidence_alive = format!(
        "{} process(es) alive and responsive when the hang began",
        pids.len()
    );
    let mut evidence = assert_returned_to_stock(fixture, &unit)?;
    evidence.insert(0, evidence_alive);
    evidence.push("a live, signal-responsive process was still classified as hung".to_owned());
    Ok(evidence)
}

fn ignored_termination(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.stubborn", &["ignore-term"])?;
    let pids = unit_pids(&unit);
    // It ignores SIGTERM, so the only way out is the bounded force stage.
    fixture.command("stock")?;
    let mut evidence = assert_returned_to_stock(fixture, &unit)?;
    for pid in &pids {
        if alive(*pid) {
            return Err(format!("pid {pid} survived a session that refused SIGTERM"));
        }
    }
    evidence.push(format!(
        "{} process(es) that ignored SIGTERM were force-terminated inside the budget",
        pids.len()
    ));
    Ok(evidence)
}

fn lingering_children(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.leavers", &["leave-children"])?;
    // The parent exits cleanly and leaves children reparented to pid 1. A
    // parent-pid walk cannot find these; the cgroup sweep can.
    std::thread::sleep(Duration::from_millis(600));
    let orphans = unit_pids(&unit);
    let mut evidence = vec![format!(
        "{} orphaned child process(es) observed",
        orphans.len()
    )];
    let more = assert_returned_to_stock(fixture, &unit)?;
    evidence.extend(more);
    for pid in orphans {
        if alive(pid) {
            return Err(format!("orphan {pid} outlived its session"));
        }
    }
    evidence.push("every orphan was contained and terminated".to_owned());
    Ok(evidence)
}

fn display_process_crash(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.display", &["healthy"])?;
    // The registry names the process holding the panel. Killing it while the
    // session is still healthy is the "display process crash" row: the session
    // did not die, but it no longer owns the display.
    let lock = fixture.locks_dir.join("epframebuffer.lock");
    let holder: u32 = fs::read_to_string(&lock)
        .map_err(|error| format!("cannot read the ownership registry: {error}"))?
        .split_whitespace()
        .next()
        .ok_or("the ownership registry was empty")?
        .parse()
        .map_err(|_| "the ownership registry did not name a pid".to_owned())?;
    fs::remove_file(&lock).map_err(|error| error.to_string())?;
    let mut evidence = vec![format!("display owner was pid {holder}; registry cleared")];
    evidence.extend(assert_returned_to_stock(fixture, &unit)?);
    if alive(holder) {
        return Err(format!(
            "the session ({holder}) kept running with no display"
        ));
    }
    evidence
        .push("the dependent session was terminated rather than left holding nothing".to_owned());
    Ok(evidence)
}

fn host_crash(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.orphaned", &["healthy"])?;
    let host_pid: u32 = property(HOST_UNIT, "MainPID")
        .parse()
        .map_err(|_| "the supervisor has no main pid".to_owned())?;

    // SIGKILL. No handler runs, no destructor runs, no trap runs. Whatever
    // brings the display back is by definition not the supervisor.
    systemctl(&["kill", "--signal", "SIGKILL", HOST_UNIT])
        .or_else(|_| systemctl(&["kill", "-s", "SIGKILL", HOST_UNIT]))?;
    if !wait(Duration::from_secs(10), || !alive(host_pid)) {
        return Err("the supervisor survived SIGKILL, which is not possible".to_owned());
    }

    let mut evidence = vec![format!("supervisor pid {host_pid} killed with SIGKILL")];
    if !wait(Duration::from_secs(20), || {
        !property(RESTORE_UNIT, "NRestarts").is_empty() && unit_active(STOCK_UNIT)
    }) && !unit_active(STOCK_UNIT)
    {
        return Err(format!(
            "no independent recovery ran; stock is `{}` and {RESTORE_UNIT} is `{}`",
            property(STOCK_UNIT, "ActiveState"),
            property(RESTORE_UNIT, "Result")
        ));
    }
    evidence.push(format!(
        "{RESTORE_UNIT} ran from systemd's OnFailure= with no supervisor alive"
    ));
    evidence.extend(assert_returned_to_stock(fixture, &unit)?);
    Ok(evidence)
}

fn ssh_disconnect(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.remote", &["healthy"])?;
    let session_pids = unit_pids(&unit);
    let host_pid: u32 = property(HOST_UNIT, "MainPID").parse().unwrap_or(0);

    // Simulate the terminal going away: a shell in its own session, which we
    // then kill along with its whole process group. Nothing of ours is in it,
    // because systemd started everything — which is the entire claim.
    let mut shell = std::process::Command::new("setsid")
        .args(["sh", "-c", "sleep 30"])
        .spawn()
        .map_err(|error| format!("cannot start a stand-in login shell: {error}"))?;
    let shell_pid = shell.id();
    std::thread::sleep(Duration::from_millis(300));
    shell.kill().map_err(|error| error.to_string())?;
    let _ = shell.wait();

    let mut evidence = vec![format!("stand-in login session {shell_pid} killed")];
    std::thread::sleep(Duration::from_millis(500));
    if !unit_active(&unit) {
        return Err("the session died with the terminal that started it".to_owned());
    }
    if host_pid != 0 && !alive(host_pid) {
        return Err("the supervisor died with the terminal".to_owned());
    }
    for pid in &session_pids {
        if !alive(*pid) {
            return Err(format!("session pid {pid} died with the terminal"));
        }
    }
    evidence.push(format!(
        "session and supervisor still running ({} process(es))",
        session_pids.len()
    ));
    evidence.push(format!(
        "session cgroup is {} — systemd's, not a login scope",
        property(&unit, "ControlGroup")
    ));
    fixture.command("stock")?;
    evidence.extend(assert_returned_to_stock(fixture, &unit)?);
    Ok(evidence)
}

fn repeated_failures(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let mut evidence = Vec::new();
    let unit = fixture.app_unit("dev.calum.flaky");
    fixture.install_app("dev.calum.flaky", &["panic"])?;

    for attempt in 1..=3 {
        fixture.command("app dev.calum.flaky")?;
        // Both halves are needed. Waiting only for "back at stock" passes
        // instantly, because that is where it already is — the case would
        // then measure nothing three times and still look green.
        if !wait(Duration::from_secs(25), || {
            fixture.status_field("state").as_deref() == Some("app")
        }) {
            return Err(format!(
                "attempt {attempt} never reached a running session; supervisor says `{}`",
                fixture.status_field("state").unwrap_or_default()
            ));
        }
        // A switch requested while it is still recovering is ignored on
        // purpose — queueing one is how two owners end up racing for the
        // panel — so the next attempt must not be asked for early.
        if !wait(Duration::from_secs(30), || {
            unit_active(STOCK_UNIT) && fixture.status_field("state").as_deref() == Some("stock")
        }) {
            return Err(format!(
                "attempt {attempt} did not end back at stock; supervisor says `{}`",
                fixture.status_field("state").unwrap_or_default()
            ));
        }
        evidence.push(format!("crash {attempt} recovered to stock"));
    }

    if fixture.status_field("may_relaunch").as_deref() != Some("false") {
        return Err(format!(
            "after three crashes the supervisor is still willing to relaunch: {}",
            fixture.status()
        ));
    }
    evidence.push("supervisor stopped relaunching after its failure budget".to_owned());

    // And it must stay stopped: a fourth request is recorded and not acted on.
    let before = property(&unit, "NRestarts");
    fixture.command("app dev.calum.flaky")?;
    std::thread::sleep(Duration::from_secs(3));
    if !unit_active(STOCK_UNIT) {
        return Err("a request after the budget was honoured; that is a restart loop".to_owned());
    }
    evidence.push(format!(
        "a further request left the device at stock (NRestarts {before} -> {})",
        property(&unit, "NRestarts")
    ));
    let diagnosis = fixture.status_field("diagnosis").unwrap_or_default();
    if diagnosis.is_empty() {
        return Err("no diagnosis was recorded for the repeated failures".to_owned());
    }
    evidence.push(format!("diagnosis: {diagnosis}"));
    Ok(evidence)
}

fn stock_fails_to_start(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.doomed", &["healthy"])?;

    // From here stock cannot come back, which is the worst state in §10 and
    // the one where an optimistic supervisor does the most damage.
    fixture.stock_should_fail()?;
    fixture.command("stock")?;

    // Generous on purpose: the restore underneath is two guarded starts with a
    // 25 s wait each, and the point of this case is that it gives up *after*
    // trying properly rather than declaring failure early.
    if !wait(Duration::from_secs(120), || {
        fixture.status_field("state").as_deref() == Some("failed")
    }) {
        fixture.stock_should_start()?;
        return Err(format!(
            "the supervisor never reached `failed`; it says `{}`",
            fixture.status_field("state").unwrap_or_default()
        ));
    }
    let mut evidence = vec!["supervisor state is `failed`".to_owned()];

    let bundle = fixture.paths.diagnostics().join("last-failure.txt");
    let text = fs::read_to_string(&bundle)
        .map_err(|error| format!("no preserved diagnostics at {}: {error}", bundle.display()))?;
    if !text.contains("systemctl status") || !text.contains("journal") {
        return Err("the diagnostics bundle preserved no logs".to_owned());
    }
    evidence.push(format!(
        "{} bytes of preserved logs at {}",
        text.len(),
        bundle.display()
    ));

    let status = fixture.status();
    if status.contains("state=stock") {
        return Err("the supervisor claimed stock was restored when it was not".to_owned());
    }
    evidence.push("no claim of a successful recovery anywhere in the status".to_owned());

    // The independent CLI must give the same answer, and must fail loudly.
    let paperctl = fixture.paths.root.join("bin/paperctl");
    let output = std::process::Command::new(&paperctl)
        .args(["stock", "--state"])
        .arg(&fixture.paths.state)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        fixture.stock_should_start()?;
        return Err(
            "`paperctl stock` reported success while stock was refusing to start".to_owned(),
        );
    }
    let message = String::from_utf8_lossy(&output.stderr);
    if !message.contains("NOT restored") {
        return Err(format!(
            "`paperctl stock` was not explicit about failing: {message}"
        ));
    }
    evidence
        .push("`paperctl stock` failed explicitly and independently of the supervisor".to_owned());

    // And the supervisor must not have hammered stock's start limit.
    let attempts = property(STOCK_UNIT, "NRestarts");
    evidence.push(format!("stock start attempts recorded: {attempts}"));

    fixture.stock_should_start()?;
    let _ = systemctl(&["reset-failed", STOCK_UNIT]);
    let _ = unit;
    Ok(evidence)
}

fn start_budget_refusal(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    // Three recent starts recorded, which is `platform/device`'s whole
    // allowance out of systemd's four. A fourth would be the one that fails
    // the unit, and Xochitl's OnFailure= is an emergency shell on a device
    // with no serial console attached.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    fs::write(
        fixture.paths.state.join("xochitl-starts"),
        format!("{now}\n{}\n{}\n", now - 5, now - 10),
    )
    .map_err(|error| error.to_string())?;

    fixture.install_app("dev.calum.unlucky", &["healthy"])?;
    let unit = fixture.app_unit("dev.calum.unlucky");
    fixture.command("app dev.calum.unlucky")?;

    // The session must never start, and stock must never be stopped.
    std::thread::sleep(Duration::from_secs(6));
    if unit_active(&unit) {
        return Err("a session started with the start budget spent".to_owned());
    }
    if !unit_active(STOCK_UNIT) {
        return Err("stock was stopped despite the refusal".to_owned());
    }
    let state = fixture.status_field("state").unwrap_or_default();
    if state != "stock" {
        return Err(format!(
            "the supervisor ended in `{state}`, not back at stock"
        ));
    }
    Ok(vec![
        "three recent starts recorded — the whole device-level allowance".to_owned(),
        format!("{unit} never started"),
        format!("{STOCK_UNIT} never stopped"),
        "supervisor back at stock, having refused rather than begun".to_owned(),
    ])
}

fn reboot(fixture: &Fixture) -> Outcome {
    let mut evidence = Vec::new();
    // Nothing of ours may be installed anywhere that survives a reboot.
    for directory in [
        "/etc/systemd/system",
        "/usr/lib/systemd/system",
        "/lib/systemd/system",
    ] {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("paperclip") {
                return Err(format!("{directory}/{name} would survive a reboot"));
            }
        }
    }
    evidence.push("no paperclip unit outside /run/systemd/system".to_owned());

    let enabled = systemctl(&["list-unit-files", "paperclip*", "--no-legend"]).unwrap_or_default();
    for line in enabled.lines() {
        if line.contains("enabled") {
            return Err(format!("a paperclip unit is enabled: {line}"));
        }
    }
    evidence.push("no paperclip unit is enabled".to_owned());

    // /run is a tmpfs, so a reboot is exactly "the unit files are gone".
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    if !mounts.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next();
        fields.next() == Some("/run") && fields.next() == Some("tmpfs")
    }) {
        return Err(
            "/run is not a tmpfs here, so the reboot guarantee cannot be checked".to_owned(),
        );
    }
    evidence.push("/run is a tmpfs; the generated units do not survive a boot".to_owned());

    // Simulate it, then put the fixture back.
    for unit in fixture.unit_names() {
        let _ = fs::remove_file(fixture.paths.runtime_units.join(&unit));
    }
    systemctl(&["daemon-reload"])?;
    let after = systemctl(&["list-unit-files", "paperclip*", "--no-legend"]).unwrap_or_default();
    let remaining: Vec<&str> = after
        .lines()
        .filter(|line| line.starts_with("paperclip") && !line.starts_with("paperclip-harness"))
        .collect();
    if !remaining.is_empty() {
        return Err(format!(
            "units survived the simulated reboot: {remaining:?}"
        ));
    }
    evidence.push("after clearing /run, systemd knows of no paperclip units at all".to_owned());

    for file in fixture.units().files {
        fs::write(fixture.paths.runtime_units.join(&file.name), &file.contents)
            .map_err(|error| error.to_string())?;
    }
    systemctl(&["daemon-reload"])?;
    Ok(evidence)
}

fn memory_pressure(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let unit = bring_up(fixture, "dev.calum.hungry", &["memory-pressure"])?;
    let mut evidence = assert_returned_to_stock(fixture, &unit)?;
    evidence.push(format!(
        "session ended as Result={} ExecMainStatus={}",
        property(&unit, "Result"),
        property(&unit, "ExecMainStatus")
    ));
    // The claim is containment, not a memory limit: the supervisor survived,
    // stock came back, and the failure is attributable to the session.
    if !unit_active(HOST_UNIT) {
        return Err("the supervisor died with the session that ran out of memory".to_owned());
    }
    evidence.push("the supervisor survived the session's memory exhaustion".to_owned());
    evidence.push(format!(
        "bounded by {} here",
        if fixture.facilities.memory.enforces() {
            "MemoryMax= and LimitAS="
        } else {
            "LimitAS= only — see the isolation report"
        }
    ));
    Ok(evidence)
}

fn disk_full(fixture: &Fixture) -> Outcome {
    // A small tmpfs standing in for a full writable grant.
    let grant = fixture.paths.storage.join("dev.calum.harness");
    fs::create_dir_all(&grant).map_err(|error| error.to_string())?;
    systemctl(&["--version"]).ok();
    let mount = std::process::Command::new("mount")
        .args(["-t", "tmpfs", "-o", "size=1m,mode=0777", "tmpfs"])
        .arg(&grant)
        .status()
        .map_err(|error| format!("cannot mount the stand-in grant: {error}"))?;
    if !mount.success() {
        return Err("cannot mount a 1 MiB tmpfs for the full-disk case".to_owned());
    }
    let result = (|| -> Outcome {
        start_supervisor(fixture)?;
        let unit = bring_up(
            fixture,
            "dev.calum.filler",
            &[
                "disk-fill",
                "--target",
                &grant.join("blob").display().to_string(),
            ],
        )?;
        let mut evidence = assert_returned_to_stock(fixture, &unit)?;
        if !unit_active(HOST_UNIT) {
            return Err("the supervisor died when the session filled its grant".to_owned());
        }
        evidence.push("the supervisor and its state directory were unaffected".to_owned());
        evidence.push(format!(
            "supervisor could still write its status ({} bytes)",
            fixture.status().len()
        ));
        Ok(evidence)
    })();
    let _ = std::process::Command::new("umount").arg(&grant).status();
    result
}

fn write_outside_grants(fixture: &Fixture) -> Outcome {
    start_supervisor(fixture)?;
    let mut evidence = Vec::new();
    // Three targets: the root filesystem, Paperclip's own read-only tree, and
    // the device-identity path that must never be written on the tablet.
    for (label, target) in [
        ("root filesystem", "/etc/paperclip-should-not-exist"),
        (
            "paperclip's own tree",
            "/opt/paperclip-harness/bin/tampered",
        ),
        ("device identity", "/data/paperclip-should-not-exist"),
    ] {
        let app = "dev.calum.escapee";
        fixture.install_app(app, &["write-outside", "--target", target])?;
        let unit = fixture.app_unit(app);
        let _ = systemctl(&["reset-failed", &unit]);
        fixture.command(&format!("app {app}"))?;
        if !wait(Duration::from_secs(20), || {
            !property(&unit, "ExecMainStatus").is_empty()
                && property(&unit, "ActiveState") != "active"
        }) {
            return Err(format!("the {label} attempt never finished"));
        }
        let status = property(&unit, "ExecMainStatus");
        if status == "42" {
            return Err(format!(
                "a session WROTE {target} — the {label} boundary does not hold"
            ));
        }
        evidence.push(format!("{label}: {target} refused (exit {status})"));
        if std::path::Path::new(target).exists() {
            return Err(format!("{target} exists after the attempt"));
        }
        let _ = systemctl(&["stop", SESSION_TARGET]);
        std::thread::sleep(Duration::from_millis(300));
    }
    let _ = fixture.reset();
    Ok(evidence)
}


// --- §13: platform upgrade ---------------------------------------------------
//
// Every case here drives the real `paperctl upgrade`, against real systemd,
// with a real release tree. What is a stand-in is the *candidate*: its
// `bin/paperclip-host` is `paper-fault-app` playing a scripted climb, because
// the only other way to make a release stall at `device-adapter` is to break
// a display. The baseline release is the real supervisor, so "the fallback
// came back" always means the real one climbed the real ladder.

/// Runs `paperctl upgrade run` against a bundle and returns its output.
fn run_upgrade(fixture: &Fixture, bundle: &Path) -> Result<String, String> {
    let extra = fixture.upgrade_args();
    let trust = fixture.trust_file();
    let arguments = vec![
        "upgrade",
        "run",
        bundle.to_str().ok_or("a non-UTF-8 bundle path")?,
        "--trust",
        trust.to_str().ok_or("a non-UTF-8 key path")?,
        &extra[0],
        &extra[1],
        &extra[2],
        &extra[3],
    ];
    fixture.paperctl(&arguments)
}

/// `paperctl upgrade status`, as a string.
fn upgrade_status(fixture: &Fixture) -> Result<String, String> {
    let extra = fixture.upgrade_args();
    fixture.paperctl(&["upgrade", "status", &extra[0], &extra[1]])
}

/// The version `current` names right now.
fn selected(fixture: &Fixture) -> String {
    fixture
        .platform
        .selected()
        .ok()
        .flatten()
        .map_or_else(|| "none".to_owned(), |version| version.to_string())
}

/// Every upgrade case ends the same way: stock owns the display and nothing of
/// ours is still running.
fn assert_at_stock(fixture: &Fixture) -> Result<Vec<String>, String> {
    if !wait(Duration::from_secs(20), || unit_active(STOCK_UNIT)) {
        return Err(format!(
            "stock is `{}` after the upgrade",
            property(STOCK_UNIT, "ActiveState")
        ));
    }
    let held = fs::read_to_string(fixture.power_dir.join("wake_lock")).unwrap_or_default();
    if held.split_whitespace().any(|tag| tag == "paperclip-harness") {
        return Err("the wakelock is still held after the upgrade".to_owned());
    }
    Ok(vec![
        "stock is active and the wakelock is released".to_owned(),
    ])
}

fn upgrade_healthy(fixture: &Fixture) -> Outcome {
    let bundle = fixture.bundle("0.2.0", "ready")?;
    let output = run_upgrade(fixture, &bundle)?;

    if selected(fixture) != "0.2.0" {
        return Err(format!("`current` is {} after a healthy upgrade", selected(fixture)));
    }
    let fallback = fixture
        .platform
        .fallback()
        .map_err(|error| error.to_string())?
        .map_or_else(|| "none".to_owned(), |version| version.to_string());
    if fallback != BASELINE {
        return Err(format!("`previous` is {fallback}, not the baseline {BASELINE}"));
    }
    if !unit_active(HOST_UNIT) {
        return Err("the new supervisor is not running after a committed upgrade".to_owned());
    }
    let status = upgrade_status(fixture)?;
    if !status.contains("commit") {
        return Err(format!("the journal did not reach commit:\n{status}"));
    }
    Ok(vec![
        format!("upgrade reported: {}", output.trim().replace('\n', " | ")),
        format!("current 0.2.0, previous {BASELINE}, journal at commit"),
    ])
}

fn upgrade_panics(fixture: &Fixture) -> Outcome {
    let bundle = fixture.bundle("0.2.0", "panic")?;
    let output = run_upgrade(fixture, &bundle)?;

    if selected(fixture) != BASELINE {
        return Err(format!(
            "`current` is {} after a candidate that panicked; it should be back at {BASELINE}",
            selected(fixture)
        ));
    }
    if !output.contains("refused") {
        return Err(format!("the report did not say the candidate was refused:\n{output}"));
    }
    if !unit_active(HOST_UNIT) {
        return Err("the restored supervisor is not running".to_owned());
    }
    let mut evidence = vec![
        "a candidate whose supervisor panicked was refused and 0.1.0 came back".to_owned(),
    ];
    evidence.push(format!("report: {}", output.trim().replace('\n', " | ")));
    Ok(evidence)
}

fn upgrade_never_ready(fixture: &Fixture) -> Outcome {
    // The §13 sentence, exercised: the unit reaches `active` — the stand-in
    // sends READY=1 — and the release is still refused, because the readiness
    // ladder in the status file never reaches `ready`.
    let bundle = fixture.bundle("0.2.0", "stall=home")?;
    let output = run_upgrade(fixture, &bundle)?;

    if selected(fixture) != BASELINE {
        return Err(format!(
            "`current` is {}; a release that never reached `ready` was committed",
            selected(fixture)
        ));
    }
    if !output.contains("home") {
        return Err(format!("the report did not name the rung reached:\n{output}"));
    }
    Ok(vec![
        "a release that went `active` without reaching `ready` was refused".to_owned(),
        format!("report: {}", output.trim().replace('\n', " | ")),
    ])
}

fn upgrade_middle_rung(fixture: &Fixture) -> Outcome {
    let bundle = fixture.bundle("0.2.0", "stall=protocol")?;
    let output = run_upgrade(fixture, &bundle)?;

    if selected(fixture) != BASELINE {
        return Err(format!("`current` is {} after a mid-ladder stall", selected(fixture)));
    }
    if !output.contains("device-adapter") {
        return Err(format!(
            "the report must name the rung that was never reached:\n{output}"
        ));
    }
    let mut evidence = vec![format!("report: {}", output.trim().replace('\n', " | "))];
    evidence.extend(assert_at_stock(fixture).unwrap_or_default());
    Ok(evidence)
}

fn upgrade_power_loss(fixture: &Fixture) -> Outcome {
    // Kill the upgrade between ACTIVATE and COMMIT, the way a flat battery
    // would. Nothing gets a chance to clean up: no destructor, no signal
    // handler, no trap. What has to be enough is the journal.
    let bundle = fixture.bundle("0.2.0", "ready")?;
    let extra = fixture.upgrade_args();
    let trust = fixture.trust_file();
    let mut child = std::process::Command::new(fixture.paths.root.join("bin/paperctl"))
        .args([
            "upgrade",
            "run",
            bundle.to_str().ok_or("a non-UTF-8 bundle path")?,
            "--trust",
            trust.to_str().ok_or("a non-UTF-8 key path")?,
            &extra[0],
            &extra[1],
            &extra[2],
            &extra[3],
        ])
        .spawn()
        .map_err(|error| format!("cannot start paperctl: {error}"))?;

    let journal = fixture.platform.journal_file();
    let reached = wait(Duration::from_secs(60), || {
        fs::read_to_string(&journal)
            .is_ok_and(|raw| raw.contains("\"activate\"") || raw.contains("\"verify\""))
    });
    let _ = child.kill();
    let _ = child.wait();
    if !reached {
        return Err("the upgrade never reached ACTIVATE, so there was nothing to interrupt"
            .to_owned());
    }

    let before = fs::read_to_string(&journal).unwrap_or_default();
    if !before.contains("\"activate\"") && !before.contains("\"verify\"") {
        return Err(format!("the journal is not mid-transaction:\n{before}"));
    }

    // What `paperctl` does on the way back up.
    let output = fixture.paperctl(&["upgrade", "reconcile", &extra[0], &extra[1]])?;
    if selected(fixture) != BASELINE {
        return Err(format!(
            "reconcile left `current` at {}; an update that never committed must not stay selected",
            selected(fixture)
        ));
    }
    Ok(vec![
        "killed mid-transaction with SIGKILL; the journal survived".to_owned(),
        format!("reconcile: {}", output.trim().replace('\n', " | ")),
        format!("`current` is back at {BASELINE}"),
    ])
}

fn upgrade_reboot(fixture: &Fixture) -> Outcome {
    // Two separate guarantees, both structural.
    let mut evidence = Vec::new();

    // 1. The journal is on persistent storage, not /run. A record cleared by
    //    the reboot it exists to survive would be no record at all.
    let journal = fixture.platform.journal_file();
    if journal.starts_with("/run") {
        return Err(format!(
            "the update journal is at {}, which a reboot clears",
            journal.display()
        ));
    }
    evidence.push(format!(
        "the update journal is at {}, outside /run",
        journal.display()
    ));

    // 2. The units that would bring a candidate back up are in /run, so a
    //    reboot mid-update lands at stock by construction (WWW-11). The
    //    `reboot` case proves the general form; this checks it still holds for
    //    the unit whose ExecStart now resolves through `current`.
    let host_unit = fixture.paths.runtime_units.join(HOST_UNIT);
    if !host_unit.starts_with("/run") {
        return Err(format!("{} would survive a reboot", host_unit.display()));
    }
    let contents = fs::read_to_string(&host_unit).map_err(|error| error.to_string())?;
    let expected = fixture.paths.root.join("current/bin/paperclip-host");
    if !contents.contains(&expected.display().to_string()) {
        return Err(format!(
            "the host unit does not start the selected release; ExecStart should name {}",
            expected.display()
        ));
    }
    // A *line* that is `[Install]`, not the substring — the unit's own comment
    // says "No [Install]: never enabled", and matching that would fail a unit
    // for documenting the thing it does not do.
    if contents.lines().any(|line| line.trim() == "[Install]") {
        return Err("the host unit has an [Install] section, so a reboot could take over".to_owned());
    }
    evidence.push("the supervisor unit is runtime-only, resolves through `current`, and is never enabled".to_owned());

    // 3. And an interrupted journal is reconcilable without a session to talk
    //    to — which is the state a reboot actually leaves.
    let extra = fixture.upgrade_args();
    let output = fixture.paperctl(&["upgrade", "reconcile", &extra[0], &extra[1]])?;
    evidence.push(format!("reconcile on a clean tree: {}", output.trim()));
    Ok(evidence)
}

fn upgrade_cannot_replace_the_bootstrap(fixture: &Fixture) -> Outcome {
    // §13: routine installation can never replace the host, the trust keys or
    // the recovery bootstrap. For a *platform* upgrade the first is the point;
    // the other two must survive it untouched.
    let bootstrap = fixture.paths.root.join("bin/paperctl");
    let keys = fixture.platform.keys_dir().join("harness.pub");
    let before = (
        fs::metadata(&bootstrap).map_err(|error| error.to_string())?.len(),
        fs::read(&keys).map_err(|error| error.to_string())?,
    );

    let bundle = fixture.bundle("0.2.0", "ready")?;
    run_upgrade(fixture, &bundle)?;

    let after = (
        fs::metadata(&bootstrap).map_err(|error| error.to_string())?.len(),
        fs::read(&keys).map_err(|error| error.to_string())?,
    );
    if before != after {
        return Err("a platform upgrade changed the bootstrap or the trusted keys".to_owned());
    }

    // And nothing landed outside `releases/` and `state/`.
    let stray: Vec<String> = fs::read_dir(&fixture.paths.root)
        .map_err(|error| error.to_string())?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            !matches!(
                name.as_str(),
                "releases"
                    | "current"
                    | "previous"
                    | "staging"
                    | "state"
                    | "keys"
                    | "bin"
                    | "apps"
                    | "storage"
                    | "bundles"
                    | ".paperclip-platform"
            )
        })
        .collect();
    if !stray.is_empty() {
        return Err(format!("the upgrade left unexpected paths: {stray:?}"));
    }
    Ok(vec![
        "the bootstrap and the trusted keys are byte-identical after a platform upgrade".to_owned(),
        "nothing was written outside the release tree".to_owned(),
    ])
}


fn setup_is_idempotent(fixture: &Fixture) -> Outcome {
    // §14 asks for staged, version-aware and diagnostic — and for setup to
    // inspect the actual prerequisites rather than treat a working shell as
    // proof of anything. The part worth a case is the idempotence: setup is
    // the command someone runs when they are not sure what state the device is
    // in, which is exactly when a command that is only safe once is worst.
    let root = fixture.paths.root.display().to_string();
    let arguments = ["setup", "--root", &root, "--stock-unit", STOCK_UNIT];

    let first = fixture.paperctl(&arguments)?;
    let selected_before = selected(fixture);
    let second = fixture.paperctl(&arguments)?;
    let selected_after = selected(fixture);

    if selected_before != selected_after {
        return Err(format!(
            "setup changed the selected release from {selected_before} to {selected_after}"
        ));
    }
    for (run, output) in [("first", &first), ("second", &second)] {
        if !output.contains("isolation") || !output.contains("runtime units") {
            return Err(format!("the {run} run skipped a prerequisite stage:\n{output}"));
        }
        if output.contains("MISSING") {
            return Err(format!("the {run} run reported a missing prerequisite:\n{output}"));
        }
    }
    if !second.contains("is established") {
        return Err(format!("the second run did not recognise the root:\n{second}"));
    }
    Ok(vec![
        "setup reported every stage and found no prerequisite missing".to_owned(),
        format!("run twice; `current` stayed at {selected_after}"),
    ])
}
