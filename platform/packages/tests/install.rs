//! Publish, install, update, roll back — and every fault path §12 names.
//!
//! These run the real code end to end: a package is built, published into a
//! real catalog directory, fetched back through a transport, verified against
//! a trusted key and installed into a real store. Nothing is mocked, because
//! what is being tested is exactly the seams a mock would paper over.
//!
//! The faults covered here, in the order §12 lists them: a damaged package, a
//! bad signature, an interrupted download, a disk that fills up, a catalog
//! that cannot be reached, and an activation that is interrupted — plus a
//! simulated power loss between staging and commit, which is the one that
//! decides whether any of the rest matter.
//!
//! The invariant every fault test asserts is the same one: **whatever fails,
//! the previously installed release is still there and still selected.**

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use paper_packages::archive;
use paper_packages::catalog::{Catalog, CatalogError, FileTransport};
use paper_packages::install::{
    ActivationGuard, InstallError, InstallOptions, Installed, NothingIsRunning, PackageManager,
    Progress, Step, Unwatched,
};
use paper_packages::publish::Publisher;
use paper_packages::signing::{SecretKey, TrustedKeys};
use paper_packages::store::{Layout, RELEASE_MARKER};
use paper_packages::{AppId, Capability, InstallPolicy, InstalledApp, Manifest};
use semver::Version;

const MANIFEST: &str = r#"
[app]
id = "dev.calum.chess"
name = "Chess"
version = "VERSION"
protocol = "1.0"
entrypoint = "bin/chess"
assets = ["assets/board.dat"]
"#;

/// Everything a test needs: a publisher, a catalog, and a store.
struct World {
    _dirs: Vec<tempfile::TempDir>,
    secret: SecretKey,
    catalog_dir: PathBuf,
    publisher: Publisher,
    layout: Layout,
    work: PathBuf,
}

