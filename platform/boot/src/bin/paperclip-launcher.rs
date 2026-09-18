//! `paperclip-launcher` — decides whether this boot starts Paperclip, and
//! grades the result against `state=home`, not process liveness (WWW-53).
//!
//! Started by `paperclip-launcher.service`, the one unit in this project
//! installed to the device's root filesystem rather than written into
//! `/run/systemd/system` at session start — see [`paper_boot::units`] for
//! why, and ADR-0008's WWW-53 amendment for what that costs.
//!
//! It never lives in a slot: it starts whatever `current` names, but it is
//! not part of what `current` names, and an ordinary platform update never
//! replaces it — `paper_host::units::SessionPaths::launcher` puts it beside
//! `paperctl`, outside `releases/`, for the same reason `paperctl` is there.
//! If this binary becomes something an update can overwrite, the thing that
//! picks the slot is living in a slot, and that is the mistake this whole
//! project is built to avoid making once.

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!(
            "paperclip-launcher: boot autostart runs on the device, or in the Linux VM\n\
             harness. There is no macOS boot path — use `paperctl units` to preview what\n\
             a session would be given, or run the harness in tools/vm-harness."
        );
        ExitCode::FAILURE
    }

    #[cfg(target_os = "linux")]
    {
        linux::main()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use paper_host::facilities::Facilities;
    use paper_host::linux::recovery::RecoveryConfig;
    use paper_host::linux::systemd::{self, Systemd};
    use paper_host::units::{SessionGrants, SessionPaths, SessionSpec, UnitSet};
    use paper_packages::{AppId, InstallPolicy, InstalledApp, Manifest};
    use paper_updater::SessionControl as _;
    use paper_updater::linux::SystemdSession;

    /// The app the boot launcher always requests: Home, one of the three
    /// default apps that ship with Paperclip itself (project description,
    /// §3/§5 amendment) rather than something installed from the catalog.
    const HOME_APP: &str = "dev.calum.home";
    const HOME_MANIFEST: &str = include_str!("../../../../apps/home/paper.toml");

    /// How long the launcher waits for `state=home` before giving up on this
    /// boot. See [`paper_boot::units::LAUNCHER_WATCHDOG`] for the derivation;
    /// this is the grading budget the watchdog constant is sized to exceed.
    const BOOT_GRADE_BUDGET: Duration = Duration::from_secs(45);
    const POLL_INTERVAL: Duration = Duration::from_millis(500);

    pub(super) fn main() -> ExitCode {
        if let Err(error) = paper_telemetry::init_journald() {
            eprintln!("paperclip-launcher: telemetry did not start: {error}");
        }
        let span = tracing::info_span!("boot");
        let _entered = span.enter();

        let paths = SessionPaths::device();
        match run(&paths) {
            Ok(()) => {
                systemd::notify_ready();
                ExitCode::SUCCESS
            }
            Err(message) => {
                tracing::error!("{message}");
                // A non-zero exit here is a launcher *bug* — an I/O failure
                // writing the units, a systemd call that could not even be
                // attempted — never a graded "Home did not arrive", which is
                // handled inside `launch` and always returns `Ok`. See
                // `paperclip-launcher.service`'s `Restart=on-failure` comment
                // for why that distinction is load-bearing.
                ExitCode::FAILURE
            }
        }
    }

    fn run(paths: &SessionPaths) -> Result<(), String> {
        if paper_boot::autostart::is_disabled(&paths.root) {
            tracing::info!("autostart is disabled; leaving the device at stock");
            return Ok(());
        }

        let counter = paper_boot::BootCounter::read(&paths.root)
            .map_err(|error| format!("reading the boot counter: {error}"))?;

        match paper_boot::decide(counter, false) {
            paper_boot::Decision::Skip(reason) => {
                tracing::warn!(reason = %reason.describe(), "not starting Paperclip this boot");
                if let paper_boot::SkipReason::AttemptsExhausted { .. } = reason {
                    // Mirrors what paperclip-safe-mode.service does when
                    // systemd's own start limit trips: set the disable
                    // marker so the device stops trying, in writing, rather
                    // than silently repeating "skip" every boot forever with
                    // nothing for an operator to find over SSH.
                    paper_boot::autostart::disable(&paths.root, &reason.describe())
                        .map_err(|error| format!("recording why autostart stopped: {error}"))?;
                }
                Ok(())
            }
            paper_boot::Decision::Launch => launch(paths),
        }
    }

    fn launch(paths: &SessionPaths) -> Result<(), String> {
        // Durable *before* anything that could wedge the machine happens. A
        // crash between here and `mark_good` is exactly the case this
        // counter exists to catch.
        let attempt = paper_boot::counter::record_attempt(&paths.root)
            .map_err(|error| format!("recording the boot attempt: {error}"))?;
        tracing::info!(attempt = attempt.attempts, "starting Paperclip");

        std::fs::create_dir_all(&paths.state)
            .map_err(|error| format!("creating {}: {error}", paths.state.display()))?;
        // `paperclip-host` requires `--config` to name a file that exists
        // (platform/host/src/bin/paperclip-host.rs); `RuntimeConfig::load`
        // starts from `RuntimeConfig::device()` and only the keys present
        // override it, so an empty-but-parseable file is exactly the device
        // configuration already baked into `SessionPaths::device()` below.
        std::fs::write(
            paths.state.join("config"),
            "# written by paperclip-launcher; device defaults apply\n",
        )
        .map_err(|error| format!("writing the supervisor config: {error}"))?;

        install_units(paths)?;

        let recovery = RecoveryConfig::default();
        let session = SystemdSession::new(recovery, paths.state.clone());
        session
            .bring_up()
            .map_err(|error| format!("starting the supervisor: {error}"))?;

        request_home(paths)?;

        match watch_for_home(paths, &session) {
            Reached::Home => {
                paper_boot::counter::mark_good(&paths.root)
                    .map_err(|error| format!("resetting the boot counter: {error}"))?;
                tracing::info!("Home reached; boot counter reset");
            }
            Reached::Other { state, diagnosis } => {
                // Leave the counter incremented — recorded, not undone.
                // Getting the display back to stock from here is
                // `platform/host`'s own job (ADR-0012's OnFailure= path, or
                // the supervisor's own terminal-state handling); this
                // process has nothing further to do to the display, only to
                // the record of what happened.
                tracing::warn!(
                    state = %state,
                    diagnosis = %diagnosis,
                    "Paperclip did not reach Home this boot; leaving stock in place"
                );
            }
        }
        Ok(())
    }

    /// Writes the runtime session units into `/run/systemd/system` and
    /// reloads systemd. `/run` is a tmpfs (ADR-0008): every boot has to do
    /// this again, which is the whole reason a persistent launcher unit is
    /// needed to trigger it in the first place.
    fn install_units(paths: &SessionPaths) -> Result<(), String> {
        let facilities = paper_host::probe::probe().unwrap_or_else(|error| {
            tracing::warn!(
                "the live facilities probe failed ({error}); falling back to the recorded \
                 Paper Pro profile (WWW-1/WWW-11)"
            );
            Facilities::paper_pro()
        });
        let spec = SessionSpec {
            paths: paths.clone(),
            ..SessionSpec::device()
        };
        let app: AppId = HOME_APP
            .parse()
            .expect("`dev.calum.home` is a valid app id");
        let manifest = Manifest::parse(HOME_MANIFEST)
            .expect("apps/home/paper.toml is checked in and already valid");
        let granted = InstalledApp::install(manifest, &InstallPolicy::deny_all())
            .capabilities()
            .clone();
        let grants = SessionGrants::derive(&app, &granted, &spec.paths);
        let set = UnitSet::plan(&spec, &grants, &facilities);

        std::fs::create_dir_all(&spec.paths.runtime_units)
            .map_err(|error| format!("creating {}: {error}", spec.paths.runtime_units.display()))?;
        for file in &set.files {
            let path = spec.paths.runtime_units.join(&file.name);
            std::fs::write(&path, &file.contents)
                .map_err(|error| format!("writing {}: {error}", path.display()))?;
        }

        Systemd::default()
            .daemon_reload()
            .map_err(|error| format!("daemon-reload: {error}"))
    }

    /// Asks the supervisor to bring Home to the foreground, the same way an
    /// interactive `paperctl` session would (`platform/host/src/linux/runtime.rs`'s
    /// `take_command`).
    fn request_home(paths: &SessionPaths) -> Result<(), String> {
        std::fs::write(paths.state.join("command"), "home\n")
            .map_err(|error| format!("requesting Home: {error}"))
    }

    /// What the boot ended up being.
    enum Reached {
        /// `state=home` was observed in the status file.
        Home,
        /// Anything else: still switching when the budget ran out, in
        /// recovery, failed, or the supervisor exited before saying either.
        Other { state: String, diagnosis: String },
    }

    /// Polls the supervisor's status file for `state=home`, petting the
    /// launcher's own watchdog while it waits.
    ///
    /// Deliberately not [`paper_updater::health::watch`]: that function
    /// grades the *supervisor's* readiness ladder, which reaches `ready`
    /// once the process has initialised — before a foreground switch to
    /// Home has even been requested (`Supervisor::run` sends `READY=1`
    /// immediately after `climb()`, ahead of reading the command file). "Mark
    /// good" for a boot has to mean the stronger claim the ticket asks for:
    /// the machine reached `SessionState::Home`, which only the status
    /// file's `state=` field records.
    fn watch_for_home(paths: &SessionPaths, session: &SystemdSession) -> Reached {
        let started = Instant::now();
        loop {
            systemd::notify_watchdog();
            if let Ok(status) = std::fs::read_to_string(paths.state.join("status")) {
                let state = field(&status, "state").unwrap_or_default();
                if state == "home" {
                    return Reached::Home;
                }
                if state == "failed" || state == "recovering" {
                    return Reached::Other {
                        state,
                        diagnosis: field(&status, "diagnosis").unwrap_or_default(),
                    };
                }
            }
            if !session.observe().alive && started.elapsed() > Duration::from_secs(2) {
                return Reached::Other {
                    state: "absent".to_owned(),
                    diagnosis: "the supervisor exited before reaching Home".to_owned(),
                };
            }
            if started.elapsed() >= BOOT_GRADE_BUDGET {
                return Reached::Other {
                    state: field(
                        &std::fs::read_to_string(paths.state.join("status")).unwrap_or_default(),
                        "state",
                    )
                    .unwrap_or_else(|| "unknown".to_owned()),
                    diagnosis: format!(
                        "deadline of {:.0}s expired before Home arrived",
                        BOOT_GRADE_BUDGET.as_secs_f32()
                    ),
                };
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// One field out of the supervisor's `key=value` status file. The same
    /// shape `paper_updater::linux::SystemdSession` parses internally for
    /// `ready`/`protocol`; duplicated here rather than exposed from there
    /// because those two fields are all that private helper reads, and
    /// `state`/`diagnosis` are a different pair no existing caller needs.
    fn field(status: &str, key: &str) -> Option<String> {
        status.lines().find_map(|line| {
            line.split_once('=')
                .filter(|(name, _)| name.trim() == key)
                .map(|(_, value)| value.trim().to_owned())
        })
    }
}
