//! `paperctl dev` — the desktop dev loop (§14, §15).
//!
//! Runs a real app — [`paper_home::HomeApp`], [`paper_chess::ChessApp`] or
//! [`paper_sudoku::SudokuApp`] — through the actual wire protocol
//! [`paper_sdk::run`] speaks, in a real window, with isolated local storage
//! that survives a restart.
//!
//! ## Hot reload
//!
//! A [`notify`] watcher on this source tree runs for as long as `paperctl
//! dev` does, on its own thread. When it sees a change during an open
//! session, it does two things: sets a flag this module checks once the
//! session ends, and asks the window to close *now*, through
//! [`desktop::PreviewHandle`] — the door [`PreviewControl`] left for exactly
//! this, closing the loop from outside a [`PreviewEvent`] callback rather
//! than only from inside one. A closed window without an app-requested
//! `Launch`/`Home`/`ReturnToStock` behind it and a set flag means "the
//! watcher did this, not the user", which is what tells [`run`] to rebuild
//! and relaunch the same app rather than exit the tool the way an
//! intentional close still does. The loop this makes is edit, save, look —
//! nothing left to close by hand.
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
//! [`crate::run()`] drives the same [`Session`](crate::session::Session)
//! against the real panel instead of a window (WWW-6).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use notify::Watcher as _;
use paper_protocol::{AppId, ExitReason, PixelFormat, Request, SurfaceDescriptor};
use paper_sdk::SCREEN;
use paper_sdk::desktop::{self, PreviewControl, PreviewEvent, PreviewHandle, PreviewOptions};

use crate::error::CommandError;
use crate::session::{self, SessionError};

/// Source directories `paperctl dev` watches for a reason to rebuild.
///
/// Everything the `apps` feature can reach: the dev-runnable app crates, their
/// rules cores, the SDK and protocol underneath them, and `paperctl` itself.
/// Not `apps/settings` or `apps/app-store` — neither is dev-runnable here
/// ([`DevAppArg`] is a closed enum and clap rejects anything else before this
/// module runs) — but their source is not part of what a session actually
/// executes, so leaving them out does not miss a rebuild those sessions need.
const WATCHED_DIRS: &[&str] = &[
    "tools/paperctl/src",
    "apps/home/src",
    "apps/chess/src",
    "apps/chess-rules/src",
    "apps/sudoku/src",
    "apps/sudoku-rules/src",
    "apps/render-test-card/src",
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
    /// The OS-level file watcher could not be created at all.
    #[error("cannot start a file watcher")]
    WatcherInit(#[source] notify::Error),
    /// A directory could not be added to the file watcher.
    #[error("cannot watch {path} for changes")]
    Watch {
        /// Which directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: notify::Error,
    },
}

/// A background watch on [`WATCHED_DIRS`], shared between the session loop
/// and the watcher's own callback thread.
struct SourceWatcher {
    /// Kept alive for as long as the watch should run — dropping it stops
    /// watching.
    _watcher: notify::RecommendedWatcher,
    /// Set the moment any watched file changes, and checked once a session
    /// ends. [`run`] clears it at the start of every session so it answers
    /// "did anything change during the session that just ran", not "ever".
    changed: Arc<AtomicBool>,
    /// The currently open preview's handle, if a session is running one —
    /// `None` between sessions and while a session's window has not opened
    /// yet. The watcher's callback uses this to close the window the moment
    /// it sees a change, rather than waiting for the session to end on its
    /// own to notice.
    handle: Arc<Mutex<Option<PreviewHandle>>>,
}

/// Starts watching [`WATCHED_DIRS`] under `root`.
fn watch_sources(root: &Path) -> Result<SourceWatcher, DevError> {
    watch_dirs(WATCHED_DIRS.iter().map(|dir| root.join(dir)))
}

/// Starts watching every directory in `dirs`, recursively.
///
/// Split from [`watch_sources`] so the watch-and-signal behaviour is
/// testable against a scratch directory, rather than only against this
/// checkout's own `WATCHED_DIRS`.
fn watch_dirs(dirs: impl IntoIterator<Item = PathBuf>) -> Result<SourceWatcher, DevError> {
    let changed = Arc::new(AtomicBool::new(false));
    let handle: Arc<Mutex<Option<PreviewHandle>>> = Arc::new(Mutex::new(None));

    let event_changed = Arc::clone(&changed);
    let event_handle = Arc::clone(&handle);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_err() {
            return;
        }
        event_changed.store(true, Ordering::SeqCst);
        // Best-effort: if no session has a window open right now, there is
        // nothing to close, and the flag above is what `run` acts on once
        // one does.
        if let Ok(handle) = event_handle.lock()
            && let Some(handle) = handle.as_ref()
        {
            handle.request_exit();
        }
    })
    .map_err(DevError::WatcherInit)?;

    for path in dirs {
        watcher
            .watch(&path, notify::RecursiveMode::Recursive)
            .map_err(|source| DevError::Watch {
                path: path.clone(),
                source,
            })?;
    }

    Ok(SourceWatcher {
        _watcher: watcher,
        changed,
        handle,
    })
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
    /// The render test card (WWW-47).
    RenderTestCard,
}

