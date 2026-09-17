//! The App Store against a real catalog and a real store.
//!
//! The screen tests in `src/screen.rs` cover the transitions with no I/O at
//! all. These cover the other half: that [`PackagesSource`] is wired to
//! `paper_packages` correctly, that the capability check is real, and that a
//! press on a row produces an install that actually happens.

use std::fs;
use std::path::PathBuf;

use paper_app_store::{AppStoreScreen, Request, SourceError, StoreSource};
use paper_packages::archive;
use paper_packages::install::{NothingIsRunning, PackageManager, Progress, Step, Unwatched};
use paper_packages::inventory::AppState;
use paper_packages::publish::Publisher;
use paper_packages::signing::{SecretKey, TrustedKeys};
use paper_packages::store::Layout;
use paper_packages::{AppId, Capability, InstallPolicy, InstalledApp, Manifest};
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

    fn keys(&self) -> TrustedKeys {
        let mut keys = TrustedKeys::none();
        keys.trust(self.secret.public_key());
        keys
    }

    /// The App Store itself, installed with whatever `granted` says.
    fn caller(&self, granted: bool) -> InstalledApp {
        let manifest = Manifest::parse(STORE_MANIFEST).expect("a valid manifest");
        let mut policy = InstallPolicy::deny_all();
        if granted {
            policy.allow(manifest.id(), Capability::Packages);
        }
        InstalledApp::install(manifest, &policy)
    }

    fn source(&self, granted: bool) -> Result<paper_app_store::PackagesSource, SourceError> {
        self.source_with(granted, Box::new(NothingIsRunning))
    }

    fn source_with(
        &self,
        granted: bool,
        running: Box<dyn paper_packages::install::ActivationGuard + Send + Sync>,
    ) -> Result<paper_app_store::PackagesSource, SourceError> {
        paper_app_store::PackagesSource::for_app(
            self.layout.clone(),
            InstallPolicy::deny_all(),
            &self.catalog,
            self.keys(),
            &self.caller(granted),
            running,
        )
    }

    /// The host's own package manager — the owner of the transaction, as
    /// distinct from the App Store that is a client of it.
    fn host(&self) -> PackageManager {
        PackageManager::host(self.layout.clone(), InstallPolicy::deny_all())
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

fn version(text: &str) -> Version {
    Version::parse(text).expect("a valid version")
}

#[test]
fn an_app_store_without_the_packages_grant_gets_no_installer() {
    let world = World::new();
    let error = world.source(false).expect_err("must be refused");
    assert!(
        matches!(error, SourceError::NotPermitted { .. }),
        "{error:?}"
    );
    // And the advice tells the operator what to actually do about it.
    assert!(error.advice().contains("packages"), "{}", error.advice());
}

#[test]
fn the_list_shows_what_the_catalog_offers_before_anything_is_installed() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");

    let source = world.source(true).expect("a source");
    let screen = AppStoreScreen::new(source.inventory().expect("an inventory"));

    let entry = screen
        .inventory()
        .entry(&chess())
        .expect("chess should be listed");
    assert_eq!(entry.state, AppState::Available);
    assert_eq!(entry.installed, None);
    assert_eq!(entry.available, Some(version("0.1.0")));
    assert_eq!(screen.primary_label(&chess()), Some("INSTALL"));
    assert_eq!(
        screen.primary_action(&chess()),
        Some(Request::Install {
            app: chess(),
            version: version("0.1.0"),
        })
    );
}

/// Records the steps an install reported.
#[derive(Debug, Default)]
struct Seen {
    steps: std::sync::Mutex<Vec<Step>>,
}

impl Progress for Seen {
    fn step(&self, step: Step) {
        if let Ok(mut steps) = self.steps.lock() {
            steps.push(step);
        }
    }
}

#[test]
fn pressing_install_installs_and_the_next_survey_says_so() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");
    let source = world.source(true).expect("a source");

    let mut screen = AppStoreScreen::new(source.inventory().expect("an inventory"));
    let request = screen
        .primary_action(&chess())
        .expect("an install is offered");
    assert!(screen.begin(&request));

    let Request::Install { app, version } = &request else {
        panic!("the primary action on an uninstalled app is an install");
    };
    let seen = Seen::default();
    let installed = source.install(app, version, &seen).expect("an install");
    assert_eq!(installed, version.clone());
    assert!(
        !seen.steps.lock().expect("the steps").is_empty(),
        "the App Store must be told what is happening"
    );

    let after = source.inventory().expect("a second inventory");
    let entry = after.entry(&chess()).expect("a row");
    assert_eq!(entry.installed, Some(version.clone()));
    assert_eq!(entry.state, AppState::UpToDate);

    let screen = AppStoreScreen::new(after);
    assert_eq!(screen.primary_label(&chess()), None, "nothing left to do");
}

