//! `paperctl dev` — the desktop dev loop (§14, §15).
//!
//! Runs a real app — [`paper_home::HomeApp`], [`paper_chess::ChessApp`] or
//! [`paper_sudoku::SudokuApp`] — through the actual wire protocol
//! [`paper_sdk::run`] speaks, in a real window, with isolated local storage
//! that survives a restart. Watches this
//! source tree; when it changes, rebuilds `paperctl` itself (which is what
//! every app crate is linked into) and restarts into the freshly built
//! binary — but only once the current session ends (the window closes, or
//! the app itself asks for Home/`Launch`/`ReturnToStock`), never while it is
//! sitting open. Making a rebuild interrupt a session someone is in the
//! middle of would need a way to wake `paper_sdk::desktop`'s event loop from
//! another thread, which does not exist yet ([`PreviewControl`] added a way
//! to close it *from inside* a callback, not from outside one) — recorded
//! here rather than guessed at, per the WWW-6 report.
//!
//! ## The bridge
//!
//! [`crate::session`] is the loopback [`Session`](crate::session::Session)
//! this module drives: the app runs for real — unmodified, the same code a
//! launched process on the device runs — on a background thread, and every
//! pointer tap is forwarded and followed by an explicit draw request, with
//! the reply read for synchronously, so a repaint the tap causes is already
//! in the shared canvas by the time the window redraws — no cross-thread wake
//! needed for that part, because `paper_sdk::desktop::Preview::pointer`
//! already calls `window.request_redraw()` itself after every handled tap.
//! [`crate::run`] drives the same [`Session`] against the real panel instead
//! of a window (WWW-6).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use paper_protocol::{AppId, ExitReason, Request};
use paper_sdk::SCREEN;
use paper_sdk::desktop::{self, PreviewControl, PreviewEvent, PreviewOptions};

use crate::error::CommandError;
use crate::session::{self, SessionError};

/// Source directories `paperctl dev` watches for a reason to rebuild.
///
/// Everything the `apps` feature can reach: the dev-runnable app crates, their
/// rules cores, the SDK and protocol underneath them, and `paperctl` itself.
/// Not `apps/settings` or `apps/app-store` — neither is dev-runnable here (see
/// [`DevError::UnknownApp`]) — but their source is not part of what a session
/// actually executes, so leaving them out does not miss a rebuild those
/// sessions need.
const WATCHED_DIRS: &[&str] = &[
    "tools/paperctl/src",
    "apps/home/src",
    "apps/chess/src",
    "apps/chess-rules/src",
    "apps/sudoku/src",
    "apps/sudoku-rules/src",
    "platform/sdk/src",
    "platform/protocol/src",
    "platform/packages/src",
];

/// Why a `paperctl dev` session could not run.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DevError {
    /// The session itself — opening it, running it, or ending it — failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The local dev storage directory could not be prepared.
    #[error("cannot prepare the dev storage directory {path}")]
    Storage {
        /// Which directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// This binary's own path could not be found, so it cannot relaunch
    /// itself after a rebuild.
    #[error("cannot find this paperctl binary to restart it")]
    CurrentExe(#[source] std::io::Error),
    /// A fresh `paperctl dev` process could not be spawned.
    #[error("cannot start the rebuilt paperctl")]
    Spawn(#[source] std::io::Error),
}

/// `paperctl dev`.
#[derive(Debug, clap::Args)]
pub(crate) struct DevArgs {
    /// Which app to run.
    #[arg(value_enum, default_value_t = DevAppArg::Home)]
    app: DevAppArg,
    /// Wipe this app's local dev storage before starting, rather than
    /// resuming whatever a previous session saved.
    #[arg(long)]
    clean: bool,
}

/// Which app `paperctl dev` can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum DevAppArg {
    /// The shelf, and the one place a dev session can `Launch` from.
    Home,
    /// The chess app, on the real rules core.
    Chess,
    /// The sudoku app, on the real rules core.
    Sudoku,
}

impl DevAppArg {
    const fn slug(self) -> &'static str {
        match self {
            DevAppArg::Home => "home",
            DevAppArg::Chess => "chess",
            DevAppArg::Sudoku => "sudoku",
        }
    }
}

/// Runs the dev loop until the window closes with nothing left to launch.
pub(crate) fn run(args: &DevArgs) -> Result<(), CommandError> {
    let storage_root = dev_storage_root();
    let mut app_slug = args.app.slug().to_owned();

    if args.clean {
        let target = storage_root.join(&app_slug);
        if target.exists() {
            fs::remove_dir_all(&target).map_err(|source| DevError::Storage {
                path: target.clone(),
                source,
            })?;
            eprintln!("paperctl dev: cleared {}", target.display());
        }
    }

    loop {
        eprintln!(
            "paperctl dev: running {app_slug} \u{2014} close the window, or use HOME / RETURN TO STOCK in the app, to pick up code changes"
        );
        let watch_started = SystemTime::now();
        let outcome = run_session(&app_slug, &storage_root)?;
        app_slug = match outcome {
            SessionOutcome::Exit => return Ok(()),
            SessionOutcome::Relaunch(next) => next,
        };

        if !sources_changed_since(watch_started) {
            continue;
        }
        eprintln!("paperctl dev: source changed since this session started; rebuilding...");
        match cargo_build() {
            Ok(()) => {
                eprintln!("paperctl dev: build succeeded; restarting on the new build");
                return relaunch_fresh_binary(&app_slug);
            }
            Err(output) => {
                eprintln!("paperctl dev: build failed; keeping the previous build\n{output}");
            }
        }
    }
}

