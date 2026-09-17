//! The package-time conformance layer (§8): everything that can be decided
//! about a package before anything runs it.
//!
//! One call, [`PackageCheck::run`], in the order a failure is most useful:
//!
//! 1. The manifest parses — ids, SemVer, protocol shape, paths, bounded sizes.
//! 2. This platform can speak the protocol it asks for.
//! 3. The payload is there and nothing on the way to it is a symlink.
//! 4. Nothing is over its size limit.
//! 5. The entrypoint is an aarch64 ELF executable.
//!
//! What is deliberately **not** here is anything about authenticity. A package
//! directory is not a published thing: the bytes a signature covers are the
//! archive's, produced by [`archive::build`](crate::archive::build) and signed
//! through [`signing`](crate::signing) and [`release`](crate::release). This
//! check answers "could this run", never "who made it", and it has no way to
//! claim otherwise because it computes nothing a signature could be checked
//! against.
//!
//! Each step assumes the earlier ones passed, and the whole thing assumes it
//! will be skipped. A package that never went through `paperctl check` is
//! exactly what the launch-time layer exists for; this one's job is to make
//! that failure happen on a developer's Mac instead of on a tablet.

use std::fs;
use std::path::{Path, PathBuf};

use crate::binary::{self, BinaryError, ExecutableTarget};
use crate::error::{ManifestError, PayloadError};
use crate::manifest::{MANIFEST_FILE_NAME, Manifest};

/// Largest entrypoint binary, in bytes.
///
/// A release-profile Rust binary for this platform is a few megabytes. 64 MiB
/// is room for one that statically links something large and still small
/// enough that an install fits in the tablet's storage many times over.
pub const MAX_ENTRYPOINT_BYTES: u64 = 64 * 1024 * 1024;

/// Largest single asset, in bytes.
pub const MAX_ASSET_BYTES: u64 = 16 * 1024 * 1024;

/// Largest package in total, in bytes.
///
/// The bound that actually matters: a personal tablet with a catalog on the
/// LAN can fill its storage with one careless package, and the install is the
/// last moment anyone can say so.
pub const MAX_PACKAGE_BYTES: u64 = 128 * 1024 * 1024;

/// What a package looked like at package time.
#[derive(Debug)]
pub struct PackageCheck {
    root: PathBuf,
    manifest: Manifest,
    target: ExecutableTarget,
    total_bytes: u64,
}

impl PackageCheck {
    /// Runs every package-time check against the package rooted at `root`.
    pub fn run(root: &Path) -> Result<Self, CheckError> {
        let manifest = Manifest::read_package(root).map_err(|source| CheckError::Manifest {
            path: root.join(MANIFEST_FILE_NAME),
            source,
        })?;

        manifest
            .ensure_runnable()
            .map_err(|source| CheckError::Manifest {
                path: root.join(MANIFEST_FILE_NAME),
                source,
            })?;

        manifest
            .validate_payload(root)
            .map_err(|source| CheckError::Payload {
                path: root.to_path_buf(),
                source,
            })?;

        let entrypoint = manifest.entrypoint().resolve_within(root);
        let mut total_bytes = file_size(&root.join(MANIFEST_FILE_NAME))?;

        let entrypoint_bytes = file_size(&entrypoint)?;
        if entrypoint_bytes > MAX_ENTRYPOINT_BYTES {
            return Err(CheckError::TooLarge {
                path: manifest.entrypoint().as_str().to_owned(),
                len: entrypoint_bytes,
                max: MAX_ENTRYPOINT_BYTES,
            });
        }
        total_bytes += entrypoint_bytes;

        for asset in manifest.assets() {
            let bytes = file_size(&asset.resolve_within(root))?;
            if bytes > MAX_ASSET_BYTES {
                return Err(CheckError::TooLarge {
                    path: asset.as_str().to_owned(),
                    len: bytes,
                    max: MAX_ASSET_BYTES,
                });
            }
            total_bytes += bytes;
        }

        if total_bytes > MAX_PACKAGE_BYTES {
            return Err(CheckError::PackageTooLarge {
                len: total_bytes,
                max: MAX_PACKAGE_BYTES,
            });
        }

        let target = binary::require_device_entrypoint(&entrypoint)?;

        Ok(Self {
            root: root.to_path_buf(),
            manifest,
            target,
            total_bytes,
        })
    }

    /// The package directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The manifest, already validated.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// What the entrypoint's ELF header says it is.
    pub fn target(&self) -> ExecutableTarget {
        self.target
    }

    /// Total declared bytes.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
}

/// Why a package did not pass.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CheckError {
    /// The manifest is wrong, or asks for a protocol this platform cannot run.
    #[error("{path} is not a valid manifest for this platform")]
    Manifest {
        /// Which manifest.
        path: PathBuf,
        /// Why.
        #[source]
        source: ManifestError,
    },

    /// The payload does not match the manifest.
    #[error("the package at {path} does not match its manifest")]
    Payload {
        /// Which package.
        path: PathBuf,
        /// Why.
        #[source]
        source: PayloadError,
    },

    /// A declared file is over its limit.
    #[error("`{path}` is {len} bytes, over the {max} byte limit")]
    TooLarge {
        /// Which declared path.
        path: String,
        /// Its size.
        len: u64,
        /// The limit.
        max: u64,
    },

    /// The package as a whole is over [`MAX_PACKAGE_BYTES`].
    #[error("the package is {len} bytes, over the {max} byte limit")]
    PackageTooLarge {
        /// Its total size.
        len: u64,
        /// The limit.
        max: u64,
    },

    /// The entrypoint is not something the tablet could run.
    #[error("the package entrypoint is not a runnable binary")]
    Binary(#[from] BinaryError),

    /// A declared file could not be measured.
    #[error("cannot measure {path}")]
    Io {
        /// Which path.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

fn file_size(path: &Path) -> Result<u64, CheckError> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|source| CheckError::Io {
            path: path.to_path_buf(),
            source,
        })
}