impl World {
    fn new() -> Self {
        let catalog = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let secret = SecretKey::generate().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();
        let layout = Layout::new(store.path());
        layout.ensure().unwrap();
        Self {
            catalog_dir: catalog.path().to_path_buf(),
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

    fn catalog(&self) -> Catalog<FileTransport> {
        Catalog::new(FileTransport::new(&self.catalog_dir), self.keys())
            .with_cache(self.layout.clone())
    }

    fn manager(&self) -> PackageManager {
        PackageManager::host(self.layout.clone(), InstallPolicy::deny_all())
    }

    /// Builds and publishes Chess at `version`.
    fn publish(&self, version: &str) -> PathBuf {
        let source = self.work.join(format!("src-{version}"));
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(source.join("assets")).unwrap();
        fs::write(
            source.join("paper.toml"),
            MANIFEST.replace("VERSION", version),
        )
        .unwrap();
        fs::write(source.join("bin/chess"), format!("chess {version}")).unwrap();
        fs::write(source.join("assets/board.dat"), b"board").unwrap();

        let package = self.work.join(format!("chess-{version}.paperpkg"));
        let mut bytes = Vec::new();
        archive::build(&source, &mut bytes).unwrap();
        fs::write(&package, &bytes).unwrap();

        self.publisher
            .publish(&package, &self.secret, "notes", 1_000)
            .unwrap();
        package
    }

    /// Installs `version` through the catalog, as a device would.
    fn install(&self, version: &str) -> Result<Installed, InstallError> {
        self.install_with(version, &InstallOptions::default(), &NothingIsRunning)
    }

    fn install_with(
        &self,
        version: &str,
        options: &InstallOptions,
        guard: &dyn ActivationGuard,
    ) -> Result<Installed, InstallError> {
        let catalog = self.catalog();
        let view = catalog.view().unwrap();
        let wanted = Version::parse(version).unwrap();
        let entry = view
            .index
            .exact(&app(), &wanted)
            .expect("the catalog should offer that version");
        let release = catalog.release(entry).unwrap();
        let archive = catalog.open_archive(entry, &release).unwrap();
        self.manager()
            .install(&release, archive, options, guard, &Unwatched)
    }

    /// The archive bytes a release descriptor points at, straight off disk.
    fn archive_bytes(&self, version: &str) -> Vec<u8> {
        fs::read(self.catalog_dir.join(format!(
            "apps/dev.calum.chess/{version}/chess-{version}.paperpkg"
        )))
        .unwrap()
    }

    fn current(&self) -> Option<Version> {
        self.layout.current(&app()).unwrap()
    }

    /// Installs straight from bytes, bypassing the transport.
    ///
    /// For the faults that are about the bytes arriving badly rather than the
    /// catalog being wrong: the descriptor stays honest and the stream does not.
    fn install_bytes(&self, version: &str, source: impl Read) -> Result<Installed, InstallError> {
        let catalog = self.catalog();
        let view = catalog.view().unwrap();
        let entry = view
            .index
            .exact(&app(), &Version::parse(version).unwrap())
            .unwrap();
        let release = catalog.release(entry).unwrap();
        self.manager().install(
            &release,
            source,
            &InstallOptions::default(),
            &NothingIsRunning,
            &Unwatched,
        )
    }
}

fn app() -> AppId {
    "dev.calum.chess".parse().unwrap()
}

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// Asserts the store still has a working `0.1.0` selected.
fn still_on_0_1_0(world: &World) {
    assert_eq!(world.current(), Some(version("0.1.0")));
    let dir = world.layout.release_dir(&app(), &version("0.1.0"));
    assert_eq!(
        fs::read_to_string(dir.join("bin/chess")).unwrap(),
        "chess 0.1.0"
    );
    assert!(dir.join(RELEASE_MARKER).exists());
}

#[test]
fn publish_install_update_and_roll_back() {
    let world = World::new();
    world.publish("0.1.0");
    let first = world.install("0.1.0").unwrap();
    assert!(first.activated);
    assert_eq!(world.current(), Some(version("0.1.0")));

    world.publish("0.2.0");
    let second = world.install("0.2.0").unwrap();
    assert!(second.activated);
    assert_eq!(second.previous, Some(version("0.1.0")));
    assert_eq!(world.current(), Some(version("0.2.0")));
    assert_eq!(
        fs::read_to_string(
            world
                .layout
                .release_dir(&app(), &version("0.2.0"))
                .join("bin/chess")
        )
        .unwrap(),
        "chess 0.2.0"
    );

    // The previous release is still on disk: rollback is a selection change.
    assert_eq!(
        world.layout.previous(&app()).unwrap(),
        Some(version("0.1.0"))
    );
    let rolled = world.manager().rollback(&app(), &NothingIsRunning).unwrap();
    assert_eq!(rolled.version, version("0.1.0"));
    assert_eq!(rolled.from, version("0.2.0"));
    still_on_0_1_0(&world);

    // And a rollback can itself be undone, without going back to the catalog.
    let forward = world.manager().rollback(&app(), &NothingIsRunning).unwrap();
    assert_eq!(forward.version, version("0.2.0"));
}

#[test]
fn a_damaged_package_is_refused_and_changes_nothing() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");

    // One byte flipped in the middle of the archive: the digest the publisher
    // signed no longer matches what arrives.
    let mut damaged = world.archive_bytes("0.2.0");
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0xff;

    let error = world.install_bytes("0.2.0", &damaged[..]).unwrap_err();
    assert!(matches!(error, InstallError::Digest { .. }), "{error:?}");
    still_on_0_1_0(&world);
    assert!(!world.layout.release_dir(&app(), &version("0.2.0")).exists());
}

#[test]
fn a_bad_signature_stops_the_install_before_anything_is_fetched() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");

    // A descriptor edited after signing. The bytes still parse; the signature
    // is over the bytes as published, so it no longer verifies.
    let descriptor = world
        .catalog_dir
        .join("apps/dev.calum.chess/0.2.0/release.toml");
    let text = fs::read_to_string(&descriptor).unwrap();
    fs::write(&descriptor, text.replace("notes", "notes ")).unwrap();