fn dev_storage_root() -> PathBuf {
    std::env::temp_dir().join("paperclip-dev")
}

/// The workspace root, resolved at compile time from where `paperctl`'s own
/// `Cargo.toml` lives — reliable regardless of the directory `paperctl dev`
/// happens to be invoked from, which a relative path would not be.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sources_changed_since(since: SystemTime) -> bool {
    let root = workspace_root();
    WATCHED_DIRS
        .iter()
        .any(|dir| directory_changed_since(&root.join(dir), since))
}

fn directory_changed_since(dir: &Path, since: SystemTime) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if directory_changed_since(&path, since) {
                return true;
            }
            continue;
        }
        if entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified > since)
        {
            return true;
        }
    }
    false
}

fn cargo_build() -> Result<(), String> {
    let output = std::process::Command::new("cargo")
        .args(["build", "-p", "paperctl"])
        .current_dir(workspace_root())
        .output();
    match output {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => Err(String::from_utf8_lossy(&result.stderr).into_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// Spawns a fresh `paperctl dev <app>` from the just-rebuilt binary.
///
/// This process's own compiled code cannot change underneath it — nothing in
/// Rust hot-swaps a running process's code — so a new process is the only way
/// to actually run the rebuilt binary. Once it is spawned, this function just
/// returns and lets `main` exit normally; there is nothing left to do here.
fn relaunch_fresh_binary(app_slug: &str) -> Result<(), CommandError> {
    let exe = std::env::current_exe().map_err(DevError::CurrentExe)?;
    std::process::Command::new(exe)
        .args(["dev", app_slug])
        .spawn()
        .map_err(DevError::Spawn)?;
    Ok(())
}

/// What a session decided once its window closed.
enum SessionOutcome {
    /// Run `paperctl dev` again for a different app — Home asked to launch
    /// it, or the app itself asked to go Home.
    Relaunch(String),
    /// Nothing more to run.
    Exit,
}

/// Runs one app in one window to completion.
fn run_session(app_slug: &str, storage_root: &Path) -> Result<SessionOutcome, CommandError> {
    let mut current = session::open_session(
        app_slug,
        storage_root,
        "DEV",
        "paperctl dev \u{2014} local, not device verified",
    )
    .map_err(DevError::from)?;
    let options = PreviewOptions::new(format!("paperctl dev \u{2014} {app_slug}"), SCREEN);
    let mut relaunch: Option<String> = None;

    let ran = desktop::run(options, |event| match event {
        PreviewEvent::Render(canvas) => {
            *canvas = current.frame();
            PreviewControl::Continue
        }
        PreviewEvent::Pointer(pointer) => match current.pointer(pointer) {
            Ok(Some(Request::Home)) => {
                relaunch = Some("home".to_owned());
                PreviewControl::Exit
            }
            Ok(Some(Request::Launch(id))) => {
                relaunch = launch_target(&id);
                PreviewControl::Exit
            }
            Ok(Some(Request::ReturnToStock)) => {
                // Dev mode has no stock to hand back to; leaving the tool is
                // the honest equivalent of the real device's exit route.
                PreviewControl::Exit
            }
            Ok(_) => PreviewControl::Continue,
            Err(error) => {
                eprintln!("paperctl dev: {error}");
                PreviewControl::Exit
            }
        },
        _ => PreviewControl::Continue,
    });
    if let Err(error) = ran {
        eprintln!("paperctl dev: the window ended unexpectedly: {error}");
    }

    let reason = if relaunch.is_some() {
        ExitReason::SwitchedAway
    } else {
        ExitReason::ReturnToStock
    };
    match current.shutdown(reason) {
        Ok(true) => {}
        Ok(false) => eprintln!("paperctl dev: the app reported that its save failed"),
        Err(error) => eprintln!("paperctl dev: could not end the session cleanly: {error}"),
    }

    Ok(match relaunch {
        Some(next) => SessionOutcome::Relaunch(next),
        None => SessionOutcome::Exit,
    })
}

fn launch_target(id: &AppId) -> Option<String> {
    match session::launch_target(id) {
        Some(slug) => Some(slug.to_owned()),
        None => {
            eprintln!(
                "paperctl dev: `{id}` is not one of the apps this dev loop can run (chess, home); stopping"
            );
            None
        }
    }
}
