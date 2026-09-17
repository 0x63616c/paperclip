//! The four places an app is allowed to keep things (§11).
//!
//! Read-only assets, private storage, scratch, and whatever shared
//! directories a person explicitly granted. All four arrive as absolute paths
//! in [`Hello`](paper_protocol::Hello), because the host may have mounted them
//! somewhere an app could not construct for itself — an app that builds a path
//! out of its own id is an app that breaks the first time the host moves
//! anything.
//!
//! ## This is not the boundary
//!
//! [`Storage`] refuses what the app was not granted, and refuses a path that
//! escapes its directory. Both of those are **convenience**: this code is
//! statically linked into the app's own process, so an app that wants to skip
//! it simply calls `std::fs` instead. What actually stops an app reaching
//! another app's files is the restrictions the supervisor puts on the process
//! (WWW-4).
//!
//! The value of checking here anyway is that an honest app gets a typed error
//! naming the missing capability, at the call, instead of an `EACCES` from
//! three layers down.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use paper_protocol::{AppId, AppPaths, Capability, PathError, RelativePath, ShareAccess};

/// Why a storage operation did not happen.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// The app does not hold the capability this area needs.
    #[error("this app was not granted the `{capability}` capability")]
    NotGranted {
        /// Which one.
        capability: Capability,
    },

    /// The name given would have left the directory it was joined to.
    #[error("`{name}` is not a safe name inside an app directory")]
    UnsafeName {
        /// The name as given.
        name: String,
        /// What specifically was wrong with it.
        #[source]
        source: PathError,
    },

    /// No share with that app exists.
    #[error("nothing is shared with `{with}`")]
    NoSuchShare {
        /// The app that was asked about.
        with: AppId,
    },

    /// The share exists but is read-only.
    #[error("the share with `{with}` is read-only")]
    ShareIsReadOnly {
        /// The app the share is with.
        with: AppId,
    },

    /// The filesystem said no.
    #[error("cannot use {path}")]
    Io {
        /// Which path.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
}

/// An app's storage areas, as the host laid them out for this process.
#[derive(Debug, Clone)]
pub struct Storage {
    paths: AppPaths,
    storage: bool,
    sharing: bool,
}

impl Storage {
    /// Builds storage from the paths and capabilities in `Hello`.
    pub fn new(paths: AppPaths, capabilities: &[Capability]) -> Self {
        Self {
            paths,
            storage: capabilities.contains(&Capability::Storage),
            sharing: capabilities.contains(&Capability::Sharing),
        }
    }

    /// The package's own asset directory, read-only.
    ///
    /// Not gated on a capability: assets are the app's own files, shipped
    /// inside its own package, and an app that cannot read them cannot draw
    /// itself. The `storage` capability is about *writing* state that outlives
    /// a launch.
    pub fn assets_dir(&self) -> &Path {
        &self.paths.assets
    }

    /// Reads one declared asset.
    pub fn read_asset(&self, name: &str) -> Result<Vec<u8>, StorageError> {
        let path = join_safely(self.assets_dir(), name)?;
        fs::read(&path).map_err(|source| StorageError::Io { path, source })
    }

    /// The private directory, which survives a restart.
    pub fn private_dir(&self) -> Result<&Path, StorageError> {
        self.require_storage()?;
        Ok(&self.paths.private)
    }

    /// Scratch space, which may be empty on every launch.
    pub fn temp_dir(&self) -> Result<&Path, StorageError> {
        self.require_storage()?;
        Ok(&self.paths.temp)
    }

    /// Reads a file from private storage, or `None` if it is not there.
    ///
    /// Absent is not an error: the first launch of an app has no save, and a
    /// caller forced to match on `ErrorKind::NotFound` to discover that is a
    /// caller that will eventually get it wrong.
    pub fn read_private(&self, name: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let path = join_safely(self.private_dir()?, name)?;
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StorageError::Io { path, source }),
        }
    }

    /// Writes a file to private storage, atomically.
    ///
    /// Write-then-rename, because this is the call an app makes when it has
    /// been told it is about to be killed. A partial file left by a process
    /// that ran out of deadline is worse than no file at all: the next launch
    /// would load it and call it a save.
    pub fn write_private(&self, name: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let directory = self.private_dir()?;
        let path = join_safely(directory, name)?;
        let mut temporary = path.clone().into_os_string();
        temporary.push(".partial");
        let temporary = PathBuf::from(temporary);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| StorageError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&temporary, bytes).map_err(|source| StorageError::Io {
            path: temporary.clone(),
            source,
        })?;
        fs::rename(&temporary, &path).map_err(|source| StorageError::Io {
            path: path.clone(),
            source,
        })
    }

    /// Every share this app was granted.
    pub fn shares(&self) -> &[paper_protocol::SharedGrant] {
        &self.paths.shared
    }

    /// The directory shared with `with`, if there is one.
    pub fn share_dir(&self, with: &AppId) -> Result<&Path, StorageError> {
        self.require_sharing()?;
        self.paths
            .shared
            .iter()
            .find(|grant| &grant.with == with)
            .map(|grant| grant.path.as_path())
            .ok_or_else(|| StorageError::NoSuchShare { with: with.clone() })
    }

    /// The directory shared with `with`, refusing a read-only share.
    pub fn writable_share_dir(&self, with: &AppId) -> Result<&Path, StorageError> {
        self.require_sharing()?;
        let grant = self
            .paths
            .shared
            .iter()
            .find(|grant| &grant.with == with)
            .ok_or_else(|| StorageError::NoSuchShare { with: with.clone() })?;
        match grant.access {
            ShareAccess::ReadWrite => Ok(grant.path.as_path()),
            // `ShareAccess` is `#[non_exhaustive]`, so the wildcard covers a
            // level this build has never heard of. Refusing the write is the
            // only safe reading of an access mode we cannot interpret.
            _ => Err(StorageError::ShareIsReadOnly { with: with.clone() }),
        }
    }

    fn require_storage(&self) -> Result<(), StorageError> {
        if self.storage {
            Ok(())
        } else {
            Err(StorageError::NotGranted {
                capability: Capability::Storage,
            })
        }
    }

    /// Checked against the capability, not against the grant list.
    ///
    /// An app granted `sharing` with nothing shared yet gets
    /// [`StorageError::NoSuchShare`] — "there is no share with that app" —
    /// rather than being told it was never granted sharing, which would be
    /// false and would send the reader to the wrong file.
    fn require_sharing(&self) -> Result<(), StorageError> {
        if self.sharing {
            Ok(())
        } else {
            Err(StorageError::NotGranted {
                capability: Capability::Sharing,
            })
        }
    }
}

