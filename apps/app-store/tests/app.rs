//! `AppStoreApp` against a real catalog and a real store — the WWW-38
//! verification's "exercise against a real local catalog end to end":
//! `paperctl key generate`, `package`, `publish` are `Publisher`/`SecretKey`
//! here instead of the CLI, then the same `AppStoreApp` a session runs is
//! driven over a real socket pair, the same harness `ChessApp`'s own tests
//! use (`apps/chess/src/app.rs`), pressing INSTALL the way a finger would.
//!
//! `apps/app-store/tests/store.rs` already covers `PackagesSource` against
//! `paper_packages` directly, with no `App` in between; this file is the
//! other half — that the wire adapter (`src/app.rs`) drives that same source
//! correctly, without blocking the input loop, and that the install is
//! really on disk afterwards.

use std::fs;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use paper_app_store::{AppStoreApp, AppStoreScreen, PackagesSource, StoreLayout, StoreSource};
use paper_packages::archive;
use paper_packages::install::NothingIsRunning;
use paper_packages::inventory::AppState;
use paper_packages::publish::Publisher;
use paper_packages::signing::{SecretKey, TrustedKeys};
use paper_packages::store::Layout;
use paper_packages::{AppId, Capability, InstallPolicy, InstalledApp, Manifest};
use paper_protocol::{
    AppMessage, AppPaths, DrawReason, DrawRequest, ExitReason, FrameId, Hello, HostMessage,
    LaunchReason, LifecycleEvent, PixelFormat, Pointer, PointerEvent, PointerPhase, Request,
    SessionId, SurfaceDescriptor, codec,
};
use paper_sdk::{Canvas, ContactId, Damage, LocalSurfaces, Outcome, RuntimeError, SCREEN, run};
use semver::Version;

const MANIFEST: &str = r#"
[app]
id = "dev.calum.chess"
name = "Chess"
version = "VERSION"
protocol = "1.0"
entrypoint = "bin/chess"
"#;

const STORE_MANIFEST: &str = r#"
[app]
id = "dev.calum.app-store"
name = "App Store"
version = "0.1.0"
protocol = "1.0"
entrypoint = "bin/app-store"
"#;

/// A catalog with one publisher and a store to install into — the same
/// fixture shape as `apps/app-store/tests/store.rs`'s `World`, trimmed to
/// what driving a session needs.
struct World {
    _dirs: Vec<tempfile::TempDir>,
    secret: SecretKey,
    catalog: PathBuf,
    publisher: Publisher,
    layout: Layout,
    work: PathBuf,
}

impl World {
    fn new() -> Self {
        let catalog = tempfile::tempdir().expect("a catalog directory");
        let store = tempfile::tempdir().expect("a store directory");
        let work = tempfile::tempdir().expect("a work directory");
        let secret = SecretKey::generate().expect("a key");
        let publisher = Publisher::open(catalog.path(), "calum-home").expect("a catalog");
        let layout = Layout::new(store.path());
        layout.ensure().expect("a store");
        Self {
            catalog: catalog.path().to_path_buf(),
            work: work.path().to_path_buf(),
            _dirs: vec![catalog, store, work],
            secret,
            publisher,
            layout,
        }
    }

    fn source(&self) -> PackagesSource {
        let manifest = Manifest::parse(STORE_MANIFEST).expect("a valid manifest");
        let mut policy = InstallPolicy::deny_all();
        policy.allow(manifest.id(), Capability::Packages);
        let caller = InstalledApp::install(manifest, &policy);
        let mut keys = TrustedKeys::none();
        keys.trust(self.secret.public_key());
        PackagesSource::for_app(
            self.layout.clone(),
            InstallPolicy::deny_all(),
            &self.catalog,
            keys,
            &caller,
            Box::new(NothingIsRunning),
        )
        .expect("the App Store is granted `packages`")
    }

    fn publish(&self, version: &str, notes: &str) {
        let source = self.work.join(format!("src-{version}"));
        fs::create_dir_all(source.join("bin")).expect("a source tree");
        fs::write(
            source.join("paper.toml"),
            MANIFEST.replace("VERSION", version),
        )
        .expect("a manifest");
        fs::write(source.join("bin/chess"), format!("chess {version}")).expect("a binary");

        let package = self.work.join(format!("chess-{version}.paperpkg"));
        let mut bytes = Vec::new();
        archive::build(&source, &mut bytes).expect("a package");
        fs::write(&package, &bytes).expect("a package file");
        self.publisher
            .publish(&package, &self.secret, notes, 1_000)
            .expect("a publish");
    }
}

fn chess() -> AppId {
    "dev.calum.chess".parse().expect("a valid id")
}

fn app_paths() -> AppPaths {
    AppPaths {
        assets: "assets".into(),
        private: "private".into(),
        temp: "temp".into(),
        shared: Vec::new(),
    }
}

type Session = (
    UnixStream,
    UnixStream,
    std::thread::JoinHandle<Result<Outcome, RuntimeError>>,
);