#[test]
fn an_update_is_offered_and_leaves_the_previous_release_to_roll_back_to() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");
    let source = world.source(true).expect("a source");
    source
        .install(&chess(), &version("0.1.0"), &Unwatched)
        .expect("the first install");

    world.publish("0.2.0", "Castling works now.");
    let screen = AppStoreScreen::new(source.inventory().expect("an inventory"));
    assert_eq!(screen.primary_label(&chess()), Some("UPDATE"));

    source
        .install(&chess(), &version("0.2.0"), &Unwatched)
        .expect("the update");

    let screen = AppStoreScreen::new(source.inventory().expect("an inventory"));
    assert!(screen.can_roll_back(&chess()));

    let back = source.rollback(&chess()).expect("a rollback");
    assert_eq!(back, version("0.1.0"));
}

#[test]
fn release_notes_come_from_the_signed_descriptor() {
    let world = World::new();
    world.publish("0.1.0", "Castling works now.");
    let source = world.source(true).expect("a source");
    assert_eq!(
        source.notes(&chess(), &version("0.1.0")).expect("notes"),
        "Castling works now."
    );
}

#[test]
fn an_unreachable_catalog_still_lists_what_is_installed() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");
    let source = world.source(true).expect("a source");
    source
        .install(&chess(), &version("0.1.0"), &Unwatched)
        .expect("an install");

    fs::remove_file(world.catalog.join("index.toml")).expect("removing the index");

    let screen = AppStoreScreen::new(source.inventory().expect("an inventory"));
    assert!(screen.is_stale(), "the screen must say the list is behind");
    assert!(screen.catalog_summary().contains("OFFLINE"));
    let entry = screen.inventory().entry(&chess()).expect("a row");
    assert_eq!(entry.installed, Some(version("0.1.0")));
}

#[test]
fn a_tampered_release_is_refused_and_the_advice_says_not_to_install_it() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");
    let descriptor = world
        .catalog
        .join("apps/dev.calum.chess/0.1.0/release.toml");
    let text = fs::read_to_string(&descriptor).expect("a descriptor");
    fs::write(&descriptor, text.replace("size = ", "size  = ")).expect("tampering");

    let source = world.source(true).expect("a source");
    let error = source
        .install(&chess(), &version("0.1.0"), &Unwatched)
        .expect_err("a tampered release must be refused");
    assert!(
        error.advice().contains("Do not install"),
        "advice was: {}",
        error.advice()
    );
}

#[test]
fn a_failing_release_offers_a_roll_back_rather_than_an_update() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");
    world.publish("0.2.0", "Second.");
    let source = world.source(true).expect("a source");
    source
        .install(&chess(), &version("0.1.0"), &Unwatched)
        .expect("the first install");
    source
        .install(&chess(), &version("0.2.0"), &Unwatched)
        .expect("the update");

    // 0.2.0 never starts.
    let ledger = paper_packages::launch::Ledger::new(world.layout.clone());
    for _ in 0..paper_packages::launch::Ledger::MAX_ATTEMPTS {
        ledger
            .record_attempt(&chess(), &version("0.2.0"))
            .expect("an attempt");
    }
    ledger
        .record_failure(&chess(), &version("0.2.0"), "exited immediately")
        .expect("a failure");

    let screen = AppStoreScreen::new(source.inventory().expect("an inventory"));
    let entry = screen.inventory().entry(&chess()).expect("a row");
    assert_eq!(entry.state, AppState::Failing);
    assert_eq!(screen.primary_label(&chess()), Some("ROLL BACK"));
    assert_eq!(
        screen.primary_action(&chess()),
        Some(Request::Rollback { app: chess() })
    );
}

/// A reader that dies partway through, the way a killed process does.
struct DiesPartway {
    bytes: Vec<u8>,
    die_after: usize,
    read: usize,
}

impl std::io::Read for DiesPartway {
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the panic is the point: this simulates the process being \
                  killed mid-transaction. Returning `Err` would exercise the \
                  ordinary download-failed path, which is already covered and \
                  is a different thing entirely."
    )]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.read >= self.die_after {
            panic!("the App Store process died");
        }
        let take = buf
            .len()
            .min(self.die_after - self.read)
            .min(self.bytes.len() - self.read);
        buf[..take].copy_from_slice(&self.bytes[self.read..self.read + take]);
        self.read += take;
        Ok(take)
    }
}

