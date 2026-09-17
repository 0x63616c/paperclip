//! Where the App Store gets its facts, and how it asks for things (§5, §12).
//!
//! # The App Store owns none of this
//!
//! §5 is explicit that the App Store is a *client* of platform facilities and
//! not the owner of them, and that is a property of the code rather than a
//! statement of intent: [`PackagesSource::for_app`] can only be built by
//! handing it an [`InstalledApp`] that host policy granted
//! [`Capability::Packages`], because the only constructor goes through
//! [`PackageManager::on_behalf_of`], which refuses everything else. An App
//! Store that was not granted package management gets a
//! [`SourceError::NotPermitted`] at startup and a screen that says so — it
//! does not get a degraded installer.
//!
//! # Everything here blocks
//!
//! Every method on [`StoreSource`] touches a filesystem or a network. None of
//! them may be called from `App::event` or `App::draw` (§8). The app calls
//! them from [`Context::spawn`](paper_sdk::Context::spawn) and folds the
//! answer back in on [`Event::Completed`](paper_sdk::Event::Completed); the
//! screen itself never performs I/O, which is what makes it testable without
//! a catalog.

use std::fmt;
use std::path::PathBuf;

use paper_packages::catalog::{Catalog, CatalogError, FileTransport};
use paper_packages::install::{
    InstallError, InstallOptions, NothingIsRunning, PackageManager, Progress,
};
use paper_packages::inventory::Inventory;
use paper_packages::launch::Ledger;
use paper_packages::signing::TrustedKeys;
use paper_packages::store::{Layout, StoreError};
use paper_packages::{AppId, Capability, InstallPolicy, InstalledApp};
use semver::Version;

/// What the App Store can ask the platform to do.
///
/// A trait because the answer comes from a different place on a developer's
/// Mac than it will on the tablet once the host carries package operations
/// over the protocol. The screen above it does not change when that lands.
pub trait StoreSource: fmt::Debug {
    /// Everything installed, everything offered, merged.
    fn inventory(&self) -> Result<Inventory, SourceError>;

    /// The release notes for one offered version.
    fn notes(&self, app: &AppId, version: &Version) -> Result<String, SourceError>;

    /// Installs or updates one app, reporting progress as it goes.
    ///
    /// Explicit, always: nothing in this crate installs anything without a
    /// person having pressed something (§12).
    fn install(
        &self,
        app: &AppId,
        version: &Version,
        progress: &dyn Progress,
    ) -> Result<Version, SourceError>;

    /// Selects an app's previous release. Also explicit, and also never
    /// automatic — a failed release is recoverable *from*, not silently
    /// replaced.
    fn rollback(&self, app: &AppId) -> Result<Version, SourceError>;
}

/// The real thing: a store on disk and a catalog directory.
#[derive(Debug)]
pub struct PackagesSource {
    manager: PackageManager,
    layout: Layout,
    catalog: PathBuf,
    keys: TrustedKeys,
}

impl PackagesSource {
    /// Builds a source on behalf of an app that holds [`Capability::Packages`].
    ///
    /// The `caller` is what makes this a client rather than an owner. There is
    /// no constructor that skips it.
    pub fn for_app(
        layout: Layout,
        policy: InstallPolicy,
        catalog: impl Into<PathBuf>,
        keys: TrustedKeys,
        caller: &InstalledApp,
    ) -> Result<Self, SourceError> {
        if !caller.capabilities().holds(Capability::Packages) {
            return Err(SourceError::NotPermitted {
                app: caller.manifest().id().clone(),
            });
        }
        let manager = PackageManager::on_behalf_of(layout.clone(), policy, caller)?;
        Ok(Self {
            manager,
            layout,
            catalog: catalog.into(),
            keys,
        })
    }

    fn catalog(&self) -> Catalog<FileTransport> {
        Catalog::new(FileTransport::new(&self.catalog), self.keys.clone())
            .with_cache(self.layout.clone())
    }
}

