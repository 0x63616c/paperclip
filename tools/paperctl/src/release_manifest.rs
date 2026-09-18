//! `release.toml`: what the platform declares about its next release (§13,
//! WWW-61, ADR-0026).
//!
//! Every app declares its own version in `apps/<app>/paper.toml`
//! ([`paper_packages::Manifest`]); nothing declared the platform's, so
//! `paperctl upgrade package` took every field on the command line and
//! nothing in the repository recorded them. This is the file that does, and
//! this reader is `paperctl upgrade package`'s side of it — `cargo xtask
//! plan-release` reads the same file for the one field it needs
//! (`[release].version`) with its own small parser rather than sharing this
//! one, because a struct built for two readers with different needs is the
//! premature abstraction, not the duplication.

use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::Deserialize;

/// The file name, at the repository root.
pub(crate) const RELEASE_MANIFEST_FILE_NAME: &str = "release.toml";

/// Largest `release.toml` that will be read, in bytes. The same ceiling
/// [`paper_packages::MAX_MANIFEST_BYTES`] uses for `paper.toml`: a
/// real one is a few hundred bytes, and this only guards against an
/// unreasonable file rather than describing a working limit.
pub(crate) const MAX_RELEASE_MANIFEST_BYTES: u64 = 64 * 1024;

/// A parsed, validated `release.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredRelease {
    /// The release version.
    pub version: Version,
    /// The protocol this platform speaks, unparsed — the caller decides what
    /// to parse it as.
    pub protocol: String,
    /// The persistent state version this release writes.
    pub state_writes: u32,
    /// The lowest state version that can still read what this release
    /// writes.
    pub state_readable_back_to: u32,
}

#[derive(Debug, Deserialize)]
struct RawRelease {
    release: RawReleaseTable,
}

#[derive(Debug, Deserialize)]
struct RawReleaseTable {
    version: String,
    protocol: String,
    state: RawState,
}

#[derive(Debug, Deserialize)]
struct RawState {
    writes: u32,
    readable_back_to: u32,
}

/// Why `release.toml` could not be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReleaseManifestError {
    /// The file could not be read.
    #[error("cannot read {path}")]
    Read {
        /// Which file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The file is larger than [`MAX_RELEASE_MANIFEST_BYTES`].
    #[error("{path} is {len} bytes, larger than the {MAX_RELEASE_MANIFEST_BYTES} byte limit")]
    TooLarge {
        /// Which file.
        path: PathBuf,
        /// Its actual size.
        len: u64,
    },
    /// The file did not parse as the expected shape.
    #[error("{path} is not a valid release manifest")]
    Syntax {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: toml::de::Error,
    },
    /// `[release].version` was not SemVer.
    #[error("{path}'s version is not SemVer")]
    Version {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: semver::Error,
    },
}

impl DeclaredRelease {
    /// Reads and validates `release.toml` at `path`.
    ///
    /// # Errors
    ///
    /// The file is missing, oversized, not valid TOML in the expected shape,
    /// or its version is not SemVer.
    pub(crate) fn read(path: &Path) -> Result<Self, ReleaseManifestError> {
        let metadata = fs::metadata(path).map_err(|source| ReleaseManifestError::Read {
            path: path.to_owned(),
            source,
        })?;
        if metadata.len() > MAX_RELEASE_MANIFEST_BYTES {
            return Err(ReleaseManifestError::TooLarge {
                path: path.to_owned(),
                len: metadata.len(),
            });
        }
        let text = fs::read_to_string(path).map_err(|source| ReleaseManifestError::Read {
            path: path.to_owned(),
            source,
        })?;
        let raw: RawRelease =
            toml::from_str(&text).map_err(|source| ReleaseManifestError::Syntax {
                path: path.to_owned(),
                source,
            })?;
        let version = raw.release.version.parse::<Version>().map_err(|source| {
            ReleaseManifestError::Version {
                path: path.to_owned(),
                source,
            }
        })?;
        Ok(Self {
            version,
            protocol: raw.release.protocol,
            state_writes: raw.release.state.writes,
            state_readable_back_to: raw.release.state.readable_back_to,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &tempfile::TempDir, contents: &str) -> PathBuf {
        let path = dir.path().join("release.toml");
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn reads_a_well_formed_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "[release]\n\
             version = \"0.4.0\"\n\
             protocol = \"1.0\"\n\
             \n\
             [release.state]\n\
             writes = 2\n\
             readable_back_to = 1\n",
        );
        let declared = DeclaredRelease::read(&path).unwrap();
        assert_eq!(declared.version, Version::new(0, 4, 0));
        assert_eq!(declared.protocol, "1.0");
        assert_eq!(declared.state_writes, 2);
        assert_eq!(declared.state_readable_back_to, 1);
    }

    #[test]
    fn the_two_state_fields_are_read_independently_not_defaulted_from_each_other() {
        // A regression test for the swap ADR-0026 exists to prevent: if
        // `readable_back_to` were ever accidentally read from the same key
        // as `writes`, this would not catch it structurally, but it does
        // prove the two are parsed from distinct keys rather than one value
        // copied to both fields.
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "[release]\n\
             version = \"1.0.0\"\n\
             protocol = \"1.0\"\n\
             \n\
             [release.state]\n\
             writes = 5\n\
             readable_back_to = 3\n",
        );
        let declared = DeclaredRelease::read(&path).unwrap();
        assert_ne!(declared.state_writes, declared.state_readable_back_to);
        assert_eq!(declared.state_writes, 5);
        assert_eq!(declared.state_readable_back_to, 3);
    }

    #[test]
    fn a_missing_state_table_is_a_syntax_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "[release]\nversion = \"0.1.0\"\nprotocol = \"1.0\"\n");
        assert!(matches!(
            DeclaredRelease::read(&path),
            Err(ReleaseManifestError::Syntax { .. })
        ));
    }

    #[test]
    fn a_non_semver_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "[release]\n\
             version = \"not-semver\"\n\
             protocol = \"1.0\"\n\
             \n\
             [release.state]\n\
             writes = 1\n\
             readable_back_to = 1\n",
        );
        assert!(matches!(
            DeclaredRelease::read(&path),
            Err(ReleaseManifestError::Version { .. })
        ));
    }

    #[test]
    fn a_missing_file_is_a_read_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        assert!(matches!(
            DeclaredRelease::read(&path),
            Err(ReleaseManifestError::Read { .. })
        ));
    }
}
