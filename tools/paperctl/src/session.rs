//! The live half of a dev or device session: the loopback connection to the
//! app thread, and where its published frames land.
//!
//! Shared between `paperctl dev` (a window on the Mac) and `paperctl run` (the
//! real panel on the device, WWW-6): both hand a real, unmodified
//! [`HomeApp`]/[`ChessApp`]/[`SettingsApp`]/[`SudokuApp`]/[`AppStoreApp`] to
//! [`paper_sdk::run`] over a loopback [`UnixStream`] and read the frames it
//! publishes back out of a shared canvas — the same bridge
//! [`dev`](crate::dev) used before this module existed, gated only on `apps`
//! rather than `desktop` so a device build, which has no windowing stack at
//! all, still links it.
//!
//! [`DevApp`] wraps Home, Chess, Settings and Sudoku behind one `App` impl,
//! so [`open_session_with`] can hand `paper_sdk::run` a single concrete type.
//! The App Store cannot join it: unlike the other four,
//! [`AppStoreApp::Completion`](paper_sdk::App::Completion) is not
//! [`Infallible`] — it carries install progress — and [`paper_sdk::Context`]
//! is a distinct type per completion type, with no public constructor outside
//! `paper_sdk` to convert between them. [`open_app_store_session`] builds and
//! spawns it separately instead. What both paths share — preparing storage,
//! opening the socket, the `Hello`/`Ready` handshake — is
//! [`open_session_common`], because `paper_sdk::run`'s return type does not
//! depend on `A` at all: whichever app is inside it, the thread it runs on
//! joins through the same [`Session`].

use std::convert::Infallible;
use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use paper_app_store::{AppStoreApp, AppStoreScreen, PackagesSource, SourceError, StoreSource};
use paper_chess::ChessApp;
use paper_home::{HomeApp, HomeScreen, ShelfEntry, ShelfGlyph, SystemFact};
use paper_packages::install::NothingIsRunning;
use paper_packages::signing::TrustedKeys;
use paper_packages::store::{Layout, StoreError};
use paper_packages::{InstallPolicy, InstalledApp, Manifest, ManifestError};
use paper_protocol::{
    AppId, AppMessage, AppPaths, Capability, Damage, DrawReason, DrawRequest, ExitReason, FrameId,
    Hello, HostMessage, LaunchReason, LifecycleEvent, PixelFormat, PointerEvent, Request, Saved,
    SessionId, SurfaceDescriptor, codec,
};
use paper_sdk::{
    App, Canvas, Context, Event, SCREEN, SaveError, Surface, SurfaceError, SurfaceProvider,
};
use paper_settings::{LiveHost, SettingsApp};
use paper_sudoku::SudokuApp;

const HOME_MANIFEST: &str = include_str!("../../../apps/home/paper.toml");
const CHESS_MANIFEST: &str = include_str!("../../../apps/chess/paper.toml");
const SETTINGS_MANIFEST: &str = include_str!("../../../apps/settings/paper.toml");
const SUDOKU_MANIFEST: &str = include_str!("../../../apps/sudoku/paper.toml");
const APP_STORE_MANIFEST: &str = include_str!("../../../apps/app-store/paper.toml");