impl StoreSource for PackagesSource {
    fn inventory(&self) -> Result<Inventory, SourceError> {
        // A catalog that cannot be reached is not an error here: the rows for
        // what is installed are still worth drawing, and `Inventory` marks
        // itself stale so the screen can say so (§12).
        let view = self.catalog().view().ok();
        Ok(Inventory::survey(
            &self.layout,
            view.as_ref(),
            &Ledger::new(self.layout.clone()),
        )?)
    }

    fn notes(&self, app: &AppId, version: &Version) -> Result<String, SourceError> {
        let catalog = self.catalog();
        let view = catalog.view()?;
        Ok(Inventory::notes(&catalog, &view, app, version)?)
    }

    fn install(
        &self,
        app: &AppId,
        version: &Version,
        progress: &dyn Progress,
    ) -> Result<Version, SourceError> {
        let catalog = self.catalog();
        let view = catalog.view()?;
        let entry = view
            .index
            .exact(app, version)
            .ok_or_else(|| SourceError::NotOffered {
                app: app.clone(),
                version: version.clone(),
            })?;
        let release = catalog.release(entry)?;
        let archive = catalog.open_archive(entry, &release)?;

        // `NothingIsRunning` only because nothing in this process can know.
        // The host supplies the real guard once package operations cross the
        // protocol; until then the refusal that matters — an update never
        // replacing a running app — is the host's to make, and this is the
        // line to change when it can.
        let installed = self.manager.install(
            &release,
            archive,
            &InstallOptions::default(),
            &NothingIsRunning,
            progress,
        )?;
        Ok(installed.version)
    }

    fn rollback(&self, app: &AppId) -> Result<Version, SourceError> {
        Ok(self.manager.rollback(app, &NothingIsRunning)?.version)
    }
}

/// Why the App Store could not do something.
///
/// Every variant carries [`Self::advice`]: §6 asks for *actionable* errors,
/// and "the catalog could not be read" is a fact rather than an action. The
/// message says what happened; the advice says what the person holding the
/// tablet can do about it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// This App Store build was not granted package management.
    #[error("`{app}` has not been granted package management")]
    NotPermitted {
        /// Which app asked.
        app: AppId,
    },

    /// The catalog no longer offers the version that was on screen.
    #[error("the catalog no longer offers `{app}` {version}")]
    NotOffered {
        /// Which app.
        app: AppId,
        /// Which version.
        version: Version,
    },

    /// The catalog could not be read or believed.
    #[error("{0}")]
    Catalog(#[from] CatalogError),

    /// An install or rollback failed.
    #[error("{0}")]
    Install(#[from] InstallError),

    /// The store could not be read or written.
    #[error("{0}")]
    Store(#[from] StoreError),
}

impl SourceError {
    /// What the person can actually do about it.
    pub fn advice(&self) -> &'static str {
        match self {
            SourceError::NotPermitted { .. } => {
                "Grant this app `packages` in host policy and reinstall the platform."
            }
            SourceError::NotOffered { .. } => "Refresh the list and try again.",
            SourceError::Catalog(CatalogError::Transport(_)) => {
                "The catalog is not reachable. Check the home network; installed apps keep working."
            }
            SourceError::Catalog(CatalogError::Signature(_)) => {
                "This was not signed by a key this tablet trusts. Do not install it."
            }
            SourceError::Catalog(CatalogError::Rollback { .. })
            | SourceError::Catalog(CatalogError::StalledSerial { .. }) => {
                "The catalog is older than one already seen. Republish it before installing."
            }
            SourceError::Catalog(_) => "Re-publish the catalog and refresh.",
            SourceError::Install(InstallError::Running { .. }) => {
                "Close the app first, then install the update."
            }
            SourceError::Install(InstallError::Digest { .. })
            | SourceError::Install(InstallError::Size { .. }) => {
                "The download did not match what was signed. Try again; nothing was changed."
            }
            SourceError::Install(InstallError::Downgrade { .. }) => {
                "That version is older than the one installed. Use roll back instead."
            }
            SourceError::Install(InstallError::NoFallback { .. }) => {
                "There is no previous release to roll back to."
            }
            SourceError::Install(InstallError::Io { .. }) | SourceError::Store(_) => {
                "There may be no room left. Free some space and try again."
            }
            SourceError::Install(_) => "Nothing was changed. The previous release is still in use.",
        }
    }
}
