//! The live half of a Home/Chess session: the loopback connection to the app
//! thread, and where its published frames land.
//!
//! Shared between `paperctl dev` (a window on the Mac) and `paperctl run` (the
//! real panel on the device, WWW-6): both hand a real, unmodified
//! [`HomeApp`]/[`ChessApp`] to [`paper_sdk::run`] over a loopback
//! [`UnixStream`] and read the frames it publishes back out of a shared
//! canvas — the same bridge [`dev`](crate::dev) used before this module
//! existed, gated only on `apps` rather than `desktop` so a device build,
//! which has no windowing stack at all, still links it.

use std::convert::Infallible;
use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use paper_chess::ChessApp;
use paper_home::{HomeApp, HomeScreen, ShelfEntry, ShelfGlyph, SystemFact};
use paper_packages::{Manifest, ManifestError};
use paper_protocol::{
    AppId, AppMessage, AppPaths, Capability, Damage, DrawReason, DrawRequest, ExitReason, FrameId,
    Hello, HostMessage, LaunchReason, LifecycleEvent, PixelFormat, PointerEvent, Request, Saved,
    SessionId, SurfaceDescriptor, codec,
};
use paper_sdk::{
    App, Canvas, Context, Event, SCREEN, SaveError, Surface, SurfaceError, SurfaceProvider,
};

const HOME_MANIFEST: &str = include_str!("../../../apps/home/paper.toml");
const CHESS_MANIFEST: &str = include_str!("../../../apps/chess/paper.toml");

