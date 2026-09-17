//! What `paperctl` can fail with.

use std::io;
use std::path::PathBuf;

use paper_packages::{ManifestError, PayloadError};

/// A command failure, phrased for someone at a terminal.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum CommandError {
    /// A canvas could not be allocated.
    #[cfg(feature = "apps")]
    #[error("cannot allocate a {width}x{height} canvas")]
    Canvas {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },

    /// The preview window failed.
    #[cfg(feature = "desktop")]
    #[error("the desktop preview could not run")]
    Preview(#[from] paper_sdk::desktop::PreviewError),

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
    #[cfg(feature = "apps")]
    #[error("the built-in manifest for `{app}` is invalid; this is a bug in paperclip")]
    BuiltInManifest {
        /// Which app's manifest.
        app: &'static str,
        /// Why.
        #[source]
        source: ManifestError,
    },

    /// An app id on the command line was not one.
    #[error("that is not a valid app id")]
    AppId {
        /// Why.
        #[source]
        source: paper_packages::IdError,
    },

    /// The isolation facilities could not be probed.
    #[error("cannot read what this machine enforces")]
    Probe(#[from] paper_host::probe::ProbeError),

    /// A command that only means something on the device was run elsewhere.
    #[cfg(not(target_os = "linux"))]
    #[error("{what} only works on the device or in the Linux VM harness")]
    NotOnDevice {
        /// What was attempted.
        what: &'static str,
    },

    /// Stock could not be restored. Never softened: §10 forbids claiming a
    /// recovery that did not happen.
    #[cfg(target_os = "linux")]
    #[error(
        "stock Xochitl was NOT restored: {detail}\n\
         the display has not been handed back; diagnostics are in {diagnostics}"
    )]
    StockUnavailable {
        /// What went wrong.
        detail: String,
        /// Where the evidence was written.
        diagnostics: String,
    },

    /// A file could not be read.
    #[cfg(target_os = "linux")]
    #[error("cannot read {path}")]
    Read {
        /// Which file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
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
