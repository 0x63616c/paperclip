//! What `paperctl` can fail with.

use std::io;
use std::path::PathBuf;

#[cfg(feature = "publishing")]
use paper_packages::CheckError;
use paper_packages::archive::ArchiveError;
use paper_packages::catalog::CatalogError;
use paper_packages::install::InstallError;
use paper_packages::signing::SignatureError;
use paper_packages::store::StoreError;
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

    /// `paperctl dev` could not run a session.
    #[cfg(feature = "desktop")]
    #[error("the dev loop failed")]
    Dev(#[from] crate::dev::DevError),

    /// A manifest was rejected.
    #[error("{path} is not a valid manifest")]
    Manifest {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: ManifestError,
    },

    /// A package source directory did not pass the package-time checks.
    ///
    /// Boxed because `CheckError` is the largest thing this enum can hold, and
    /// every `Result<(), CommandError>` in the binary would otherwise be that
    /// big on the success path too.
    ///
    /// Publishing-only: `paperctl check` is a Mac-side command, and the device
    /// build has no code path that can produce this.
    #[cfg(feature = "publishing")]
    #[error("the package source at {path} did not pass `paperctl check`")]
    PackageSource {
        /// Which package directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: Box<CheckError>,
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

    /// The device adapter refused, or the panel did.
    #[error("the display session did not complete")]
    Device(#[source] paper_device::DeviceError),

    /// A build that cannot reach the glass was asked to present.
    ///
    /// Its own variant rather than a generic refusal because the remedy is a
    /// build flag, and because the alternative — stopping Xochitl to draw into
    /// a `MemoryPanel` — would look like a successful session in every log.
    #[cfg(target_os = "linux")]
    #[error(
        "this paperctl was built without the vendor waveform engine, so it cannot reach the panel\n\
         build it with `--features vendor-engine` for aarch64, or pass --dry-run"
    )]
    NoVendorEngine,

    /// The display came back, and stock did not come back the same.
    ///
    /// Distinct from [`Self::StockUnavailable`]: there the tablet may still be
    /// without a UI, here it has one and something else is wrong — most
    /// importantly a `NRestarts` that moved, which means Xochitl crashed and
    /// the tablet is closer to an emergency shell than it was.
    #[cfg(target_os = "linux")]
    #[error(
        "the display was handed back but stock did not come back as it was found: {regressions}\n\
         check `systemctl show xochitl.service -p NRestarts -p ActiveState` before taking it again"
    )]
    StockRegressed {
        /// What differed, from `StockHealth::regressions_from`.
        regressions: String,
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
    #[error("cannot read {path}")]
    Read {
        /// Which file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },

    /// A package archive could not be built or opened.
    #[error("the package could not be handled")]
    Archive(#[from] ArchiveError),

    /// A catalog could not be published to or checked.
    #[cfg(feature = "publishing")]
    #[error("the catalog operation failed")]
    Publish(#[from] paper_packages::publish::PublishError),

    /// A catalog could not be read or believed.
    #[error("the catalog could not be read")]
    Catalog(#[from] CatalogError),

    /// An install, rollback or recovery failed.
    #[error("the package operation failed")]
    Install(#[from] InstallError),

    /// A key or signature was not usable.
    #[error("the key or signature is not usable")]
    Signature(#[from] SignatureError),

    /// The package store could not be read or written.
    #[error("the package store could not be used")]
    Store(#[from] StoreError),

    /// `<app-id>` or `<app-id>@<version>`, and this was neither.
    #[error("`{value}` is not an app id or `<app-id>@<version>`")]
    AppSpec {
        /// What was typed.
        value: String,
    },

    /// The catalog does not offer the app that was asked for.
    #[error("catalog `{catalog}` does not offer `{app}`")]
    NotOffered {
        /// Which app.
        app: String,
        /// Which catalog.
        catalog: String,
    },

    /// Checking a catalog needs a key to check it against.
    #[cfg(feature = "publishing")]
    #[error("checking a catalog needs `--trust <public key>`")]
    TrustRequired,

    /// A signing key is already there.
    #[cfg(feature = "publishing")]
    #[error(
        "{path} already exists. Overwriting a signing key makes every release \
         already published unverifiable; pass --force only if that is what you mean."
    )]
    KeyExists {
        /// Which file.
        path: PathBuf,
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

    /// `paperctl setup` found a prerequisite missing.
    ///
    /// No detail here on purpose: the stage table above it already named every
    /// one, and repeating the first in an error line is how the others get
    /// missed.
    #[error("setup did not complete; a prerequisite above is missing")]
    SetupIncomplete,

    /// A platform upgrade or removal failed.
    #[error("the platform operation did not complete")]
    Platform(#[from] paper_updater::UpdateError),

    /// A protocol version on the command line did not parse.
    #[cfg(feature = "publishing")]
    #[error("`{value}` is not a protocol version")]
    Protocol {
        /// What was typed.
        value: String,
        /// Why it is not one.
        #[source]
        source: paper_protocol::ParseError,
    },
}
