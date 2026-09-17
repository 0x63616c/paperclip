//! One error type for the whole transaction.
//!
//! §7 asks for contextual errors a person can act on. The thing a person is
//! acting on here is almost always a tablet they cannot see the screen of, so
//! every variant names the path, the version or the rung involved rather than
//! describing a category of failure.

use std::path::PathBuf;

use paper_host::readiness::Rung;
use semver::Version;

/// Why an update could not be done, or could not be finished.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateError {
    /// A filesystem operation failed.
    #[error("{path}")]
    Io {
        /// What was being read or written.
        path: PathBuf,
        /// The underlying failure.
        source: std::io::Error,
    },

    /// A durability primitive from the package store failed.
    #[error("store")]
    Store(#[from] paper_packages::store::StoreError),

    /// The bundle could not be unpacked.
    #[error("unpacking the bundle")]
    Archive(#[from] paper_packages::archive::ArchiveError),

    /// The manifest's signature did not verify.
    #[error("the platform manifest is not signed by a trusted key")]
    Signature(#[from] paper_packages::signing::SignatureError),

    /// The manifest verified but does not say what it must.
    #[error("the platform manifest is not usable: {reason}")]
    Manifest {
        /// What is wrong with it.
        reason: String,
    },

    /// A component's bytes are not what the manifest said they would be.
    #[error("component `{name}` does not match the signed manifest: {reason}")]
    Component {
        /// The component.
        name: String,
        /// Size, digest or machine.
        reason: String,
    },

    /// `current` or `previous` points at something that is not a release.
    #[error("{link} points at `{target}`, which is not a release directory")]
    CorruptSelection {
        /// The symlink.
        link: PathBuf,
        /// What it pointed at.
        target: PathBuf,
    },

    /// A selection was asked for a version that is not on disk.
    #[error("release {version} is not installed")]
    NoSuchRelease {
        /// The version asked for.
        version: Version,
    },

    /// The bundle offers a version that is already selected.
    #[error("release {version} is already selected")]
    AlreadySelected {
        /// The version.
        version: Version,
    },

    /// An update is already in flight and has not been reconciled.
    #[error("an update to {version} is in flight ({phase}); run `paperctl upgrade reconcile`")]
    InFlight {
        /// Where it got to.
        phase: &'static str,
        /// What it was going to.
        version: Version,
    },

    /// The platform root and the app store root overlap.
    #[error(
        "the platform root {platform} and the app store root {apps} overlap; \
         an app install could then reach the host"
    )]
    OverlappingRoots {
        /// The platform root.
        platform: PathBuf,
        /// The app store root.
        apps: PathBuf,
    },

    /// Rolling back would leave the older release reading state it cannot
    /// read, and no snapshot could be taken.
    #[error(
        "release {to} writes platform state version {writes}, which release {from} \
         (state version {reads}) cannot read, and the state could not be snapshotted: {reason}"
    )]
    StateNotRollbackSafe {
        /// The outgoing release.
        from: Version,
        /// The incoming release.
        to: Version,
        /// The lowest state version the incoming release's writes stay
        /// readable by.
        writes: u32,
        /// The outgoing release's state version.
        reads: u32,
        /// Why the snapshot could not be taken.
        reason: String,
    },

    /// Standing the session down did not end with stock owning the display.
    #[error("the session did not stand down: {reason}")]
    StandDown {
        /// What was observed instead.
        reason: String,
    },

    /// The wakelock is still held by a session that no longer exists.
    #[error("the wakelock is still held after the session stood down; refusing to activate")]
    WakelockStuck,

    /// Bringing the session up failed outright, before any rung.
    #[error("the session could not be started: {reason}")]
    BringUp {
        /// What systemd said.
        reason: String,
    },

    /// The journal on disk is not something this build wrote.
    #[error("{path} is not a readable update journal: {reason}")]
    CorruptJournal {
        /// The journal.
        path: PathBuf,
        /// Why.
        reason: String,
    },

    /// Removal was asked for a directory that is not a Paperclip root.
    #[error("{path} is not a Paperclip root; refusing to remove anything from it")]
    NotAPaperclipRoot {
        /// What was named.
        path: PathBuf,
    },
}

impl UpdateError {
    /// An [`UpdateError::Io`] for `path`.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Why a candidate release was refused after it was started.
///
/// Distinct from [`UpdateError`] on purpose: these are not failures of the
/// update, they are the update working. A release that stalls at
/// `device-adapter` and is rolled back is a transaction that did its job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unhealthy {
    /// The highest rung reached.
    pub reached: Rung,
    /// The rung it never got past.
    pub stalled_at: Rung,
    /// What the release itself said about why, if anything.
    pub note: String,
    /// Whether the supervisor process was gone by the time the deadline ran
    /// out.
    pub died: bool,
}

impl std::fmt::Display for Unhealthy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "reached `{}`, never reached `{}`",
            self.reached, self.stalled_at
        )?;
        if self.died {
            formatter.write_str("; the supervisor exited")?;
        }
        if !self.note.is_empty() {
            write!(formatter, " ({})", self.note)?;
        }
        Ok(())
    }
}
