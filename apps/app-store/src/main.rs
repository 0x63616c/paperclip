//! The App Store's standalone entrypoint (§8, ADR-0022, §16).
//!
//! Hands a real, unmodified [`AppStoreApp`] to [`paper_sdk::run`] over this
//! process's own stdin/stdout — the same lifecycle `open_app_store_session`
//! gives it in-process in `paperctl`, now on the far side of an actual
//! process boundary. This is `bin/app-store`, the file
//! `apps/app-store/paper.toml` names as the entrypoint.
//!
//! # Granting itself `packages`
//!
//! [`PackagesSource::for_app`]'s own doc is explicit that the App Store
//! cannot grant itself anything — the constructor only accepts an
//! [`InstalledApp`] that *host* policy already granted [`Capability::Packages`].
//! There is no host process yet to do that granting from the outside
//! (WWW-4's supervisor), so — exactly as
//! `tools/paperctl/src/session.rs`'s `app_store_source` already does for an
//! in-process session — [`own_source`] plays that part: it builds the policy,
//! grants this app's own manifest `packages`, and only then calls
//! `for_app` with it. That stand-in belongs here, in the binary that launches
//! the app, and deliberately not as a convenience constructor on
//! [`PackagesSource`] itself, which would let the library quietly grant
//! capabilities no host asked it to.
//!
//! `NothingIsRunning` is the same stand-in `paperctl` uses for
//! `ActivationGuard`: nothing surveys running apps until there is a
//! supervisor to ask.

use std::io;
use std::process::ExitCode;
use std::sync::Arc;

use paper_app_store::{AppStoreApp, AppStoreScreen, PackagesSource, SourceError, StoreSource};
use paper_packages::install::NothingIsRunning;
use paper_packages::signing::TrustedKeys;
use paper_packages::store::Layout;
use paper_packages::{Capability, InstallPolicy, InstalledApp, Manifest, ManifestError};
use paper_sdk::LocalSurfaces;

const MANIFEST: &str = include_str!("../paper.toml");

/// Why this entrypoint could not start.
#[derive(Debug, thiserror::Error)]
enum StartupError {
    /// This binary's own compiled-in manifest is invalid — a bug in this
    /// build, not in anything the environment did.
    #[error("this build's own manifest is invalid: {0}")]
    Manifest(#[from] ManifestError),
    /// The package source could not be built.
    #[error(transparent)]
    Source(#[from] SourceError),
}

fn main() -> ExitCode {
    let (screen, source) = match start() {
        Ok(built) => built,
        Err(error) => {
            eprintln!("paper-app-store: {error}");
            return ExitCode::FAILURE;
        }
    };
    let app = AppStoreApp::new(screen, Arc::new(source));
    let outcome = paper_sdk::run(app, io::stdin(), io::stdout(), LocalSurfaces::new());
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paper-app-store: {error}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<(AppStoreScreen, PackagesSource), StartupError> {
    let manifest = Manifest::parse(MANIFEST)?;
    manifest.ensure_runnable()?;
    let source = own_source(&manifest)?;
    let screen = AppStoreScreen::new(source.inventory()?);
    Ok((screen, source))
}

/// Grants this process's own manifest [`Capability::Packages`] and points it
/// at the layout's own `catalog` directory. See the module doc.
fn own_source(manifest: &Manifest) -> Result<PackagesSource, SourceError> {
    let mut policy = InstallPolicy::deny_all();
    policy.allow(manifest.id(), Capability::Packages);
    let caller = InstalledApp::install(manifest.clone(), &policy);

    let layout = Layout::from_environment();
    layout.ensure()?;
    let catalog = layout.root().join("catalog");
    PackagesSource::for_app(
        layout,
        policy,
        catalog,
        TrustedKeys::none(),
        &caller,
        Box::new(NothingIsRunning),
    )
}
