//! The on-disk layout, and the durability primitives everything else uses (§11).
//!
//! ```text
//! <root>/
//!   apps/<id>/releases/<version>/   one extracted package, immutable
//!   apps/<id>/current               which version is selected
//!   apps/<id>/previous              which version to fall back to
//!   data/<id>/                      the app's own writable state
//!   shared/<grant>/                 explicit cross-app sharing
//!   staging/<transaction>/          downloads and extraction in progress
//!   state/                          the host's own bookkeeping
//!   releases/, current              the platform's own releases (WWW-8)
//! ```
//!
//! Writable data is never inside a release directory. That is what makes a
//! release directory disposable, and disposable is what makes rollback a
//! rename rather than a restore (§11).
//!
//! # Rename is not a durability guarantee
//!
//! `rename(2)` is atomic with respect to *other readers*. It says nothing
//! about power. After a rename returns, a crash can still leave you with the
//! new name pointing at a file whose contents never reached the disk, or with
//! neither name, because the directory entry itself is in a write-back cache.
//!
//! So every commit in this module is the same four steps, and
//! [`atomic_write`] and [`commit_directory`] are the only places they are
//! written:
//!
//! 1. write the new content to a temporary name in the *same* directory;
//! 2. `fsync` the content — all of it, every file in a tree;
//! 3. `rename` the temporary name over the real one;
//! 4. `fsync` the directory that now holds the new name.
//!
//! Step 2 is what makes the file real. Step 4 is what makes the *name* real.
//! Skipping either leaves a window where a power cut produces a selection
//! pointing at a release that is not all there — the failure this whole module
//! exists to make impossible.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::Deserialize;

use crate::digest::Digest;
use crate::id::AppId;
use crate::signing::KeyId;

/// The file that marks a release directory complete.
///
/// Written inside the staging copy as the *last* thing before the tree is
/// fsynced and renamed into place, so a release directory either has it or is
/// not a release directory. Its name uses the reserved prefix that
/// [`archive`](crate::archive) refuses to extract, so no package can forge one.
pub const RELEASE_MARKER: &str = ".paperclip-release";

/// Where Paperclip keeps its data on the tablet (§11).
pub const DEVICE_ROOT: &str = "/home/root/.local/share/paperclip";

/// The environment variable that overrides the root, for the Mac and for tests.
pub const ROOT_ENV: &str = "PAPERCLIP_ROOT";

