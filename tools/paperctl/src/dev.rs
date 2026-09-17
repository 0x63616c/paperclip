//! `paperctl dev` — the desktop dev loop (§14, §15).
//!
//! Runs a real app — [`paper_home::HomeApp`] or [`paper_chess::ChessApp`] —
//! through the actual wire protocol [`paper_sdk::run`] speaks, in a real
//! window, with isolated local storage that survives a restart. Watches this
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
//! [`paper_sdk::run`] speaks to a host over a byte stream; here that stream
//! is a loopback [`UnixStream`] pair, with this module playing the host on
//! one end and the app running for real — unmodified, the same code a
//! launched process on the device runs — on a background thread on the
//! other. [`BridgeSurfaces`] is the surface: the app draws into its own
//! private [`Canvas`], and `publish` clones it into an `Arc<Mutex<Canvas>>`
//! this module reads from whenever `paper_sdk::desktop` asks for a frame.
//! Every pointer tap is forwarded and followed by an explicit draw request,
//! and the reply is read for synchronously, so a repaint the tap causes is
//! already in the shared canvas by the time the window redraws — no
//! cross-thread wake needed for that part, because
//! `paper_sdk::desktop::Preview::pointer` already calls
//! `window.request_redraw()` itself after every handled tap.

use std::convert::Infallible;
use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use paper_chess::ChessApp;
use paper_home::{HomeApp, HomeScreen, ShelfEntry, ShelfGlyph, SystemFact};
use paper_packages::{Manifest, ManifestError};
use paper_protocol::{
    AppId, AppMessage, AppPaths, Capability, Damage, DrawReason, DrawRequest, ExitReason, FrameId,
    Hello, HostMessage, LaunchReason, LifecycleEvent, PixelFormat, PointerEvent, Request, Saved,
    SessionId, SurfaceDescriptor, codec,
};
use paper_sdk::desktop::{self, PreviewControl, PreviewEvent, PreviewOptions};
use paper_sdk::{
    App, Canvas, Context, Event, SCREEN, SaveError, Surface, SurfaceError, SurfaceProvider,
};

use crate::error::CommandError;

const HOME_MANIFEST: &str = include_str!("../../../apps/home/paper.toml");
const CHESS_MANIFEST: &str = include_str!("../../../apps/chess/paper.toml");

/// Source directories `paperctl dev` watches for a reason to rebuild.
///
/// Everything the `apps` feature can reach: the two dev-runnable app crates,
/// the rules core, the SDK and protocol underneath them, and `paperctl`
/// itself. Not `apps/settings` or `apps/app-store` — neither is dev-runnable
/// here (see [`DevError::UnknownApp`]) — but their source is not part of
/// what a Home/Chess session actually executes, so leaving them out does not
/// miss a rebuild those sessions need.
const WATCHED_DIRS: &[&str] = &[
    "tools/paperctl/src",
    "apps/home/src",
    "apps/chess/src",
    "apps/chess-rules/src",
    "platform/sdk/src",
    "platform/protocol/src",
    "platform/packages/src",
];

