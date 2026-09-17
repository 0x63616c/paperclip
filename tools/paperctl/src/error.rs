//! What `paperctl` can fail with.

use std::io;
use std::path::PathBuf;

use paper_packages::{ManifestError, PayloadError};
use paper_sdk::desktop::PreviewError;

/// A command failure, phrased for someone at a terminal.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum CommandError {
    /// A canvas could not be allocated.
    #[error("cannot allocate a {width}x{height} canvas")]
    Canvas {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },

    /// The preview window failed.
    #[error("the desktop preview could not run")]
    Preview(#[from] PreviewError),

    /// A manifest was rejected.
    #[error("{path} is not a valid manifest")]
    Manifest {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: ManifestError,
    },

    /// A package payload did not match its manifest.
    #[error("the package at {path} does not match its manifest")]
    Payload {
        /// Which package directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: PayloadError,
    },

    /// A built-in manifest is broken, which is a bug in this build rather than
    /// anything the user did.
    #[error("the built-in manifest for `{app}` is invalid; this is a bug in paperclip")]
    BuiltInManifest {
        /// Which app's manifest.
        app: &'static str,
        /// Why.
        #[source]
        source: ManifestError,
    },

    /// A file could not be written.
    #[error("cannot write {path}")]
    Write {
        /// Which file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },
}