/// The directory tree Paperclip owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// A layout rooted at `root`. Nothing is created until [`Self::ensure`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The layout this process should use.
    ///
    /// `PAPERCLIP_ROOT` wins, then the device path. There is no "current
    /// directory" fallback: a store that silently appears wherever a command
    /// was run from is a store that gets two copies.
    pub fn from_environment() -> Self {
        match std::env::var_os(ROOT_ENV) {
            Some(root) if !root.is_empty() => Self::new(root),
            _ => Self::new(DEVICE_ROOT),
        }
    }

    /// The root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates every directory the layout needs, if it does not exist.
    pub fn ensure(&self) -> Result<(), StoreError> {
        for path in [
            self.root.clone(),
            self.root.join("apps"),
            self.root.join("data"),
            self.root.join("shared"),
            self.root.join("staging"),
            self.root.join("state"),
            self.journal_dir(),
            self.locks_dir(),
        ] {
            create_dir_if_missing(&path)?;
        }
        Ok(())
    }

    /// Every app directory that exists, in id order.
    pub fn apps(&self) -> Result<Vec<AppId>, StoreError> {
        let dir = self.root.join("apps");
        let mut apps = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(apps),
            Err(source) => return Err(StoreError::io(&dir, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| StoreError::io(&dir, source))?;
            if let Some(id) = entry.file_name().to_str().and_then(|n| n.parse().ok()) {
                apps.push(id);
            }
        }
        apps.sort();
        Ok(apps)
    }

    /// One app's directory.
    pub fn app_dir(&self, app: &AppId) -> PathBuf {
        self.root.join("apps").join(app.as_str())
    }

    /// Where an app's releases live.
    pub fn releases_dir(&self, app: &AppId) -> PathBuf {
        self.app_dir(app).join("releases")
    }

    /// One release directory.
    pub fn release_dir(&self, app: &AppId, version: &Version) -> PathBuf {
        self.releases_dir(app).join(version.to_string())
    }

    /// The file naming the selected version.
    pub fn current_file(&self, app: &AppId) -> PathBuf {
        self.app_dir(app).join("current")
    }

    /// The file naming the version to fall back to.
    pub fn previous_file(&self, app: &AppId) -> PathBuf {
        self.app_dir(app).join("previous")
    }

    /// An app's private writable directory. Never inside a release.
    pub fn data_dir(&self, app: &AppId) -> PathBuf {
        self.root.join("data").join(app.as_str())
    }

    /// A shared area, reachable only through an explicit grant (§11).
    pub fn shared_dir(&self, grant: &str) -> PathBuf {
        self.root.join("shared").join(grant)
    }

    /// Where in-progress work lives.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// The host's bookkeeping directory.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// Where install intents are recorded before they are acted on.
    pub fn journal_dir(&self) -> PathBuf {
        self.state_dir().join("journal")
    }

    /// Where per-app locks live.
    pub fn locks_dir(&self) -> PathBuf {
        self.state_dir().join("locks")
    }

    /// Every complete release of an app, oldest first.
    ///
    /// Incomplete directories — the debris of an interrupted install — are not
    /// listed. Nothing that is not complete is ever offered to anything.
    pub fn installed_versions(&self, app: &AppId) -> Result<Vec<Version>, StoreError> {
        let dir = self.releases_dir(app);
        let mut versions = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(versions),
            Err(source) => return Err(StoreError::io(&dir, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| StoreError::io(&dir, source))?;
            let Some(version) = entry
                .file_name()
                .to_str()
                .and_then(|name| Version::parse(name).ok())
            else {
                continue;
            };
            if read_marker(&self.release_dir(app, &version))?.is_some() {
                versions.push(version);
            }
        }
        versions.sort();
        Ok(versions)
    }

    /// The selected version, if it is selected *and* complete.
    ///
    /// A selection pointing at a directory that is missing or half-written
    /// reads as no selection, rather than as a launchable app. Recovery
    /// repairs it; until then nothing will try to run it.
    pub fn current(&self, app: &AppId) -> Result<Option<Version>, StoreError> {
        self.usable_selection(app, &self.current_file(app))
    }

    /// The fallback version, if it is recorded *and* complete.
    pub fn previous(&self, app: &AppId) -> Result<Option<Version>, StoreError> {
        self.usable_selection(app, &self.previous_file(app))
    }

    /// What a release directory records about itself.
    pub fn release_marker(&self, app: &AppId, version: &Version) -> Result<Marker, StoreError> {
        read_marker(&self.release_dir(app, version))?.ok_or_else(|| StoreError::NotInstalled {
            app: app.clone(),
            version: version.clone(),
        })
    }

    fn usable_selection(&self, app: &AppId, path: &Path) -> Result<Option<Version>, StoreError> {
        let Some(version) = read_selection(path)? else {
            return Ok(None);
        };
        if read_marker(&self.release_dir(app, &version))?.is_some() {
            Ok(Some(version))
        } else {
            Ok(None)
        }
    }
}

/// What a completed release directory records about itself.
///
/// Provenance, not configuration: it answers "where did these bytes come from"
/// long after the catalog that served them has moved on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// The digest of the archive these bytes came out of.
    pub digest: Digest,
    /// The trusted key that signed the release descriptor.
    pub signer: KeyId,
    /// When the install committed, as seconds since the Unix epoch.
    pub installed: u64,
}

impl Marker {
    /// The file body for a completed release.
    pub fn to_document(&self) -> String {
        format!(
            "# Written by the installer. A release directory without this file is\n\
             # incomplete and will never be launched.\n\
             digest = \"{}\"\n\
             signer = \"{}\"\n\
             installed = {}\n",
            self.digest, self.signer, self.installed
        )
    }
}

/// Reads a release directory's marker, or `None` if it has none.
///
/// The one place a marker is parsed. "Is this release complete?" has exactly
/// one answer, and it is this function returning `Some`.
pub fn read_marker(release: &Path) -> Result<Option<Marker>, StoreError> {
    let path = release.join(RELEASE_MARKER);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(StoreError::io(&path, source)),
    };
    let raw: RawMarker =
        toml::from_str(&text).map_err(|_| StoreError::Corrupt { path: path.clone() })?;
    let digest = raw
        .digest
        .parse()
        .map_err(|_| StoreError::Corrupt { path: path.clone() })?;
    let signer = raw
        .signer
        .parse()
        .map_err(|_| StoreError::Corrupt { path: path.clone() })?;
    Ok(Some(Marker {
        digest,
        signer,
        installed: raw.installed,
    }))
}