/// How long a read on the loopback socket may block waiting for the app
/// thread before this side gives up on it. See [`open_session`].
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a session could not be opened or run to completion.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    /// `app` is not one this loop knows how to run.
    #[error(
        "`{app}` is not something this session can run \u{2014} try `home`, `chess`, `settings`, \
         `sudoku` or `app-store`"
    )]
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
    /// The App Store's own install store could not be prepared.
    #[error("cannot prepare the Paperclip store")]
    PackageStore(#[from] StoreError),
    /// The App Store's package source could not be built.
    #[error("the App Store's package source could not be built")]
    PackageSource(#[from] SourceError),
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
/// callers use — `"home"`, `"chess"`, `"settings"`, `"sudoku"` or
/// `"app-store"`.
///
/// Dev and device sessions only ever run the apps that ship a dev-runnable
/// entry point (see [`open_session`]'s `UnknownApp`), so this is a closed
/// match rather than a catalog lookup.
pub(crate) fn launch_target(id: &AppId) -> Option<&'static str> {
    match id.as_str() {
        "dev.calum.chess" => Some("chess"),
        "dev.calum.home" => Some("home"),
        "dev.calum.settings" => Some("settings"),
        "dev.calum.sudoku" => Some("sudoku"),
        "dev.calum.app-store" => Some("app-store"),
        _ => None,
    }
}

/// The shelf an interactive session shows, built from the compiled-in
/// manifests.
///
/// `pub(crate)` so `screens::golden::both_shelves_offer_the_same_apps` can
/// compare it against the shelf `screenshot` and `open` render, rather than
/// against a list of names someone has to remember to update — which is the
/// failure that put Settings on one shelf and not the other.
pub(crate) fn home_screen(status: &str, mode_fact: &str) -> Result<HomeScreen, SessionError> {
    let chess = Manifest::parse(CHESS_MANIFEST)?;
    let settings = Manifest::parse(SETTINGS_MANIFEST)?;
    let app_store = Manifest::parse(APP_STORE_MANIFEST)?;
    Ok(HomeScreen {
        entries: vec![
            ShelfEntry::from_manifest(&chess, ShelfGlyph::Board),
            ShelfEntry::from_manifest(&settings, ShelfGlyph::Gear),
            ShelfEntry::from_manifest(&app_store, ShelfGlyph::Store),
            ShelfEntry::action("Return to stock", "REMARKABLE", ShelfGlyph::Stock),
        ],
        facts: vec![SystemFact::new("Mode", mode_fact)],
        pressed: None,
        status: status.to_owned(),
    })
}

/// The live half of a session: the loopback connection to the app thread, and
/// where its published frames land.
pub(crate) struct Session {
    host: UnixStream,
    next_frame: FrameId,
    shared: Arc<Mutex<Canvas>>,
    /// What the app said changed, accumulated since the last time the
    /// presenter took it.
    ///
    /// Only the device present loop reads it, and that is Linux-only, so on a
    /// Mac build this field is written and never read. That is the honest
    /// shape rather than a lie to the lint.
    /// `None` means nothing has been published — which is not the same as
    /// "everything changed", and is the difference between a panel update and
    /// no panel update at all.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    damage: Arc<Mutex<Option<Damage>>>,
    app_thread: thread::JoinHandle<Result<paper_sdk::Outcome, paper_sdk::RuntimeError>>,
}

impl Session {
    /// The canvas the app most recently published.
    ///
    /// Clones the whole surface. Prefer [`Self::with_frame`] on any path that
    /// runs per input event: at panel size this is a 14 MB memcpy, and doing
    /// it per touch sample is most of what made an interactive session
    /// unusable.
    pub(crate) fn frame(&self) -> Canvas {
        self.shared
            .lock()
            .expect("the app thread does not panic while holding this lock")
            .clone()
    }

    /// Runs `f` against the published canvas without copying it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn with_frame<R>(&self, f: impl FnOnce(&Canvas) -> R) -> R {
        let frame = self
            .shared
            .lock()
            .expect("the app thread does not panic while holding this lock");
        f(&frame)
    }

    /// Takes what the app reported as changed since this was last called.
    ///
    /// `None` means the app published nothing, so there is nothing to put on
    /// the panel — the cheapest possible update.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn take_damage(&self) -> Option<Damage> {
        self.damage
            .lock()
            .expect("the app thread does not panic while holding this lock")
            .take()
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

/// One of the three dev-runnable apps that share a completion type, so
/// [`open_session_with`] can hand `paper_sdk::run` one concrete type instead
/// of a trait object — `App` has no forwarding impl for `Box<dyn App>`, and
/// adding one is a bigger change to a shared crate than this local enum is.
/// The App Store does not fit here; see the module doc for why.
pub(crate) enum DevApp {
    /// The shelf.
    Home(HomeApp),
    /// Chess, on the real rules core.
    Chess(Box<ChessApp>),
    /// Settings, over whichever [`paper_settings::SettingsHost`] the caller
    /// built — the real store for a session, a fixture for a test.
    Settings(Box<SettingsApp>),
    /// Sudoku, on the real rules core.
    Sudoku(Box<SudokuApp>),
}

impl DevApp {
    /// The slug this app is addressed by on the command line and in a
    /// `Launch` request.
    fn slug(&self) -> &'static str {
        match self {
            DevApp::Home(_) => "home",
            DevApp::Chess(_) => "chess",
            DevApp::Settings(_) => "settings",
            DevApp::Sudoku(_) => "sudoku",
        }
    }

    /// The manifest compiled in for it — the same file the app ships.
    fn manifest_text(&self) -> &'static str {
        match self {
            DevApp::Home(_) => HOME_MANIFEST,
            DevApp::Chess(_) => CHESS_MANIFEST,
            DevApp::Settings(_) => SETTINGS_MANIFEST,
            DevApp::Sudoku(_) => SUDOKU_MANIFEST,
        }
    }
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
            DevApp::Settings(app) => app.event(event, context),
            DevApp::Sudoku(app) => app.event(event, context),
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>) {
        match self {
            DevApp::Home(app) => app.draw(canvas, context),
            DevApp::Chess(app) => app.draw(canvas, context),
            DevApp::Settings(app) => app.draw(canvas, context),
            DevApp::Sudoku(app) => app.draw(canvas, context),
        }
    }

    fn save(&mut self, context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        match self {
            DevApp::Home(app) => app.save(context),
            DevApp::Chess(app) => app.save(context),
            DevApp::Settings(app) => app.save(context),
            DevApp::Sudoku(app) => app.save(context),
        }
    }

    fn damage(&self) -> Damage {
        match self {
            DevApp::Home(app) => app.damage(),
            DevApp::Chess(app) => app.damage(),
            DevApp::Settings(app) => app.damage(),
            DevApp::Sudoku(app) => app.damage(),
        }
    }
}