/// How long a read on the loopback socket may block waiting for the app
/// thread before this side gives up on it. See [`open_session`].
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a session could not be opened or run to completion.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    /// `app` is not one this loop knows how to run.
    #[error("`{app}` is not something this session can run \u{2014} try `home` or `chess`")]
    UnknownApp {
        /// What was asked for.
        app: String,
    },
    /// A built-in manifest did not parse — a bug in this build, not in
    /// anything the user did.
    #[error("a built-in manifest is invalid; this is a bug in paperclip")]
    Manifest(#[from] ManifestError),
    /// The local app storage directory could not be prepared.
    #[error("cannot prepare the app storage directory {path}")]
    Storage {
        /// Which directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// Setting up the loopback connection failed.
    #[error("cannot set up the session")]
    Io(#[from] std::io::Error),
    /// The loopback connection to the app thread failed.
    #[error("the connection to the app process failed")]
    Transport(#[from] paper_protocol::CodecError),
    /// The app's own session loop ended with an error.
    #[error("the app session failed")]
    Runtime(#[from] paper_sdk::RuntimeError),
}

/// Which app id a `Launch` request names, resolved to the slug this loop's
/// callers use — `"home"` or `"chess"`.
///
/// Both dev and device sessions only ever run the two apps that ship a
/// dev-runnable entry point (see [`open_session`]'s `UnknownApp`), so this is
/// a closed match rather than a catalog lookup.
pub(crate) fn launch_target(id: &AppId) -> Option<&'static str> {
    match id.as_str() {
        "dev.calum.chess" => Some("chess"),
        "dev.calum.home" => Some("home"),
        _ => None,
    }
}

fn home_screen(chess: &Manifest, status: &str, mode_fact: &str) -> HomeScreen {
    HomeScreen {
        entries: vec![
            ShelfEntry::from_manifest(chess, ShelfGlyph::Board),
            ShelfEntry::action("Return to stock", "REMARKABLE", ShelfGlyph::Stock),
        ],
        facts: vec![SystemFact::new("Mode", mode_fact)],
        pressed: None,
        status: status.to_owned(),
    }
}

/// The live half of a session: the loopback connection to the app thread, and
/// where its published frames land.
pub(crate) struct Session {
    host: UnixStream,
    next_frame: FrameId,
    shared: Arc<Mutex<Canvas>>,
    app_thread: thread::JoinHandle<Result<paper_sdk::Outcome, paper_sdk::RuntimeError>>,
}

impl Session {
    /// The canvas the app most recently published.
    pub(crate) fn frame(&self) -> Canvas {
        self.shared
            .lock()
            .expect("the app thread does not panic while holding this lock")
            .clone()
    }

    /// The read timeout actually in force on the loopback socket — a way for
    /// a test to check [`open_session`]'s wiring without needing a peer that
    /// actually hangs.
    #[cfg(test)]
    fn read_timeout(&self) -> Option<Duration> {
        self.host
            .read_timeout()
            .expect("querying a socket's own timeout does not fail")
    }

    /// Sends a pointer event and waits for the frame it produces.
    pub(crate) fn pointer(&mut self, event: PointerEvent) -> Result<Option<Request>, SessionError> {
        codec::write_message(&mut self.host, &HostMessage::Pointer(event))?;
        self.request_frame(DrawReason::AppRequested)
    }

    /// Asks for a frame and pumps messages until it arrives, returning the
    /// last [`Request`] seen along the way, if any — a tap can produce one
    /// before the frame that shows its effect does.
    pub(crate) fn request_frame(
        &mut self,
        reason: DrawReason,
    ) -> Result<Option<Request>, SessionError> {
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
                    eprintln!("paperctl: [app] {diagnostic:?}");
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
    pub(crate) fn shutdown(mut self, reason: ExitReason) -> Result<bool, SessionError> {
        let event = LifecycleEvent::prepare_to_exit(reason, Duration::from_secs(5));
        codec::write_message(&mut self.host, &HostMessage::Lifecycle(event))?;
        let saved = loop {
            match codec::read_message::<_, AppMessage>(&mut self.host)? {
                AppMessage::Saved(Saved { ok, .. }) => break ok,
                AppMessage::Diagnostic(diagnostic) => {
                    eprintln!("paperctl: [app] {diagnostic:?}");
                }
                AppMessage::Ready(_) | AppMessage::Frame(_) | AppMessage::Request(_) => {}
                _ => {}
            }
        };
        match self.app_thread.join() {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => eprintln!("paperctl: the app session ended with an error: {error}"),
            Err(_) => eprintln!("paperctl: the app thread panicked"),
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

/// Opens a session for `app_slug`, under `storage_root`, and waits for its
/// first frame.
///
/// `status` and `mode_fact` become the Home shelf's status line and one
/// system fact — the visible difference between "this is `paperctl dev`,
/// local, not device verified" and "this is `paperctl run`, presenting on the
/// panel", so a screenshot of either alone says which one it is.
pub(crate) fn open_session(
    app_slug: &str,
    storage_root: &Path,
    status: &str,
    mode_fact: &str,
) -> Result<Session, SessionError> {
    let chess_manifest = Manifest::parse(CHESS_MANIFEST)?;
    let (manifest_text, app) = match app_slug {
        "chess" => (CHESS_MANIFEST, DevApp::Chess(Box::new(ChessApp::new()))),
        "home" => (
            HOME_MANIFEST,
            DevApp::Home(HomeApp::new(home_screen(
                &chess_manifest,
                status,
                mode_fact,
            ))),
        ),
        other => {
            return Err(SessionError::UnknownApp {
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
        fs::create_dir_all(dir).map_err(|source| SessionError::Storage {
            path: dir.clone(),
            source,
        })?;
    }

    let (host, app_side) = UnixStream::pair()?;
    // Bounded, not unbounded: a hung app thread (§17 — "an app that ignores
    // termination") must not block `read_message` forever. On the device
    // that read runs inside the process holding the vendor engine open, and
    // that process not exiting is the one failure `paper_device::hold`
    // cannot route around — the out-of-process watchdog restarts Xochitl on
    // a timer, but only after this process's lock is actually released.
    // Long enough that no legitimate draw or save ever trips it.
    host.set_read_timeout(Some(READ_TIMEOUT))?;
    let reader = app_side.try_clone()?;
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

/// A [`Surface`] that shares its pixels with whatever is watching `shared`,
/// rather than a socket — the app thread and its host live in the same
/// process, so there is nothing to serialise.
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

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use paper_protocol::{DrawReason, FrameId};
    use paper_sdk::{Canvas, SCREEN};

    use super::{READ_TIMEOUT, Session};

    /// A stand-in for an app whose event/draw loop never returns — the fault
    /// `paper-fault-app`'s `hang` mode injects. Nothing writes to `app_side`
    /// and nothing reads from it; the thread just outlives the test.
    fn hung_session() -> Session {
        let (host, app_side) = UnixStream::pair().expect("pairs on this platform");
        host.set_read_timeout(Some(READ_TIMEOUT))
            .expect("sets a read timeout on a fresh socket");
        let app_thread = thread::spawn(move || {
            let _keep_open = app_side;
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        });
        Session {
            host,
            next_frame: FrameId::FIRST,
            shared: Arc::new(Mutex::new(Canvas::new(SCREEN).expect("allocates"))),
            app_thread,
        }
    }

    #[test]
    fn a_request_to_a_hung_app_gives_up_within_the_read_timeout_rather_than_blocking_forever() {
        let mut session = hung_session();
        let started = Instant::now();

        let result = session.request_frame(DrawReason::First);

        assert!(
            result.is_err(),
            "a peer that never replies must not read Ok"
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < READ_TIMEOUT + Duration::from_secs(5),
            "took {elapsed:?}, expected close to the {READ_TIMEOUT:?} read timeout \u{2014} \
             the process holding the vendor engine open must exit promptly on a hang, not \
             block forever (WWW-6 \u{a7}17)"
        );
    }

    /// [`hung_session`] proves the timeout works once it is set; this proves
    /// [`super::open_session`] actually sets it on a real session, rather
    /// than the two ever drifting apart.
    #[test]
    fn open_session_leaves_the_hang_guard_armed_on_a_real_session() {
        let root = std::env::temp_dir().join(format!(
            "paperctl-session-test-{}-{}",
            std::process::id(),
            "open-session-leaves-the-hang-guard-armed"
        ));
        let session = super::open_session("home", &root, "TEST", "unit test").expect("opens");

        assert_eq!(session.read_timeout(), Some(READ_TIMEOUT));

        session
            .shutdown(paper_protocol::ExitReason::ReturnToStock)
            .expect("a real Home session shuts down cleanly");
        let _ = std::fs::remove_dir_all(&root);
    }
}