/// Reads a `current` or `previous` file.
fn read_selection(path: &Path) -> Result<Option<Version>, StoreError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(StoreError::io(path, source)),
    };
    match Version::parse(text.trim()) {
        Ok(version) => Ok(Some(version)),
        Err(_) => Err(StoreError::Corrupt {
            path: path.to_path_buf(),
        }),
    }
}

/// Replaces the contents of `path` durably, creating it if it does not exist.
///
/// See the module docs for why this is four steps and not one.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let temporary = parent.join(temporary_name(path));

    let outcome = (|| -> Result<(), StoreError> {
        let mut file =
            File::create(&temporary).map_err(|source| StoreError::io(&temporary, source))?;
        io::Write::write_all(&mut file, contents)
            .map_err(|source| StoreError::io(&temporary, source))?;
        // Before the rename, never after: a rename over durable content is
        // recoverable, a rename over content still in a cache is not.
        file.sync_all()
            .map_err(|source| StoreError::io(&temporary, source))?;
        drop(file);
        fs::rename(&temporary, path).map_err(|source| StoreError::io(path, source))?;
        sync_directory(parent)
    })();

    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}

/// Moves a fully-built directory into its final name, durably.
///
/// `staged` must be complete — including its marker — because after this
/// returns, anything reading the store may use it. The whole tree is fsynced
/// before the rename, so the name and the bytes become visible together.
pub fn commit_directory(staged: &Path, destination: &Path) -> Result<(), StoreError> {
    sync_tree(staged)?;
    let parent = destination.parent().unwrap_or(Path::new("."));
    create_dir_if_missing(parent)?;
    fs::rename(staged, destination).map_err(|source| StoreError::io(destination, source))?;
    sync_directory(parent)
}

/// `fsync`s every file and directory under `path`, depth first.
pub fn sync_tree(path: &Path) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| StoreError::io(path, source))?;
    if metadata.is_dir() {
        let entries = fs::read_dir(path).map_err(|source| StoreError::io(path, source))?;
        for entry in entries {
            let entry = entry.map_err(|source| StoreError::io(path, source))?;
            sync_tree(&entry.path())?;
        }
        sync_directory(path)
    } else {
        let file = File::open(path).map_err(|source| StoreError::io(path, source))?;
        file.sync_all()
            .map_err(|source| StoreError::io(path, source))
    }
}

/// `fsync`s a directory, so the names it holds survive a power cut.
pub fn sync_directory(path: &Path) -> Result<(), StoreError> {
    let dir = File::open(path).map_err(|source| StoreError::io(path, source))?;
    match dir.sync_all() {
        Ok(()) => Ok(()),
        // macOS answers `F_FULLFSYNC` on a directory with `EINVAL`. The
        // ordinary `fsync` underneath `sync_data` is what Linux would have
        // done anyway, and it is the call that matters here.
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => dir
            .sync_data()
            .map_err(|source| StoreError::io(path, source)),
        Err(source) => Err(StoreError::io(path, source)),
    }
}

/// Creates a directory, tolerating one that is already there.
pub fn create_dir_if_missing(path: &Path) -> Result<(), StoreError> {
    match fs::create_dir_all(path) {
        Ok(()) => Ok(()),
        Err(source) => Err(StoreError::io(path, source)),
    }
}

/// Removes a directory tree, tolerating one that is already gone.
pub fn remove_tree(path: &Path) -> Result<(), StoreError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StoreError::io(path, source)),
    }
}

/// Seconds since the Unix epoch, for stamping markers and journals.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An exclusive claim on one app's install state.
///
/// A directory, created with `create_dir`, because that is the one
/// cross-platform exclusive-create the standard library offers without
/// reaching for `unsafe` (§7 bans it outside `platform/device/native`). It
/// serialises the conflicting operations §12 asks about: two installs of the
/// same app, or an install racing a rollback.
///
/// Released on drop, including on an unwind. A lock left behind by a killed
/// process is stale, and [`AppLock::acquire`] says so rather than waiting
/// forever on a process that is not coming back.
#[derive(Debug)]
pub struct AppLock {
    path: PathBuf,
}