/// An app the guard reports as running.
#[derive(Debug)]
struct Running;

impl paper_packages::install::ActivationGuard for Running {
    fn is_running(&self, _app: &AppId) -> bool {
        true
    }
}

#[test]
fn the_app_store_dying_mid_install_does_not_invalidate_the_transaction() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");
    world.publish("0.2.0", "Second.");
    let source = world.source(true).expect("a source");
    source
        .install(&chess(), &version("0.1.0"), &Unwatched)
        .expect("the first install");

    let archive = fs::read(
        world
            .catalog
            .join("apps/dev.calum.chess/0.2.0/chess-0.2.0.paperpkg"),
    )
    .expect("the archive");

    // The App Store is killed halfway through the download. The transaction is
    // the *host's*; the App Store is only the thing that asked for it.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = world.host().install(
            &fetch_release(&world, "0.2.0"),
            DiesPartway {
                die_after: archive.len() / 2,
                bytes: archive,
                read: 0,
            },
            &paper_packages::install::InstallOptions::default(),
            &NothingIsRunning,
            &Unwatched,
        );
    }));
    std::panic::set_hook(previous);
    assert!(died.is_err(), "the simulated death must actually unwind");

    // Nothing is invalidated: 0.1.0 is still selected and still all there.
    assert_eq!(
        world.layout.current(&chess()).expect("a selection"),
        Some(version("0.1.0"))
    );
    assert!(
        !world
            .layout
            .release_dir(&chess(), &version("0.2.0"))
            .exists(),
        "a half-installed release must not appear"
    );

    // The host tidies up, and says what it found.
    let report = world.host().recover().expect("recovery");
    assert!(!report.is_clean(), "there was debris to clear: {report:?}");

    // And the same install, asked for again, simply works.
    let source = world.source(true).expect("a source");
    source
        .install(&chess(), &version("0.2.0"), &Unwatched)
        .expect("the retry after a crash");
    assert_eq!(
        world.layout.current(&chess()).expect("a selection"),
        Some(version("0.2.0"))
    );
}

#[test]
fn a_lock_left_by_a_killed_app_store_does_not_wedge_installs_forever() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");

    // What a `SIGKILL` leaves: `Drop` never ran, so the lock directory is
    // still there with no process behind it.
    let stale = world.layout.locks_dir().join("dev.calum.chess.lock");
    fs::create_dir_all(&stale).expect("a stale lock");
    fs::write(stale.join("owner"), "pid 999999 since 0\n").expect("an owner file");

    let source = world.source(true).expect("a source");
    assert!(
        source
            .install(&chess(), &version("0.1.0"), &Unwatched)
            .is_err(),
        "a held lock must refuse while it is believed"
    );

    // Recovery runs at host start, when nothing can be holding one.
    let report = world.host().recover().expect("recovery");
    assert_eq!(report.locks_broken, 1);
    assert!(!stale.exists());

    source
        .install(&chess(), &version("0.1.0"), &Unwatched)
        .expect("the install after the stale lock was cleared");
}

#[test]
fn an_update_is_refused_while_the_host_says_the_app_is_running() {
    let world = World::new();
    world.publish("0.1.0", "First cut.");
    world.publish("0.2.0", "Second.");

    let quiet = world.source(true).expect("a source");
    quiet
        .install(&chess(), &version("0.1.0"), &Unwatched)
        .expect("the first install");

    // The same App Store, told by the host that Chess is running.
    let busy = world
        .source_with(true, Box::new(Running))
        .expect("a source");
    let error = busy
        .install(&chess(), &version("0.2.0"), &Unwatched)
        .expect_err("an update must not replace a running app");
    assert!(
        error.advice().contains("Close the app first"),
        "advice was: {}",
        error.advice()
    );
    assert_eq!(
        world.layout.current(&chess()).expect("a selection"),
        Some(version("0.1.0")),
        "the running version must still be the selected one"
    );
}

/// Fetches and verifies one release, the way the App Store does.
fn fetch_release(world: &World, version_text: &str) -> paper_packages::release::VerifiedRelease {
    use paper_packages::catalog::{Catalog, FileTransport};

    let catalog = Catalog::new(FileTransport::new(&world.catalog), world.keys());
    let view = catalog.view().expect("a catalog view");
    let entry = view
        .index
        .exact(&chess(), &version(version_text))
        .expect("an offered version");
    catalog.release(entry).expect("a verified release")
}
