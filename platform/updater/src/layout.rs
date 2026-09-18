//! Where a platform release lives, and what makes `current` legible (§13).
//!
//! ```text
//! /home/root/paperclip/
//!   bin/paperctl              the recovery bootstrap   ─┐ outside `releases/`:
//!   bin/paperclip-updater     the updater              ─┘ never replaced by an
//!                                                        ordinary update
//!   keys/                     trusted public keys
//!   releases/<version>/       one complete platform release, immutable
//!     platform.toml           the signed manifest
//!     platform.toml.sig
//!     bin/{paperclip-host,paperclip-compositor,home,app-store,settings}
//!   current   -> releases/0.4.0
//!   previous  -> releases/0.3.1
//!   staging/<transaction>/    extraction in progress
//!   state/update.json         the update journal
//!   state/platform/           the platform's own persistent state
//!   state/snapshots/<version>/  a copy of it, taken when rollback needs one
//!   storage/<app>/            per-app persistent storage
//! ```
//!
//! # Two symlinks, not one and a search
//!
//! `previous` is written down rather than derived. Deriving it — "the
//! highest version in `releases/` that is not `current`" — is a rule that is
//! right until someone has three releases on disk, and then it silently picks
//! a release that was never the one running. A named symlink makes rollback a
//! swap, and makes the state answerable with `ls -l` over SSH by someone who
//! has no working screen to read it from.
//!
//! # Apps cannot reach any of this
//!
//! The app store is a different tree — `paper_packages::store::DEVICE_ROOT`,
//! under `~/.local/share` — and [`PlatformLayout::separate_from`] refuses to
//! operate when the two overlap. That is the structural half of §13's "routine
//! app installation can never replace the host": there is no `AppId` for which
//! `Layout::release_dir` produces a path inside this tree, because this tree is
//! not under that root. The other half is
//! [`Domain::PLATFORM`](paper_packages::signing::Domain::PLATFORM): a signature
//! over an app release does not verify as a platform manifest.

use std::fs;
use std::path::{Component, Path, PathBuf};

use paper_host::units::SessionPaths;
use paper_packages::store::{self, Layout};
use semver::Version;

use crate::error::UpdateError;

/// The marker that says a directory is a Paperclip platform root.
///
/// Written by [`PlatformLayout::ensure`] and required by removal, which
/// otherwise has only a path to go on and no way to tell a platform root from
/// a home directory somebody mistyped.
pub const PLATFORM_MARKER: &str = ".paperclip-platform";

/// The directory holding the platform's releases and its own state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformLayout {
    root: PathBuf,
}

impl PlatformLayout {
    /// A layout rooted at `root`. Nothing is created until [`Self::ensure`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The layout implied by a session's paths, so the updater and the
    /// supervisor can never disagree about where `current` is.
    pub fn from_session(paths: &SessionPaths) -> Self {
        Self::new(&paths.root)
    }

    /// The root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where releases are unpacked.
    pub fn releases_dir(&self) -> PathBuf {
        self.root.join("releases")
    }

    /// One release directory.
    pub fn release_dir(&self, version: &Version) -> PathBuf {
        self.releases_dir().join(version.to_string())
    }

    /// The selected release symlink.
    pub fn current(&self) -> PathBuf {
        self.root.join("current")
    }

    /// The fallback release symlink.
    pub fn previous(&self) -> PathBuf {
        self.root.join("previous")
    }

    /// Where a bundle is unpacked before it is anything.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// The platform's own bookkeeping. Survives a reboot, unlike the session
    /// state under `/run`.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// The update journal.
    pub fn journal_file(&self) -> PathBuf {
        self.state_dir().join("update.json")
    }

    /// The platform's persistent state, the thing a rollback has to be able to
    /// read after a newer release has written it.
    pub fn platform_state(&self) -> PathBuf {
        self.state_dir().join("platform")
    }

    /// Snapshots of [`Self::platform_state`], keyed by the release they were
    /// taken for.
    pub fn snapshot_dir(&self, version: &Version) -> PathBuf {
        self.state_dir().join("snapshots").join(version.to_string())
    }

    /// Trusted public keys. Outside `releases/`, because a release must not be
    /// able to widen the set of keys that can sign its successor.
    pub fn keys_dir(&self) -> PathBuf {
        self.root.join("keys")
    }