/// Joins a caller-supplied name onto a directory, refusing anything that would
/// leave it.
fn join_safely(directory: &Path, name: &str) -> Result<PathBuf, StorageError> {
    let relative: RelativePath = name.parse().map_err(|source| StorageError::UnsafeName {
        name: name.to_owned(),
        source,
    })?;
    Ok(relative.resolve_within(directory))
}

#[cfg(test)]
mod tests {
    use super::{Storage, StorageError};
    use paper_protocol::{AppId, AppPaths, Capability, ShareAccess, SharedGrant};
    use std::path::PathBuf;

    fn paths(root: &std::path::Path) -> AppPaths {
        AppPaths {
            assets: root.join("assets"),
            private: root.join("private"),
            temp: root.join("temp"),
            shared: Vec::new(),
        }
    }

    fn storage(root: &std::path::Path, capabilities: &[Capability]) -> Storage {
        Storage::new(paths(root), capabilities)
    }

    #[test]
    fn assets_are_readable_without_any_capability() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("assets")).unwrap();
        std::fs::write(root.path().join("assets/board.toml"), b"squares").unwrap();

        let storage = storage(root.path(), &[]);
        assert_eq!(storage.read_asset("board.toml").unwrap(), b"squares");
        assert!(matches!(
            storage.private_dir(),
            Err(StorageError::NotGranted {
                capability: Capability::Storage
            })
        ));
    }

    #[test]
    fn private_storage_needs_the_storage_capability() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("private")).unwrap();

        let storage = storage(root.path(), &[Capability::Storage]);
        assert!(storage.read_private("game.save").unwrap().is_none());
        storage.write_private("game.save", b"e4").unwrap();
        assert_eq!(
            storage.read_private("game.save").unwrap(),
            Some(b"e4".to_vec())
        );
    }

    /// The save an app writes on its way out must never be readable in a
    /// half-written state, so the write goes somewhere else first.
    #[test]
    fn a_private_write_leaves_nothing_partial_behind() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("private")).unwrap();
        let storage = storage(root.path(), &[Capability::Storage]);
        storage.write_private("game.save", b"e4 e5").unwrap();

        let leftovers: Vec<PathBuf> = std::fs::read_dir(root.path().join("private"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "partial"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// The convenience check, doing its one job: a name that would climb out
    /// of the app's own directory is refused before it reaches the filesystem.
    #[test]
    fn a_name_that_escapes_the_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path(), &[Capability::Storage]);
        for name in [
            "../secrets",
            "/etc/passwd",
            "a/../../b",
            "~/.ssh/id_ed25519",
        ] {
            assert!(
                matches!(
                    storage.read_asset(name),
                    Err(StorageError::UnsafeName { .. })
                ),
                "accepted {name}"
            );
        }
    }

    #[test]
    fn a_read_only_share_refuses_a_write_path() {
        let root = tempfile::tempdir().unwrap();
        let other: AppId = "dev.calum.app-store".parse().unwrap();
        let mut paths = paths(root.path());
        paths.shared.push(SharedGrant {
            with: other.clone(),
            path: root.path().join("share"),
            access: ShareAccess::Read,
        });

        let storage = Storage::new(paths, &[Capability::Sharing]);
        assert!(storage.share_dir(&other).is_ok());
        assert!(matches!(
            storage.writable_share_dir(&other),
            Err(StorageError::ShareIsReadOnly { .. })
        ));

        let stranger: AppId = "dev.calum.chess".parse().unwrap();
        assert!(matches!(
            storage.share_dir(&stranger),
            Err(StorageError::NoSuchShare { .. })
        ));
    }

    /// An app granted sharing but with nothing shared yet is told there is no
    /// such share, not that it was never granted sharing. The second sentence
    /// would be false and would send the reader to the wrong file.
    #[test]
    fn sharing_granted_with_nothing_shared_reports_the_missing_share() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path(), &[Capability::Sharing]);
        let other: AppId = "dev.calum.app-store".parse().unwrap();
        assert!(matches!(
            storage.share_dir(&other),
            Err(StorageError::NoSuchShare { .. })
        ));

        let ungranted = super::Storage::new(paths(root.path()), &[]);
        assert!(matches!(
            ungranted.share_dir(&other),
            Err(StorageError::NotGranted {
                capability: Capability::Sharing
            })
        ));
    }
}