    let catalog = world.catalog();
    let view = catalog.view().unwrap();
    let entry = view.index.exact(&app(), &version("0.2.0")).unwrap();
    assert!(matches!(
        catalog.release(entry),
        Err(CatalogError::Signature(_))
    ));
    still_on_0_1_0(&world);
}

#[test]
fn a_release_signed_by_an_untrusted_key_is_refused() {
    let world = World::new();
    world.publish("0.1.0");

    let stranger = SecretKey::generate().unwrap();
    let mut keys = TrustedKeys::none();
    keys.trust(stranger.public_key());
    let catalog = Catalog::new(FileTransport::new(&world.catalog_dir), keys);
    assert!(matches!(catalog.view(), Err(CatalogError::Signature(_))));
}

#[test]
fn an_interrupted_download_leaves_the_previous_release_selected() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");

    let truncated = {
        let full = world.archive_bytes("0.2.0");
        full[..full.len() / 3].to_vec()
    };
    let error = world.install_bytes("0.2.0", &truncated[..]).unwrap_err();
    assert!(matches!(error, InstallError::Size { .. }), "{error:?}");

    still_on_0_1_0(&world);
    assert_eq!(staging_entries(&world), 0, "staging was left behind");
    assert_eq!(
        journal_entries(&world),
        0,
        "a journal entry was left behind"
    );
}

#[test]
fn a_download_longer_than_the_signed_size_is_refused() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");

    let mut padded = world.archive_bytes("0.2.0");
    padded.extend_from_slice(&[0u8; 4096]);
    let error = world.install_bytes("0.2.0", &padded[..]).unwrap_err();
    // The bound is the declared size, so the extra bytes are refused as they
    // arrive rather than noticed after they are on disk.
    assert!(
        matches!(error, InstallError::Io { .. } | InstallError::Size { .. }),
        "{error:?}"
    );
    still_on_0_1_0(&world);
}

/// A reader that runs out of disk partway through, the way a real one does.
struct DiskFullAfter {
    bytes: Vec<u8>,
    limit: usize,
    read: usize,
}

impl Read for DiskFullAfter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.read >= self.limit {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        let take = buf
            .len()
            .min(self.limit - self.read)
            .min(self.bytes.len() - self.read);
        buf[..take].copy_from_slice(&self.bytes[self.read..self.read + take]);
        self.read += take;
        Ok(take)
    }
}

#[test]
fn a_full_disk_fails_the_install_and_not_the_device() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");

    let bytes = world.archive_bytes("0.2.0");
    let limit = bytes.len() / 2;
    let error = world
        .install_bytes(
            "0.2.0",
            DiskFullAfter {
                bytes,
                limit,
                read: 0,
            },
        )
        .unwrap_err();
    assert!(matches!(error, InstallError::Io { .. }), "{error:?}");

    still_on_0_1_0(&world);
    assert_eq!(staging_entries(&world), 0);
    assert_eq!(journal_entries(&world), 0);
}

#[test]
fn an_unavailable_catalog_still_shows_what_is_installed() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();

    fs::remove_file(world.catalog_dir.join("index.toml")).unwrap();

    // Offline: the cached index is still shown, marked stale, and the
    // installed release is untouched and still selected.
    let view = world.catalog().view().unwrap();
    assert!(view.stale);
    assert_eq!(view.index.entries().len(), 1);
    still_on_0_1_0(&world);
    assert_eq!(
        world.manager().installed_versions(&app()).unwrap(),
        vec![version("0.1.0")]
    );
}

#[test]
fn an_unavailable_catalog_with_no_history_says_so() {
    let world = World::new();
    assert!(matches!(
        world.catalog().view(),
        Err(CatalogError::Transport(_))
    ));
}