/// Why a `paperctl dev` session could not run.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DevError {
    /// `app` is not one `paperctl dev` knows how to run.
    #[error("`{app}` is not something `paperctl dev` can run — try `home` or `chess`")]
    UnknownApp {
        /// What was asked for.
        app: String,
    },
    /// A built-in manifest did not parse — a bug in this build, not in
    /// anything the user did.
    #[error("a built-in manifest is invalid; this is a bug in paperclip")]
    Manifest(#[from] ManifestError),
    /// The local dev storage directory could not be prepared.
    #[error("cannot prepare the dev storage directory {path}")]
    Storage {
        /// Which directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The loopback connection to the app thread failed.
    #[error("the connection to the app process failed")]
    Transport(#[from] paper_protocol::CodecError),
    /// The app's own session loop ended with an error.
    #[error("the app session failed")]
    Runtime(#[from] paper_sdk::RuntimeError),
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
}

impl DevAppArg {
    const fn slug(self) -> &'static str {
        match self {
            DevAppArg::Home => "home",
            DevAppArg::Chess => "chess",
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
    let mut session = open_session(app_slug, storage_root)?;
    let options = PreviewOptions::new(format!("paperctl dev \u{2014} {app_slug}"), SCREEN);
    let mut relaunch: Option<String> = None;

    let ran = desktop::run(options, |event| match event {
        PreviewEvent::Render(canvas) => {
            if let Ok(frame) = session.shared.lock() {
                *canvas = frame.clone();
            }
            PreviewControl::Continue
        }
        PreviewEvent::Pointer(pointer) => match session.pointer(pointer) {
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
    match session.shutdown(reason) {
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
    match id.as_str() {
        "dev.calum.chess" => Some("chess".to_owned()),
        "dev.calum.home" => Some("home".to_owned()),
        other => {
            eprintln!(
                "paperctl dev: `{other}` is not one of the apps this dev loop can run (chess, home); stopping"
            );
            None
        }
    }
}

fn home_screen(chess: &Manifest) -> HomeScreen {
    HomeScreen {
        entries: vec![
            ShelfEntry::from_manifest(chess, ShelfGlyph::Board),
            ShelfEntry::action("Return to stock", "REMARKABLE", ShelfGlyph::Stock),
        ],
        facts: vec![SystemFact::new(
            "Mode",
            "paperctl dev \u{2014} local, not device verified",
        )],
        pressed: None,
        status: "DEV".to_owned(),
    }
}

/// The live half of a dev session: the loopback connection to the app
/// thread, and where its published frames land.
struct Session {
    host: UnixStream,
    next_frame: FrameId,
    shared: Arc<Mutex<Canvas>>,
    app_thread: thread::JoinHandle<Result<paper_sdk::Outcome, paper_sdk::RuntimeError>>,
}

impl Session {
    /// Sends a pointer event and waits for the frame it produces.
    fn pointer(&mut self, event: PointerEvent) -> Result<Option<Request>, DevError> {
        codec::write_message(&mut self.host, &HostMessage::Pointer(event))?;
        self.request_frame(DrawReason::AppRequested)
    }

    /// Asks for a frame and pumps messages until it arrives, returning the
    /// last [`Request`] seen along the way, if any — a tap can produce one
    /// before the frame that shows its effect does.
    fn request_frame(&mut self, reason: DrawReason) -> Result<Option<Request>, DevError> {
        let frame = self.next_frame;
        self.next_frame = self.next_frame.next();
        codec::write_message(
            &mut self.host,
            &HostMessage::Draw(DrawRequest {
                frame,
                reason,
                viewport: SCREEN,
            }),
        )?;
        let mut request = None;
        loop {
            match codec::read_message::<_, AppMessage>(&mut self.host)? {
                AppMessage::Frame(_) => return Ok(request),
                AppMessage::Request(seen) => request = Some(seen),
                AppMessage::Diagnostic(diagnostic) => {
                    eprintln!("paperctl dev: [app] {diagnostic:?}");
                }
                AppMessage::Ready(_) | AppMessage::Saved(_) => {}
                // Non-exhaustive: a message this build has never heard of is
                // safely ignorable here, same as an unknown `Request`.
                _ => {}
            }
        }
    }

    /// Asks the app to save and exit, and waits for it to say it did.
    ///
    /// Consumes `self`: this is the one door out of a session, and nothing
    /// here is meaningful to call twice.
    fn shutdown(mut self, reason: ExitReason) -> Result<bool, DevError> {
        let event = LifecycleEvent::prepare_to_exit(reason, Duration::from_secs(5));
        codec::write_message(&mut self.host, &HostMessage::Lifecycle(event))?;
        let saved = loop {
            match codec::read_message::<_, AppMessage>(&mut self.host)? {
                AppMessage::Saved(Saved { ok, .. }) => break ok,
                AppMessage::Diagnostic(diagnostic) => {
                    eprintln!("paperctl dev: [app] {diagnostic:?}");
                }
                AppMessage::Ready(_) | AppMessage::Frame(_) | AppMessage::Request(_) => {}
                _ => {}
            }
        };
        match self.app_thread.join() {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                eprintln!("paperctl dev: the app session ended with an error: {error}")
            }
            Err(_) => eprintln!("paperctl dev: the app thread panicked"),
        }
        Ok(saved)
    }
}

/// Either dev-runnable app, so [`open_session`] can hand `paper_sdk::run` one
/// concrete type instead of a trait object — `App` has no forwarding impl for
/// `Box<dyn App>`, and adding one is a bigger change to a shared crate than
/// this local enum is.
enum DevApp {
    Home(HomeApp),
    Chess(Box<ChessApp>),
}

impl App for DevApp {
    type Completion = Infallible;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> paper_protocol::Action {
        match self {
            DevApp::Home(app) => app.event(event, context),
            DevApp::Chess(app) => app.event(event, context),
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>) {
        match self {
            DevApp::Home(app) => app.draw(canvas, context),
            DevApp::Chess(app) => app.draw(canvas, context),
        }
    }

    fn save(&mut self, context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        match self {
            DevApp::Home(app) => app.save(context),
            DevApp::Chess(app) => app.save(context),
        }
    }

    fn damage(&self) -> Damage {
        match self {
            DevApp::Home(app) => app.damage(),
            DevApp::Chess(app) => app.damage(),
        }
    }
}

fn open_session(app_slug: &str, storage_root: &Path) -> Result<Session, DevError> {
    let chess_manifest = Manifest::parse(CHESS_MANIFEST)?;
    let (manifest_text, app) = match app_slug {
        "chess" => (CHESS_MANIFEST, DevApp::Chess(Box::new(ChessApp::new()))),
        "home" => (
            HOME_MANIFEST,
            DevApp::Home(HomeApp::new(home_screen(&chess_manifest))),
        ),
        other => {
            return Err(DevError::UnknownApp {
                app: other.to_owned(),
            });
        }
    };
    let manifest = Manifest::parse(manifest_text)?;

    let app_storage = storage_root.join(app_slug);
    let private = app_storage.join("private");
    let had_state = private.exists();
    let assets = app_storage.join("assets");
    let temp = app_storage.join("temp");
    for dir in [&assets, &private, &temp] {
        fs::create_dir_all(dir).map_err(|source| DevError::Storage {
            path: dir.clone(),
            source,
        })?;
    }

    let (host, app_side) = UnixStream::pair().map_err(DevError::Spawn)?;
    let reader = app_side.try_clone().map_err(DevError::Spawn)?;
    let shared = Arc::new(Mutex::new(
        Canvas::new(SCREEN).expect("SCREEN is always a valid canvas size"),
    ));
    let surfaces = BridgeSurfaces {
        shared: shared.clone(),
    };
    let app_thread = thread::spawn(move || paper_sdk::run(app, reader, app_side, surfaces));

    let mut host = host;
    let hello = Hello {
        protocol: paper_protocol::CURRENT,
        session: SessionId::new(1),
        app: manifest.id().clone(),
        version: manifest.version().clone(),
        launch: if had_state {
            LaunchReason::Restored
        } else {
            LaunchReason::Fresh
        },
        surface: SurfaceDescriptor::packed(SCREEN, PixelFormat::Argb8888),
        capabilities: vec![Capability::Storage],
        paths: AppPaths {
            assets,
            private,
            temp,
            shared: Vec::new(),
        },
    };
    codec::write_message(&mut host, &HostMessage::Hello(hello))?;
    let _ready: AppMessage = codec::read_message(&mut host)?;

    let mut session = Session {
        host,
        next_frame: FrameId::FIRST,
        shared,
        app_thread,
    };
    session.request_frame(DrawReason::First)?;
    Ok(session)
}

/// A [`Surface`] that shares its pixels with whatever is watching
/// `shared`, rather than a socket — the app thread and the window live in
/// the same process, so there is nothing to serialise.
struct BridgeSurface {
    canvas: Canvas,
    shared: Arc<Mutex<Canvas>>,
}

impl Surface for BridgeSurface {
    fn canvas(&mut self) -> &mut Canvas {
        &mut self.canvas
    }

    fn publish(&mut self, _damage: &Damage) -> Result<(), SurfaceError> {
        if let Ok(mut frame) = self.shared.lock() {
            *frame = self.canvas.clone();
        }
        Ok(())
    }
}

struct BridgeSurfaces {
    shared: Arc<Mutex<Canvas>>,
}

impl SurfaceProvider for BridgeSurfaces {
    type Surface = BridgeSurface;

    fn open(&mut self, descriptor: &SurfaceDescriptor) -> Result<Self::Surface, SurfaceError> {
        if descriptor.format != PixelFormat::Argb8888 {
            return Err(SurfaceError::UnsupportedFormat {
                format: descriptor.format,
            });
        }
        if descriptor.bytes().is_none() {
            return Err(SurfaceError::Unusable {
                extent: descriptor.extent,
                stride: descriptor.stride_bytes,
            });
        }
        let canvas = Canvas::new(descriptor.extent).ok_or(SurfaceError::Allocation {
            width: descriptor.extent.width,
            height: descriptor.extent.height,
        })?;
        Ok(BridgeSurface {
            canvas,
            shared: self.shared.clone(),
        })
    }
}