impl AppLock {
    /// Removes every lock, and says how many there were.
    ///
    /// # When this is safe, and only then
    ///
    /// **Call this only from recovery, at host start, before anything has been
    /// launched.** A lock existing at that moment cannot be held by a live
    /// operation, because nothing has had the chance to start one.
    ///
    /// It exists because [`Drop`] is not a guarantee. A cleanly-exiting process
    /// releases its lock; one that is killed — `SIGKILL`, an OOM, a battery
    /// that ran out mid-install — does not, and the directory it left behind
    /// would otherwise refuse every future install of that app forever. §12
    /// requires that an App Store closing or crashing does not invalidate a
    /// host-owned transaction, and a lock nobody can ever clear is exactly
    /// that kind of invalidation.
    ///
    /// Checking whether the recorded pid is still alive would be the precise
    /// answer, and is deliberately not done: it needs `kill(2)` or `/proc`, and
    /// §7 bans `unsafe` outside `platform/device/native` while `/proc` does not
    /// exist on the Mac this also has to run on. "Nothing is running yet" is a
    /// weaker precondition that the caller can actually guarantee.
    pub fn break_all(layout: &Layout) -> Result<usize, StoreError> {
        let dir = layout.locks_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(source) => return Err(StoreError::io(&dir, source)),
        };
        let mut broken = 0;
        for entry in entries {
            let entry = entry.map_err(|source| StoreError::io(&dir, source))?;
            if entry.path().is_dir() {
                fs::remove_dir_all(entry.path())
                    .map_err(|source| StoreError::io(&entry.path(), source))?;
                broken += 1;
            }
        }
        Ok(broken)
    }

    /// Claims `app`, or reports who holds it.
    pub fn acquire(layout: &Layout, app: &AppId) -> Result<Self, StoreError> {
        create_dir_if_missing(&layout.locks_dir())?;
        let path = layout.locks_dir().join(format!("{app}.lock"));
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(StoreError::Locked {
                    app: app.clone(),
                    holder: fs::read_to_string(path.join("owner"))
                        .unwrap_or_else(|_| "an unknown process".to_owned())
                        .trim()
                        .to_owned(),
                });
            }
            Err(source) => return Err(StoreError::io(&path, source)),
        }
        let _ = fs::write(
            path.join("owner"),
            format!("pid {} since {}\n", std::process::id(), now()),
        );
        Ok(Self { path })
    }
}

impl Drop for AppLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A temporary name in the same directory as `path`.
///
/// Same directory because a rename across filesystems is not a rename, and
/// `/tmp` is a different filesystem often enough to matter. The name carries
/// the reserved prefix so a leftover is recognisable as ours.
fn temporary_name(path: &Path) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        ".paperclip-tmp.{stem}.{}.{nanos}.{count}",
        std::process::id()
    )
}

/// Why the store could not be read or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A filesystem operation failed.
    #[error("cannot access {path}")]
    Io {
        /// Which path.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },

    /// A file the installer wrote is not readable as what it should be.
    #[error("{path} is not readable as something this installer wrote")]
    Corrupt {
        /// Which file.
        path: PathBuf,
    },

    /// Another process holds the app's install lock.
    #[error("another operation on `{app}` is in progress ({holder})")]
    Locked {
        /// Which app.
        app: AppId,
        /// Who holds it, as recorded in the lock.
        holder: String,
    },

    /// A version that is not installed, or not completely.
    #[error("`{app}` {version} is not installed")]
    NotInstalled {
        /// Which app.
        app: AppId,
        /// Which version.
        version: Version,
    },
}

impl StoreError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMarker {
    digest: String,
    signer: String,
    installed: u64,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use semver::Version;

    use super::{
        AppLock, Layout, Marker, RELEASE_MARKER, StoreError, atomic_write, commit_directory,
    };
    use crate::digest::Digest;
    use crate::id::AppId;

    fn app() -> AppId {
        "dev.calum.chess".parse().unwrap()
    }

    fn marker() -> Marker {
        Marker {
            digest: Digest::of_bytes(b"archive"),
            signer: "0011223344556677".parse().unwrap(),
            installed: 1_758_000_000,
        }
    }

