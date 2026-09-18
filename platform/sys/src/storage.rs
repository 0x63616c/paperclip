//! Writing to disk, behind a seam.
//!
//! `platform/packages/src/install.rs` and `platform/updater/src/upgrade.rs`
//! both perform every byte of a power-cut-sensitive operation inline against
//! `std::fs`, which is why their power-cut cases are simulated by hand-deleting
//! directories between two calls rather than by injecting a fault mid-operation
//! (WWW-46). This trait is the seam that would let a fault be injected instead;
//! `platform/testing`'s `FakeStorage` is where that injection lives.
//!
//! Adopting this in `platform/packages` and `platform/updater` in place of
//! their direct `std::fs` calls is deliberately **not** done by this change —
//! see the WWW-46 result comment for why swapping the seam under
//! already-proven power-cut logic was left as a follow-up rather than bundled
//! here.

use std::fmt;
use std::fs::{self, File};
use std::path::Path;

/// Why a storage operation failed.
#[derive(Debug, thiserror::Error)]
#[error("{op} {path}: {source}")]
pub struct StorageError {
    /// What was being done, e.g. `"create directory"`.
    pub op: &'static str,
    /// The path involved.
    pub path: std::path::PathBuf,
    /// The underlying I/O error.
    #[source]
    pub source: std::io::Error,
}

impl StorageError {
    fn new(op: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self {
            op,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// The filesystem operations a power-cut-sensitive transaction needs.
pub trait Storage: fmt::Debug {
    /// Creates `path` and every missing parent. Not an error if it exists.
    ///
    /// # Errors
    ///
    /// If the directory could not be created.
    fn create_dir_if_missing(&self, path: &Path) -> Result<(), StorageError>;

    /// Removes a directory tree, tolerating one that is already gone.
    ///
    /// # Errors
    ///
    /// If the tree exists but could not be removed.
    fn remove_tree(&self, path: &Path) -> Result<(), StorageError>;

    /// Makes `staged` durable, then renames it to `destination`, creating
    /// `destination`'s parent first.
    ///
    /// The rename is what makes this atomic on the same filesystem: a reader
    /// sees either the old tree at `destination` or the fully-staged new one,
    /// never a partial write.
    ///
    /// # Errors
    ///
    /// If `staged` could not be made durable, `destination`'s parent could
    /// not be created, or the rename failed (for instance, across
    /// filesystems).
    fn commit_directory(&self, staged: &Path, destination: &Path) -> Result<(), StorageError>;

    /// Copies a directory tree, creating `destination`. Regular files and
    /// directories only — a snapshot that followed a symlink out of the tree
    /// would restore something the caller never captured.
    ///
    /// # Errors
    ///
    /// If a file could not be read, written, or a directory could not be
    /// created.
    fn copy_tree(&self, source: &Path, destination: &Path) -> Result<(), StorageError>;
}

/// The real one: `std::fs`, with an `fsync` before the commit rename so a
/// power cut cannot reorder "the bytes are on disk" after "the name says they
/// are".
#[derive(Debug, Clone, Copy, Default)]
pub struct Filesystem;

impl Filesystem {
    fn sync_tree(&self, path: &Path) -> Result<(), StorageError> {
        let metadata =
            fs::symlink_metadata(path).map_err(|source| StorageError::new("sync", path, source))?;
        if metadata.is_dir() {
            let entries =
                fs::read_dir(path).map_err(|source| StorageError::new("sync", path, source))?;
            for entry in entries {
                let entry = entry.map_err(|source| StorageError::new("sync", path, source))?;
                self.sync_tree(&entry.path())?;
            }
            let dir = File::open(path).map_err(|source| StorageError::new("sync", path, source))?;
            let _ = dir.sync_all();
        } else if metadata.is_file() {
            let file =
                File::open(path).map_err(|source| StorageError::new("sync", path, source))?;
            file.sync_all()
                .map_err(|source| StorageError::new("sync", path, source))?;
        }
        Ok(())
    }
}

impl Storage for Filesystem {
    fn create_dir_if_missing(&self, path: &Path) -> Result<(), StorageError> {
        fs::create_dir_all(path)
            .map_err(|source| StorageError::new("create directory", path, source))
    }

    fn remove_tree(&self, path: &Path) -> Result<(), StorageError> {
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StorageError::new("remove", path, source)),
        }
    }

    fn commit_directory(&self, staged: &Path, destination: &Path) -> Result<(), StorageError> {
        self.sync_tree(staged)?;
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        self.create_dir_if_missing(parent)?;
        fs::rename(staged, destination)
            .map_err(|source| StorageError::new("commit", destination, source))?;
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn copy_tree(&self, source: &Path, destination: &Path) -> Result<(), StorageError> {
        fs::create_dir_all(destination)
            .map_err(|source_err| StorageError::new("create directory", destination, source_err))?;
        if !source.is_dir() {
            return Ok(());
        }
        let entries =
            fs::read_dir(source).map_err(|error| StorageError::new("read", source, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| StorageError::new("read", source, error))?;
            let from = entry.path();
            let to = destination.join(entry.file_name());
            let kind = entry
                .file_type()
                .map_err(|error| StorageError::new("read", &from, error))?;
            if kind.is_dir() {
                self.copy_tree(&from, &to)?;
            } else if kind.is_file() {
                fs::copy(&from, &to).map_err(|error| StorageError::new("copy", &from, error))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_dir_if_missing_is_not_an_error_when_it_already_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = Filesystem;
        fs.create_dir_if_missing(dir.path()).expect("first");
        fs.create_dir_if_missing(dir.path())
            .expect("second, idempotent");
    }

    #[test]
    fn remove_tree_tolerates_a_path_that_is_already_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("never-existed");
        Filesystem.remove_tree(&missing).expect("not an error");
    }

    #[test]
    fn commit_directory_renames_staged_into_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        let staged = dir.path().join("staging");
        let destination = dir.path().join("nested").join("release");
        fs::create_dir_all(&staged).expect("stage");
        fs::write(staged.join("marker"), b"payload").expect("write");

        Filesystem
            .commit_directory(&staged, &destination)
            .expect("commits");

        assert!(
            !staged.exists(),
            "the staged directory was moved, not copied"
        );
        assert_eq!(
            fs::read(destination.join("marker")).expect("read back"),
            b"payload"
        );
    }

    #[test]
    fn copy_tree_reproduces_nested_files_without_touching_the_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir_all(source.join("nested")).expect("stage");
        fs::write(source.join("nested").join("file"), b"data").expect("write");

        Filesystem.copy_tree(&source, &destination).expect("copies");

        assert!(
            source.join("nested").join("file").exists(),
            "source untouched"
        );
        assert_eq!(
            fs::read(destination.join("nested").join("file")).expect("read back"),
            b"data"
        );
    }
}