/// Opens a real session over a real socket pair — the same loop a launched
/// process runs — blocking until `Ready` arrives.
fn start_session(source: PackagesSource) -> Session {
    let (host_side, app_side) = UnixStream::pair().expect("a socket pair");
    let reader = app_side.try_clone().expect("clone for reading");
    let writer = app_side;
    let screen = AppStoreScreen::new(source.inventory().expect("an inventory"));
    let app = AppStoreApp::new(screen, Arc::new(source));
    let handle = std::thread::spawn(move || run(app, reader, writer, LocalSurfaces::new()));

    let mut host_writer = host_side.try_clone().expect("clone for writing");
    let hello = Hello {
        protocol: paper_protocol::CURRENT,
        session: SessionId::new(1),
        app: "dev.calum.app-store".parse().expect("a valid id"),
        version: "0.1.0".parse().expect("a valid version"),
        launch: LaunchReason::Fresh,
        surface: SurfaceDescriptor::packed(SCREEN, PixelFormat::Argb8888),
        capabilities: vec![Capability::Packages],
        paths: app_paths(),
    };
    codec::write_message(&mut host_writer, &HostMessage::Hello(hello)).expect("writes the hello");

    let mut host_reader = host_side;
    let _ready: AppMessage = codec::read_message(&mut host_reader).expect("reads ready");
    (host_reader, host_writer, handle)
}

fn send(
    host_reader: &mut UnixStream,
    host_writer: &mut UnixStream,
    message: &HostMessage,
) -> AppMessage {
    codec::write_message(host_writer, message).expect("writes the message");
    codec::read_message(host_reader).expect("reads the reply")
}

/// Asks for `frame` and returns its damage, discarding any `Request`s that
/// arrive first — see `src/app.rs`'s own tests for why an install in flight
/// can interleave one of those with the `Frame` this is waiting for.
fn draw(host_reader: &mut UnixStream, host_writer: &mut UnixStream, frame: u64) -> Damage {
    codec::write_message(
        host_writer,
        &HostMessage::Draw(DrawRequest {
            frame: FrameId::new(frame),
            reason: DrawReason::AppRequested,
            viewport: SCREEN,
        }),
    )
    .expect("writes the draw request");
    loop {
        match codec::read_message::<_, AppMessage>(host_reader).expect("reads a reply") {
            AppMessage::Frame(done) => return done.damage,
            _ => continue,
        }
    }
}

fn pointer_up(at: paper_protocol::Point) -> HostMessage {
    HostMessage::Pointer(PointerEvent::new(
        at,
        PointerPhase::Up,
        Pointer::Touch,
        ContactId::FIRST,
    ))
}

fn finish_session(
    mut host_reader: UnixStream,
    mut host_writer: UnixStream,
    handle: std::thread::JoinHandle<Result<Outcome, RuntimeError>>,
) {
    let deadline =
        LifecycleEvent::prepare_to_exit(ExitReason::ReturnToStock, Duration::from_secs(5));
    let reply = send(
        &mut host_reader,
        &mut host_writer,
        &HostMessage::Lifecycle(deadline),
    );
    assert!(
        matches!(
            reply,
            AppMessage::Saved(paper_protocol::Saved { ok: true, .. })
        ),
        "expected a successful save, got {reply:?}"
    );
    match handle.join().expect("the session thread did not panic") {
        Ok(Outcome::Exited { saved: true, .. }) => {}
        other => panic!("expected a saved exit, got {other:?}"),
    }
}

#[test]
fn pressing_install_against_a_real_catalog_ends_with_it_on_disk() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");

    // Computed the same way the app itself renders, independent of the
    // session, so the tap lands where the button actually is (the same
    // convention `ChessApp`'s own tests use for `square_center`).
    let probe = AppStoreScreen::new(world.source().inventory().expect("an inventory"));
    let mut canvas = Canvas::new(SCREEN).expect("a canvas");
    let StoreLayout::List(list) = paper_app_store::render(&mut canvas, &probe) else {
        panic!("an offered, uninstalled app shows the list");
    };
    let install_at = list
        .rows
        .first()
        .expect("one row")
        .action
        .expect("an install button")
        .center();

    let (mut host_reader, mut host_writer, handle) = start_session(world.source());
    assert_eq!(
        draw(&mut host_reader, &mut host_writer, 1),
        Damage::Full,
        "the first frame is always the whole panel"
    );

    let reply = send(&mut host_reader, &mut host_writer, &pointer_up(install_at));
    assert!(
        matches!(reply, AppMessage::Request(Request::Redraw)),
        "pressing install must ask for a redraw, got {reply:?}"
    );

    // Draws until the install has actually finished (`Damage::Full`, which
    // only `Completion::Done` produces) rather than sleeping a guessed
    // duration — a real, local `paperpkg` installs in well under a second,
    // but nothing here should depend on exactly how fast.
    let finished = (2..52).any(|frame| {
        let damage = draw(&mut host_reader, &mut host_writer, frame);
        if damage == Damage::Full {
            true
        } else {
            std::thread::sleep(Duration::from_millis(20));
            false
        }
    });
    assert!(finished, "the install never finished");

    finish_session(host_reader, host_writer, handle);

    let after = world
        .source()
        .inventory()
        .expect("a survey after the install");
    let entry = after.entry(&chess()).expect("chess has a row");
    assert_eq!(entry.installed, Some(Version::parse("0.1.0").unwrap()));
    assert_eq!(entry.state, AppState::UpToDate);
}