/// Builds the App Store's own package source: an install store rooted at
/// `PAPERCLIP_ROOT` (or the device path, [`Layout::from_environment`]),
/// reading a `catalog` directory alongside it.
///
/// There is nowhere yet for a person to configure a different catalog or
/// trusted keys for an interactive session — `apps/settings/src/host.rs`
/// records the same gap for Settings. A missing or empty catalog is the
/// already-supported "nothing offered" state
/// ([`AppStoreScreen::catalog_summary`]), not a failure, so this still runs
/// usefully without one; pointing `PAPERCLIP_ROOT` at a store that has a real
/// `catalog/` directory underneath it is how the App Store is exercised end
/// to end today (see `apps/app-store/tests/`).
fn app_store_source(manifest: &Manifest) -> Result<PackagesSource, SessionError> {
    let mut policy = InstallPolicy::deny_all();
    policy.allow(manifest.id(), Capability::Packages);
    let caller = InstalledApp::install(manifest.clone(), &policy);

    let layout = Layout::from_environment();
    layout.ensure()?;
    let catalog = layout.root().join("catalog");
    Ok(PackagesSource::for_app(
        layout,
        policy,
        catalog,
        TrustedKeys::none(),
        &caller,
        // `paperctl` drives a store from outside the running system, the
        // same as `paperctl install`/`list` (`tools/paperctl/src/install.rs`):
        // when the host gains a supervisor (WWW-4) it supplies the real
        // answer, and claiming to know one here would be inventing it.
        Box::new(NothingIsRunning),
    )?)
}