    /// The updater and the recovery bootstrap.
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// Creates the directories and the root marker.
    ///
    /// Idempotent: running it against an established root changes nothing,
    /// which is what §14 asks of `setup`.
    ///
    /// # Errors
    ///
    /// Any directory or the marker that cannot be created.
    pub fn ensure(&self) -> Result<(), UpdateError> {
        for directory in [
            self.root.clone(),
            self.releases_dir(),
            self.staging_dir(),
            self.state_dir(),
            self.platform_state(),
            self.state_dir().join("snapshots"),
            self.keys_dir(),
            self.bin_dir(),
        ] {
            fs::create_dir_all(&directory).map_err(|source| UpdateError::Io {
                path: directory.clone(),
                source,
            })?;
        }
        let marker = self.root.join(PLATFORM_MARKER);
        if !marker.exists() {
            store::atomic_write(&marker, b"paperclip platform root\n")?;
        }
        self.ensure_traversable()?;
        Ok(())
    }

    /// Give every ancestor of the platform root the execute bit for "other".
    ///
    /// `paperclip-app@.service` runs as `User=xochitl` (uid 500) with
    /// `WorkingDirectory=` inside this root, and on a stock tablet
    /// `/home/root` is `drwx------ root root`. An unprivileged app therefore
    /// cannot *traverse* to its own working directory, and systemd fails the
    /// unit with `200/CHDIR` before the app's `main` is ever entered — which
    /// the supervisor can only report as "systemd refused to start
    /// paperclip-app@home.service", with no reason attached.
    ///
    /// Execute-without-read is the whole of what is granted: `0o111` permits
    /// walking *through* a directory to a path already known, and still
    /// refuses listing it. `/home/root`'s own contents — the notebooks this
    /// device exists for — stay as unreadable to uid 500 as they were.
    ///
    /// Here rather than in a setup script because it has to be true on every
    /// device Paperclip is ever installed on, not just the one it was first
    /// debugged on: WWW-55 found it by hand-fixing a tablet, and a hand fix
    /// is exactly what does not survive the next install.
    #[cfg(unix)]
    fn ensure_traversable(&self) -> Result<(), UpdateError> {
        use std::os::unix::fs::PermissionsExt as _;

        for ancestor in self.root.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            let Ok(metadata) = fs::metadata(ancestor) else {
                // An ancestor we cannot stat is one we also cannot fix, and
                // it is not this function's business to invent it.
                continue;
            };
            let mode = metadata.permissions().mode();
            if mode & 0o001 != 0 {
                continue;
            }
            let mut permissions = metadata.permissions();
            permissions.set_mode(mode | 0o001);
            // Best effort, and deliberately not fatal. The ancestors of a
            // platform root are not all ours to change: under a test's
            // temporary directory they belong to the system, and a root
            // somewhere unexpected on a real machine may sit below a
            // directory this process has no business widening. Failing
            // `ensure` over one of those would turn "the layout is
            // established" into "the layout is established only where every
            // parent happened to be writable", which is a worse contract
            // than doing what can be done and saying what could not. The
            // symptom this prevents — `200/CHDIR` — is reported clearly by
            // systemd if a parent really was the blocker.
            if let Err(error) = fs::set_permissions(ancestor, permissions) {
                tracing::debug!(
                    path = %ancestor.display(),
                    %error,
                    "could not add the traversal bit; an unprivileged app may not reach the root"
                );
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn ensure_traversable(&self) -> Result<(), UpdateError> {
        Ok(())
    }

    /// Whether this looks like a platform root that [`Self::ensure`] made.
    pub fn is_established(&self) -> bool {
        self.root.join(PLATFORM_MARKER).is_file()
    }

    /// The selected release, read from the `current` symlink.
    ///
    /// # Errors
    ///
    /// A `current` that exists but does not name a release directory. That is
    /// a corrupted selection rather than an absent one, and answering `None`
    /// would let an upgrade proceed as if this were a first install.
    pub fn selected(&self) -> Result<Option<Version>, UpdateError> {
        read_selection(&self.current())
    }

    /// The fallback release, read from the `previous` symlink.
    ///
    /// # Errors
    ///
    /// As [`Self::selected`].
    pub fn fallback(&self) -> Result<Option<Version>, UpdateError> {
        read_selection(&self.previous())
    }

    /// Every release on disk, ascending. Unparseable directory names are
    /// skipped rather than fatal — they cannot be selected, because a
    /// selection is a [`Version`].
    ///
    /// # Errors
    ///
    /// If `releases/` cannot be read and exists.
    pub fn installed(&self) -> Result<Vec<Version>, UpdateError> {
        let releases = self.releases_dir();
        let entries = match fs::read_dir(&releases) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(UpdateError::Io {
                    path: releases,
                    source,
                });
            }
        };
        let mut versions: Vec<Version> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| Version::parse(&entry.file_name().to_string_lossy()).ok())
            .collect();
        versions.sort();
        Ok(versions)
    }

    /// Points `link` at `releases/<version>`, atomically.
    ///
    /// A symlink cannot be replaced in place — `symlink(2)` fails on an
    /// existing name — so this is the same write-rename-fsync dance every
    /// other commit in Paperclip uses, with a symlink as the content. The
    /// final `fsync` is of the *directory*, which is what makes the new name
    /// survive a power cut; skipping it leaves a window in which `current` is
    /// neither the old release nor the new one.
    ///
    /// # Errors
    ///
    /// Any filesystem failure, and a version with no release directory —
    /// pointing `current` at nothing is the one outcome this must never
    /// produce.
    pub fn select(&self, link: &Path, version: &Version) -> Result<(), UpdateError> {
        let release = self.release_dir(version);
        if !release.is_dir() {
            return Err(UpdateError::NoSuchRelease {
                version: version.clone(),
            });
        }
        // Relative, so the whole tree can be moved, inspected from a mounted
        // backup, or rebased under a VM prefix and still resolve.
        let target = Path::new("releases").join(version.to_string());
        let temporary = link.with_extension("swapping");
        let _ = fs::remove_file(&temporary);
        std::os::unix::fs::symlink(&target, &temporary).map_err(|source| UpdateError::Io {
            path: temporary.clone(),
            source,
        })?;
        fs::rename(&temporary, link).map_err(|source| UpdateError::Io {
            path: link.to_path_buf(),
            source,
        })?;
        let parent = link.parent().unwrap_or(&self.root);
        store::sync_directory(parent)?;
        Ok(())
    }

    /// Removes a selection symlink, durably.
    ///
    /// # Errors
    ///
    /// Any filesystem failure other than the link already being absent.
    pub fn deselect(&self, link: &Path) -> Result<(), UpdateError> {
        match fs::remove_file(link) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(UpdateError::Io {
                    path: link.to_path_buf(),
                    source,
                });
            }
        }
        store::sync_directory(link.parent().unwrap_or(&self.root))?;
        Ok(())
    }

    /// Refuses a platform root that overlaps the app store's root.
    ///
    /// §13: routine app installation can never replace the host. The way that
    /// is made structural rather than conventional is that the two trees are
    /// disjoint — so no app id, however chosen, names a path in here. This is
    /// the check that says so out loud, and it is called before anything in
    /// either tree is written.
    ///
    /// # Errors
    ///
    /// [`UpdateError::OverlappingRoots`] if either root contains the other.
    pub fn separate_from(&self, store: &Layout) -> Result<(), UpdateError> {
        let platform = normalise(&self.root);
        let apps = normalise(store.root());
        if platform.starts_with(&apps) || apps.starts_with(&platform) {
            return Err(UpdateError::OverlappingRoots {
                platform: self.root.clone(),
                apps: store.root().to_path_buf(),
            });
        }
        Ok(())
    }
}

/// Reads a selection symlink into a version.
fn read_selection(link: &Path) -> Result<Option<Version>, UpdateError> {
    let target = match fs::read_link(link) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(UpdateError::Io {
                path: link.to_path_buf(),
                source,
            });
        }
    };
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Version::parse(&name)
        .map(Some)
        .map_err(|_| UpdateError::CorruptSelection {
            link: link.to_path_buf(),
            target,
        })
}

/// Lexical normalisation, enough to compare two roots for containment.
///
/// Not `canonicalize`: these paths may not exist yet, and a root that cannot
/// be compared until it has been created is a check that runs too late.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}