#[test]
fn a_power_loss_between_staging_and_commit_leaves_the_old_release_usable() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();

    // What a crash mid-download leaves: a staging directory with a partial
    // archive in it, and the journal entry that claimed it.
    let staging = world
        .layout
        .staging_dir()
        .join("dev.calum.chess-0.2.0-999-1");
    fs::create_dir_all(staging.join("payload/bin")).unwrap();
    fs::write(staging.join("download.paperpkg"), b"half an archive").unwrap();
    fs::write(staging.join("payload/bin/chess"), b"half a binary").unwrap();
    write_journal(
        &world,
        "dev.calum.chess-0.2.0-stage.toml",
        &format!(
            "app = \"dev.calum.chess\"\nversion = \"0.2.0\"\nstaging = \"{}\"\n\
             phase = \"stage\"\nstarted = 1\n",
            staging.display()
        ),
    );

    still_on_0_1_0(&world);
    let report = world.manager().recover().unwrap();
    assert_eq!(report.staging_removed, 1);
    assert!(report.completed.is_empty());
    assert!(!staging.exists());
    still_on_0_1_0(&world);
    assert_eq!(journal_entries(&world), 0);
}

#[test]
fn an_interrupted_activation_is_finished_when_the_release_is_complete() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");

    // Commit without activating, then write the journal entry by hand: this is
    // the state a crash between `commit_directory` and the selection write
    // leaves behind.
    let mut options = InstallOptions::default();
    options.activate = false;
    world
        .install_with("0.2.0", &options, &NothingIsRunning)
        .unwrap();
    assert_eq!(world.current(), Some(version("0.1.0")));

    write_journal(
        &world,
        "dev.calum.chess-0.2.0-activate.toml",
        "app = \"dev.calum.chess\"\nversion = \"0.2.0\"\nprevious = \"0.1.0\"\n\
         phase = \"activate\"\nstarted = 1\n",
    );

    let report = world.manager().recover().unwrap();
    assert_eq!(report.completed, vec![(app(), version("0.2.0"))]);
    assert_eq!(world.current(), Some(version("0.2.0")));
    assert_eq!(
        world.layout.previous(&app()).unwrap(),
        Some(version("0.1.0"))
    );
    assert_eq!(journal_entries(&world), 0);
}

#[test]
fn an_interrupted_activation_reverts_when_the_release_is_not_there() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();

    // An activation intent naming a version that never made it to disk.
    write_journal(
        &world,
        "dev.calum.chess-0.9.0-activate.toml",
        "app = \"dev.calum.chess\"\nversion = \"0.9.0\"\nprevious = \"0.1.0\"\n\
         phase = \"activate\"\nstarted = 1\n",
    );

    let report = world.manager().recover().unwrap();
    assert_eq!(report.reverted, vec![(app(), Some(version("0.1.0")))]);
    still_on_0_1_0(&world);
}

#[test]
fn a_half_written_release_directory_is_never_launchable_and_is_swept() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();

    // A release directory with content but no marker — what a naive installer
    // that extracted straight into place would leave after a power cut.
    let debris = world.layout.release_dir(&app(), &version("0.2.0"));
    fs::create_dir_all(debris.join("bin")).unwrap();
    fs::write(debris.join("bin/chess"), b"partial").unwrap();

    assert_eq!(
        world.manager().installed_versions(&app()).unwrap(),
        vec![version("0.1.0")],
        "an unmarked directory must never be listed as installed"
    );

    let report = world.manager().recover().unwrap();
    assert_eq!(report.incomplete_removed, vec![(app(), "0.2.0".to_owned())]);
    assert!(!debris.exists());
    still_on_0_1_0(&world);
}

#[test]
fn recovery_on_a_healthy_store_does_nothing() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    assert!(world.manager().recover().unwrap().is_clean());
    still_on_0_1_0(&world);
}

/// An app the host considers to be running.
#[derive(Debug)]
struct Running;

impl ActivationGuard for Running {
    fn is_running(&self, _app: &AppId) -> bool {
        true
    }
}