/// Opens a session for `app_slug`, under `storage_root`, and waits for its
/// first frame.
///
/// `status` and `mode_fact` become the Home shelf's status line and one
/// system fact — the visible difference between "this is `paperctl dev`,
/// local, not device verified" and "this is `paperctl run`, presenting on the
/// panel", so a screenshot of either alone says which one it is. Neither is
/// used by Settings, Sudoku or the App Store, which have no shelf and no
/// status line of their own.
pub(crate) fn open_session(
    app_slug: &str,
    storage_root: &Path,
    status: &str,
    mode_fact: &str,
) -> Result<Session, SessionError> {
    match app_slug {
        "chess" => open_session_with(DevApp::Chess(Box::new(ChessApp::new())), storage_root),
        "home" => open_session_with(
            DevApp::Home(HomeApp::new(home_screen(status, mode_fact)?)),
            storage_root,
        ),
        // The real store, not the fixture the desktop preview draws: a
        // Settings page that invented its numbers would be worse than one
        // that reports an empty store, which is what a Mac with no
        // `PAPERCLIP_ROOT` honestly has. See `paper_settings::LiveHost`.
        "settings" => open_session_with(
            DevApp::Settings(Box::new(SettingsApp::new(LiveHost::new(
                Layout::from_environment(),
            )))),
            storage_root,
        ),
        "sudoku" => open_session_with(DevApp::Sudoku(Box::new(SudokuApp::new())), storage_root),
        "app-store" => open_app_store_session(storage_root),
        other => Err(SessionError::UnknownApp {
            app: other.to_owned(),
        }),
    }
}

/// Opens a session on an app the caller already built, under `storage_root`,
/// and waits for its first frame.
///
/// [`open_session`] is this with the app chosen by slug. Taking a [`DevApp`]
/// directly is what lets a test run the same session loop over an app it
/// constructed itself — a Settings over a fixture Host, say — rather than
/// whatever the environment happens to hold.
pub(crate) fn open_session_with(app: DevApp, storage_root: &Path) -> Result<Session, SessionError> {
    let app_slug = app.slug();
    let manifest = Manifest::parse(app.manifest_text())?;
    open_session_common(
        app_slug,
        &manifest,
        vec![Capability::Storage],
        storage_root,
        move |reader, app_side, surfaces| {
            thread::spawn(move || paper_sdk::run(app, reader, app_side, surfaces))
        },
    )
}

/// Opens an App Store session — the App Store's counterpart to
/// [`open_session_with`], built and spawned on its own rather than through
/// [`DevApp`] for the reason the module doc gives.
fn open_app_store_session(storage_root: &Path) -> Result<Session, SessionError> {
    let manifest = Manifest::parse(APP_STORE_MANIFEST)?;
    let source = app_store_source(&manifest)?;
    let screen = AppStoreScreen::new(source.inventory()?);
    let app = AppStoreApp::new(screen, Arc::new(source));
    open_session_common(
        "app-store",
        &manifest,
        // `packages`, not `storage` — the App Store never calls
        // `context.storage()`.
        vec![Capability::Packages],
        storage_root,
        move |reader, app_side, surfaces| {
            thread::spawn(move || paper_sdk::run(app, reader, app_side, surfaces))
        },
    )
}