    /// A complete release directory at `version`.
    fn install(layout: &Layout, version: &str) {
        let version = Version::parse(version).unwrap();
        let dir = layout.release_dir(&app(), &version);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("paper.toml"), b"x").unwrap();
        fs::write(dir.join(RELEASE_MARKER), marker().to_document()).unwrap();
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_debris() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current");
        atomic_write(&path, b"0.1.0\n").unwrap();
        atomic_write(&path, b"0.2.0\n").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "0.2.0\n");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "current")
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn a_marker_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        install(&layout, "0.1.0");
        assert_eq!(
            layout
                .release_marker(&app(), &Version::parse("0.1.0").unwrap())
                .unwrap(),
            marker()
        );
    }

    #[test]
    fn an_unmarked_release_directory_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let version = Version::parse("0.1.0").unwrap();
        fs::create_dir_all(layout.release_dir(&app(), &version)).unwrap();

        assert!(layout.installed_versions(&app()).unwrap().is_empty());
        assert!(matches!(
            layout.release_marker(&app(), &version),
            Err(StoreError::NotInstalled { .. })
        ));
    }

    #[test]
    fn a_selection_pointing_at_an_incomplete_release_reads_as_no_selection() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        install(&layout, "0.1.0");
        atomic_write(&layout.current_file(&app()), b"0.2.0\n").unwrap();

        // 0.2.0 was never completed, so nothing will try to launch it.
        assert_eq!(layout.current(&app()).unwrap(), None);

        atomic_write(&layout.current_file(&app()), b"0.1.0\n").unwrap();
        assert_eq!(
            layout.current(&app()).unwrap(),
            Some(Version::parse("0.1.0").unwrap())
        );
    }

    #[test]
    fn installed_versions_are_sorted_and_exclude_debris() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        install(&layout, "0.2.0");
        install(&layout, "0.1.0");
        fs::create_dir_all(layout.releases_dir(&app()).join("not-a-version")).unwrap();

        let versions: Vec<String> = layout
            .installed_versions(&app())
            .unwrap()
            .iter()
            .map(Version::to_string)
            .collect();
        assert_eq!(versions, vec!["0.1.0", "0.2.0"]);
    }

    #[test]
    fn a_corrupt_selection_is_an_error_and_not_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        install(&layout, "0.1.0");
        fs::write(layout.current_file(&app()), b"latest\n").unwrap();
        assert!(matches!(
            layout.current(&app()),
            Err(StoreError::Corrupt { .. })
        ));
    }

    #[test]
    fn commit_directory_publishes_a_tree_under_its_final_name() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();

        let staged = layout.staging_dir().join("txn").join("payload");
        fs::create_dir_all(staged.join("bin")).unwrap();
        fs::write(staged.join("bin/chess"), b"binary").unwrap();
        fs::write(staged.join(RELEASE_MARKER), marker().to_document()).unwrap();

        let version = Version::parse("0.3.0").unwrap();
        commit_directory(&staged, &layout.release_dir(&app(), &version)).unwrap();

        assert!(!staged.exists());
        assert_eq!(
            layout.installed_versions(&app()).unwrap(),
            vec![version.clone()]
        );
        assert_eq!(
            fs::read(layout.release_dir(&app(), &version).join("bin/chess")).unwrap(),
            b"binary"
        );
    }

    #[test]
    fn a_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();

        let held = AppLock::acquire(&layout, &app()).unwrap();
        assert!(matches!(
            AppLock::acquire(&layout, &app()),
            Err(StoreError::Locked { .. })
        ));
        drop(held);
        assert!(AppLock::acquire(&layout, &app()).is_ok());
    }

    #[test]
    fn breaking_locks_clears_what_a_killed_process_left() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();

        // A lock with no process behind it: `Drop` never ran.
        let stale = layout.locks_dir().join("dev.calum.chess.lock");
        fs::create_dir_all(&stale).unwrap();
        assert!(matches!(
            AppLock::acquire(&layout, &app()),
            Err(StoreError::Locked { .. })
        ));

        assert_eq!(AppLock::break_all(&layout).unwrap(), 1);
        assert!(!stale.exists());
        assert!(AppLock::acquire(&layout, &app()).is_ok());

        // Idempotent, and fine on a store that has never been locked.
        assert_eq!(AppLock::break_all(&layout).unwrap(), 0);
    }

    #[test]
    fn different_apps_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();

        let _chess = AppLock::acquire(&layout, &app()).unwrap();
        let store: AppId = "dev.calum.app-store".parse().unwrap();
        assert!(AppLock::acquire(&layout, &store).is_ok());
    }

    #[test]
    fn writable_data_is_never_inside_a_release() {
        let layout = Layout::new("/srv/paperclip");
        let version = Version::parse("0.1.0").unwrap();
        assert!(
            !layout
                .data_dir(&app())
                .starts_with(layout.release_dir(&app(), &version))
        );
        assert!(!layout.data_dir(&app()).starts_with(layout.app_dir(&app())));
    }
}