#[test]
fn an_update_never_replaces_the_running_version() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");

    let error = world
        .install_with("0.2.0", &InstallOptions::default(), &Running)
        .unwrap_err();
    assert!(matches!(error, InstallError::Running { .. }), "{error:?}");

    // The bytes are committed — the download was not wasted — but the running
    // app is still the one that was running.
    assert_eq!(world.current(), Some(version("0.1.0")));
    assert!(
        world
            .layout
            .release_dir(&app(), &version("0.2.0"))
            .join(RELEASE_MARKER)
            .exists()
    );
    // And once it stops, the same install activates cleanly.
    world.install("0.2.0").unwrap();
    assert_eq!(world.current(), Some(version("0.2.0")));
}

#[test]
fn a_rollback_is_refused_while_the_app_is_running() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");
    world.install("0.2.0").unwrap();

    assert!(matches!(
        world.manager().rollback(&app(), &Running),
        Err(InstallError::Running { .. })
    ));
    assert_eq!(world.current(), Some(version("0.2.0")));
}

#[test]
fn an_older_version_is_never_installed_over_a_newer_one() {
    let world = World::new();
    world.publish("0.1.0");
    world.publish("0.2.0");
    world.install("0.2.0").unwrap();

    let error = world.install("0.1.0").unwrap_err();
    assert!(matches!(error, InstallError::Downgrade { .. }), "{error:?}");
    assert_eq!(world.current(), Some(version("0.2.0")));
}

#[test]
fn installing_the_same_version_twice_is_idempotent() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    let again = world.install("0.1.0").unwrap();
    assert!(again.already_present);
    assert_eq!(world.current(), Some(version("0.1.0")));
}

#[test]
fn a_package_whose_manifest_contradicts_the_signed_release_is_refused() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");

    // A perfectly valid package for a different version, served under 0.2.0's
    // descriptor — but the descriptor names a digest, so this is only
    // reachable by also re-signing. The check is here because without it a
    // signature over a wrapper would be mistaken for a signature over content.
    let other = World::new();
    other.publish("0.3.0");
    let swapped = other.archive_bytes("0.3.0");

    let error = world.install_bytes("0.2.0", &swapped[..]).unwrap_err();
    assert!(
        matches!(
            error,
            InstallError::Size { .. } | InstallError::Digest { .. }
        ),
        "{error:?}"
    );
    still_on_0_1_0(&world);
}

#[test]
fn only_an_app_granted_packages_can_manage_packages() {
    let world = World::new();
    let store_manifest = Manifest::parse(
        r#"
        [app]
        id = "dev.calum.app-store"
        name = "App Store"
        version = "0.1.0"
        protocol = "1.0"
        entrypoint = "bin/app-store"
        "#,
    )
    .unwrap();
    let chess = Manifest::parse(&MANIFEST.replace("VERSION", "0.1.0")).unwrap();

    let mut policy = InstallPolicy::deny_all();
    policy.allow(store_manifest.id(), Capability::Packages);

    let app_store = InstalledApp::install(store_manifest, &policy);
    let chess = InstalledApp::install(chess, &policy);

    assert!(PackageManager::on_behalf_of(world.layout.clone(), policy.clone(), &app_store).is_ok());
    assert!(matches!(
        PackageManager::on_behalf_of(world.layout.clone(), policy, &chess),
        Err(InstallError::NotPermitted { .. })
    ));
}

#[test]
fn an_installed_app_holds_only_what_policy_granted() {
    let world = World::new();
    world.publish("0.1.0");
    let installed = world.install("0.1.0").unwrap();
    // `deny_all`, and the package asked for nothing because it cannot.
    assert!(installed.capabilities.is_empty());
}

#[test]
fn pruning_keeps_the_selected_release_and_the_one_behind_it() {
    let world = World::new();
    for version in ["0.1.0", "0.2.0", "0.3.0"] {
        world.publish(version);
        world.install(version).unwrap();
    }
    let installed = world.manager().installed_versions(&app()).unwrap();
    assert_eq!(installed, vec![version("0.2.0"), version("0.3.0")]);
    assert_eq!(world.current(), Some(version("0.3.0")));
    assert_eq!(
        world.layout.previous(&app()).unwrap(),
        Some(version("0.2.0"))
    );
}