/// Everything common to opening a session, whichever app ends up inside it:
/// preparing its storage directories, opening the loopback socket, the
/// `Hello`/`Ready` handshake, and the first frame.
///
/// `spawn` is handed the reader, the writer and the bridged surface, and
/// returns the thread the app runs on — a closure rather than a value,
/// because [`open_session_with`] and [`open_app_store_session`] each build a
/// differently-typed `App` and `paper_sdk::run`'s return type does not depend
/// on which one, so there is nothing generic to hand back except the
/// `JoinHandle` itself (see the module doc).
fn open_session_common(
    app_slug: &str,
    manifest: &Manifest,
    capabilities: Vec<Capability>,
    storage_root: &Path,
    spawn: impl FnOnce(
        UnixStream,
        UnixStream,
        BridgeSurfaces,
    ) -> thread::JoinHandle<Result<paper_sdk::Outcome, paper_sdk::RuntimeError>>,
) -> Result<Session, SessionError> {
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
    let damage_slot: Arc<Mutex<Option<Damage>>> = Arc::new(Mutex::new(None));
    let shared = Arc::new(Mutex::new(
        Canvas::new(SCREEN).expect("SCREEN is always a valid canvas size"),
    ));
    let surfaces = BridgeSurfaces {
        shared: shared.clone(),
        damage: damage_slot.clone(),
    };
    let app_thread = spawn(reader, app_side, surfaces);

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
        capabilities,
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
        damage: damage_slot,
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
    damage: Arc<Mutex<Option<Damage>>>,
}

impl Surface for BridgeSurface {
    fn canvas(&mut self) -> &mut Canvas {
        &mut self.canvas
    }

    fn publish(&mut self, damage: &Damage) -> Result<(), SurfaceError> {
        if let Ok(mut frame) = self.shared.lock() {
            *frame = self.canvas.clone();
        }
        /* Accumulate rather than overwrite: several publishes can land between
         * two presents, and the panel must be told about all of them. Merging
         * into Full is deliberately sticky — once anything claims the whole
         * surface, a partial update would leave the rest stale. */
        if let Ok(mut slot) = self.damage.lock() {
            *slot = Some(match (slot.take(), damage) {
                (None, fresh) => fresh.clone(),
                (Some(Damage::Full), _) | (Some(_), Damage::Full) => Damage::Full,
                (Some(Damage::Regions { regions: mut have }), Damage::Regions { regions: add }) => {
                    have.extend_from_slice(add);
                    Damage::Regions { regions: have }
                }
                /* A damage kind this build does not know about cannot be
                 * narrowed safely, so treat it as the whole surface. */
                (Some(_), _) => Damage::Full,
            });
        }
        Ok(())
    }
}

struct BridgeSurfaces {
    shared: Arc<Mutex<Canvas>>,
    damage: Arc<Mutex<Option<Damage>>>,
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
            damage: self.damage.clone(),
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
            damage: Arc::new(Mutex::new(None)),
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
        let root = session_root("open-session-leaves-the-hang-guard-armed");
        let session = super::open_session("home", &root, "TEST", "unit test").expect("opens");

        assert_eq!(session.read_timeout(), Some(READ_TIMEOUT));

        session
            .shutdown(paper_protocol::ExitReason::ReturnToStock)
            .expect("a real Home session shuts down cleanly");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every app slug this loop accepts, opened for real.
    ///
    /// `paperctl dev --app sudoku` and `paperctl run --app sudoku` both reach
    /// the app through [`super::open_session`], so what would break — a
    /// missing `DevApp` arm, a manifest that does not parse, storage
    /// directories that are not created — breaks here rather than on the
    /// panel with the display taken over.
    #[test]
    fn every_runnable_app_opens_a_real_session_and_draws_something() {
        for slug in ["home", "chess", "settings", "sudoku"] {
            let root = session_root(slug);
            let session =
                super::open_session(slug, &root, "TEST", "unit test").expect("the session opens");
            assert!(
                session.frame().ink_coverage() > 0.0,
                "{slug}'s first frame is blank"
            );
            session
                .shutdown(paper_protocol::ExitReason::ReturnToStock)
                .expect("the session shuts down cleanly");
            let _ = std::fs::remove_dir_all(&root);
        }

        assert!(
            super::open_session("solitaire", &session_root("unknown"), "TEST", "unit test")
                .is_err(),
            "an app this loop cannot run must be refused rather than substituted"
        );
    }

    /// A scratch storage root of this test process's own.
    fn session_root(what: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "paperctl-session-test-{}-{what}",
            std::process::id()
        ))
    }
}
