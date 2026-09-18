//! `paperclip-host` — the supervisor process.
//!
//! Started by `paperclip-host.service`, which is written into
//! `/run/systemd/system` at session start and never installed. It speaks
//! `sd_notify`, so systemd's idea of "active" is the app's own readiness
//! rather than the fact that a process exists.
//!
//! It does **not** own recovery. `paperclip-restore-stock.service` does, and
//! systemd starts that through `OnFailure=` — including when this process is
//! killed in a way no handler could survive.

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!(
            "paperclip-host: the supervisor runs on the device, or in the Linux VM harness.\n\
             There is no macOS supervision path, and a stub here would be something to mistake\n\
             for one. Use `paperctl isolation` to inspect what a platform would enforce."
        );
        ExitCode::FAILURE
    }

    #[cfg(target_os = "linux")]
    {
        use std::path::PathBuf;

        use paper_host::linux::runtime::{Outcome, RuntimeConfig, Supervisor};

        if let Err(error) = paper_telemetry::init_journald() {
            eprintln!("paperclip-host: telemetry did not start: {error}");
        }

        let mut arguments = std::env::args().skip(1);
        let mut config_path: Option<PathBuf> = None;
        let mut state: Option<PathBuf> = None;
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "run" => {}
                "--config" => config_path = arguments.next().map(PathBuf::from),
                "--state" => state = arguments.next().map(PathBuf::from),
                other => {
                    tracing::error!("unexpected argument `{other}`");
                    return ExitCode::FAILURE;
                }
            }
        }

        let mut config = match config_path {
            // A missing file is `RuntimeConfig::device()` (WWW-75): nothing
            // writes this path for a first install or a post-reboot start,
            // and `--config` naming one is not a promise that it exists.
            Some(path) => match RuntimeConfig::load_or_device(&path) {
                Ok(config) => config,
                Err(error) => {
                    tracing::error!("cannot read {}: {error}", path.display());
                    return ExitCode::FAILURE;
                }
            },
            None => RuntimeConfig::device(),
        };
        if let Some(state) = state {
            config.paths.state = state;
        }

        // One span for the whole supervised session (WWW-46): every state
        // transition, action and diagnosis `Supervisor::run` logs nests under
        // it, so a `journalctl` filter on this span shows one session's
        // worth of events rather than every session this unit has ever run.
        let span = tracing::info_span!("session");
        let _entered = span.enter();

        let mut supervisor = Supervisor::new(config);
        match supervisor.run() {
            Ok(Outcome::StoppedAtStock) => ExitCode::SUCCESS,
            Ok(Outcome::Halted {
                diagnosis,
                diagnostics,
            }) => {
                tracing::error!(
                    diagnostics = %diagnostics.display(),
                    "halted — {}",
                    diagnosis.summary()
                );
                // Exits zero on purpose. A non-zero exit would trip this
                // unit's OnFailure= and start the recovery that has already
                // failed, which is the restart loop §10 forbids.
                ExitCode::SUCCESS
            }
            Err(error) => {
                tracing::error!("{error}");
                ExitCode::FAILURE
            }
        }
    }
}