#[test]
fn nothing_an_install_writes_is_executable_except_the_entrypoint() {
    use std::os::unix::fs::PermissionsExt as _;

    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();

    let dir = world.layout.release_dir(&app(), &version("0.1.0"));
    let mode = |relative: &str| {
        fs::metadata(dir.join(relative))
            .unwrap()
            .permissions()
            .mode()
            & 0o111
    };
    assert_ne!(mode("bin/chess"), 0);
    assert_eq!(mode("assets/board.dat"), 0);
    assert_eq!(mode("paper.toml"), 0);
}

/// Records every step an install reports, in order.
#[derive(Debug, Default)]
struct Recorder {
    steps: std::sync::Mutex<Vec<Step>>,
}

impl Progress for Recorder {
    fn step(&self, step: Step) {
        if let Ok(mut steps) = self.steps.lock() {
            steps.push(step);
        }
    }
}

#[test]
fn an_install_reports_its_progress_in_order() {
    let world = World::new();
    world.publish("0.1.0");

    let recorder = Recorder::default();
    let catalog = world.catalog();
    let view = catalog.view().unwrap();
    let entry = view.index.exact(&app(), &version("0.1.0")).unwrap();
    let release = catalog.release(entry).unwrap();
    let archive = catalog.open_archive(entry, &release).unwrap();
    world
        .manager()
        .install(
            &release,
            archive,
            &InstallOptions::default(),
            &NothingIsRunning,
            &recorder,
        )
        .unwrap();

    let steps = recorder.steps.lock().unwrap().clone();
    let named: Vec<&str> = steps
        .iter()
        .map(|step| match step {
            Step::Downloading { .. } => "download",
            Step::Verifying => "verify",
            Step::Extracting => "extract",
            Step::Committing => "commit",
            Step::Activating => "activate",
        })
        .collect();

    // Downloads may report many times; the rest exactly once, in this order.
    let mut collapsed: Vec<&str> = Vec::new();
    for name in named {
        if collapsed.last() != Some(&name) {
            collapsed.push(name);
        }
    }
    assert_eq!(
        collapsed,
        vec!["download", "verify", "extract", "commit", "activate"],
        "{steps:?}"
    );

    // The total is the signed size, known before a byte arrives, and the
    // reported progress never exceeds it.
    let signed = release.release().size();
    for step in &steps {
        if let Step::Downloading { done, total } = step {
            assert_eq!(*total, signed);
            assert!(*done <= signed, "reported {done} of {signed}");
        }
    }
}

#[test]
fn a_failed_install_stops_reporting_where_it_failed() {
    let world = World::new();
    world.publish("0.1.0");
    world.install("0.1.0").unwrap();
    world.publish("0.2.0");

    let mut damaged = world.archive_bytes("0.2.0");
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0xff;

    let recorder = Recorder::default();
    let catalog = world.catalog();
    let view = catalog.view().unwrap();
    let entry = view.index.exact(&app(), &version("0.2.0")).unwrap();
    let release = catalog.release(entry).unwrap();
    assert!(
        world
            .manager()
            .install(
                &release,
                &damaged[..],
                &InstallOptions::default(),
                &NothingIsRunning,
                &recorder,
            )
            .is_err()
    );

    let steps = recorder.steps.lock().unwrap().clone();
    // It got as far as verifying, and never claimed to commit or activate.
    assert!(steps.contains(&Step::Verifying), "{steps:?}");
    assert!(!steps.contains(&Step::Committing), "{steps:?}");
    assert!(!steps.contains(&Step::Activating), "{steps:?}");
    still_on_0_1_0(&world);
}

fn write_journal(world: &World, name: &str, body: &str) {
    let dir = world.layout.journal_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), body).unwrap();
}

fn staging_entries(world: &World) -> usize {
    count(&world.layout.staging_dir())
}

fn journal_entries(world: &World) -> usize {
    count(&world.layout.journal_dir())
}

fn count(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| entries.count())
        .unwrap_or(0)
}