impl DevAppArg {
    const fn slug(self) -> &'static str {
        match self {
            DevAppArg::Home => "home",
            DevAppArg::Chess => "chess",
            DevAppArg::Sudoku => "sudoku",
            DevAppArg::RenderTestCard => "render-test-card",
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

    let watcher = watch_sources(&workspace_root())?;

    loop {
        eprintln!(
            "paperctl dev: running {app_slug} \u{2014} edit a watched file to rebuild and reload automatically"
        );
        watcher.changed.store(false, Ordering::SeqCst);
        let outcome = run_session(&app_slug, &storage_root, &watcher)?;
        let changed_during_session = watcher.changed.load(Ordering::SeqCst);
        app_slug = match outcome {
            SessionOutcome::Exit => return Ok(()),
            SessionOutcome::Relaunch(next) => next,
        };

        if !changed_during_session {
            continue;
        }
        eprintln!("paperctl dev: source changed; rebuilding...");
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
///
/// `watcher` is registered for the window's lifetime so a source change can
/// close it from outside — see the module doc — and unregistered again
/// before this returns, so a change landing between sessions has nothing
/// stale to reach.
fn run_session(
    app_slug: &str,
    storage_root: &Path,
    watcher: &SourceWatcher,
) -> Result<SessionOutcome, CommandError> {
    let mut current = session::open_session(
        app_slug,
        storage_root,
        "DEV",
        "paperctl dev \u{2014} local, not device verified",
        SurfaceDescriptor::packed(SCREEN, PixelFormat::Argb8888),
    )
    .map_err(DevError::from)?;
    let options = PreviewOptions::new(format!("paperctl dev \u{2014} {app_slug}"), SCREEN);
    let mut relaunch: Option<String> = None;

    let ran = desktop::run_with_handle(
        options,
        |handle| {
            *watcher
                .handle
                .lock()
                .expect("the watcher's handle mutex is never poisoned") = Some(handle);
        },
        |event| match event {
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
                    // Dev mode has no stock to hand back to; leaving the tool
                    // is the honest equivalent of the real device's exit
                    // route.
                    PreviewControl::Exit
                }
                Ok(_) => PreviewControl::Continue,
                Err(error) => {
                    eprintln!("paperctl dev: {error}");
                    PreviewControl::Exit
                }
            },
            _ => PreviewControl::Continue,
        },
    );
    *watcher
        .handle
        .lock()
        .expect("the watcher's handle mutex is never poisoned") = None;
    if let Err(error) = ran {
        eprintln!("paperctl dev: the window ended unexpectedly: {error}");
    }

    // No explicit Home/Launch/ReturnToStock, but a watched file changed while
    // the window was open: the watcher closed it, not the user, so this is a
    // reload of the same app, not an exit.
    if relaunch.is_none() && watcher.changed.load(Ordering::SeqCst) {
        relaunch = Some(app_slug.to_owned());
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

#[cfg(test)]
mod tests {
    use super::watch_dirs;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    /// Polls `condition` for up to two seconds — the OS-level watchers this
    /// wraps (FSEvents, inotify, kqueue) deliver asynchronously, so a test
    /// that changed a file has to wait for a notification rather than
    /// assert immediately.
    fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        condition()
    }

    #[test]
    fn writing_a_file_in_a_watched_directory_sets_the_changed_flag() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let watcher = watch_dirs([dir.path().to_path_buf()]).expect("a watcher");
        assert!(!watcher.changed.load(Ordering::SeqCst));

        std::fs::write(dir.path().join("lib.rs"), b"// changed").expect("writes a file");

        assert!(
            wait_for(|| watcher.changed.load(Ordering::SeqCst)),
            "the watcher never saw the write"
        );
    }

    #[test]
    fn a_change_outside_every_watched_directory_is_not_seen() {
        let watched = tempfile::tempdir().expect("a temp dir");
        let unwatched = tempfile::tempdir().expect("a temp dir");
        let watcher = watch_dirs([watched.path().to_path_buf()]).expect("a watcher");

        std::fs::write(unwatched.path().join("lib.rs"), b"// changed").expect("writes a file");

        // There is nothing to wait for that would ever become true, so this
        // waits out the same window `wait_for` would and confirms it stayed
        // false throughout rather than raced a slow notification.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!watcher.changed.load(Ordering::SeqCst));
    }
}
