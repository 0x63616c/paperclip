//! `paperctl install`, `list`, `rollback`, `recover` — the installing side (§12).
//!
//! These run against a store: on the Mac for development, on the tablet over a
//! session. Which store is `--root`, or `PAPERCLIP_ROOT`, or the device path.
//! No command here holds a key that can sign anything; all of them verify.

use std::path::{Path, PathBuf};

use clap::Args;
use paper_packages::catalog::{Catalog, FileTransport};
use paper_packages::install::{InstallOptions, NothingIsRunning, PackageManager};
use paper_packages::signing::{PublicKey, TrustedKeys};
use paper_packages::store::Layout;
use paper_packages::{AppId, InstallPolicy};
use semver::Version;

use crate::error::CommandError;

/// Where the store is and who is trusted — shared by every command here.
#[derive(Debug, Args)]
pub(crate) struct StoreArgs {
    /// The Paperclip root. Defaults to `PAPERCLIP_ROOT`, then the device path.
    #[arg(long, global = true)]
    root: Option<PathBuf>,
}

impl StoreArgs {
    fn layout(&self) -> Layout {
        match &self.root {
            Some(root) => Layout::new(root),
            None => Layout::from_environment(),
        }
    }
}

/// Install or update an app from a catalog.
#[derive(Debug, Args)]
pub(crate) struct InstallArgs {
    /// The app id, optionally `@<version>`.
    ///
    /// Without a version, the newest *stable* release is installed. A
    /// prerelease is never chosen for you — name it if you want it.
    app: String,
    /// The catalog directory.
    #[arg(long)]
    catalog: PathBuf,
    /// A public key file to trust. Repeatable.
    #[arg(long = "trust", required = true)]
    trust: Vec<PathBuf>,
    /// Commit the release without selecting it.
    #[arg(long)]
    no_activate: bool,
    /// Keep releases that are neither selected nor the fallback.
    #[arg(long)]
    no_prune: bool,
    #[command(flatten)]
    store: StoreArgs,
}

/// Show what is installed.
#[derive(Debug, Args)]
pub(crate) struct ListArgs {
    /// Also list what the catalog offers.
    #[arg(long)]
    catalog: Option<PathBuf>,
    /// A public key file to trust when reading a catalog. Repeatable.
    #[arg(long = "trust")]
    trust: Vec<PathBuf>,
    #[command(flatten)]
    store: StoreArgs,
}

/// Select the previous release of an app.
#[derive(Debug, Args)]
pub(crate) struct RollbackArgs {
    /// The app id.
    app: String,
    #[command(flatten)]
    store: StoreArgs,
}

/// Finish or undo whatever an interrupted install left behind.
#[derive(Debug, Args)]
pub(crate) struct RecoverArgs {
    #[command(flatten)]
    store: StoreArgs,
}

/// Runs `paperctl install`.
pub(crate) fn install(args: &InstallArgs) -> Result<(), CommandError> {
    let (id, wanted) = split_app(&args.app)?;
    let layout = args.store.layout();
    let catalog = Catalog::new(FileTransport::new(&args.catalog), trusted(&args.trust)?)
        .with_cache(layout.clone());

    let view = catalog.view()?;
    if view.stale {
        println!("catalog    unreachable; showing the last index seen");
    }
    let entry = match &wanted {
        Some(version) => view.index.exact(&id, version),
        None => view.index.newest_stable(&id),
    }
    .ok_or_else(|| CommandError::NotOffered {
        app: id.to_string(),
        catalog: view.index.catalog().to_owned(),
    })?;

    let release = catalog.release(entry)?;
    let archive = catalog.open_archive(entry, &release)?;

    let mut options = InstallOptions::default();
    options.activate = !args.no_activate;
    options.prune = !args.no_prune;

    // `NothingIsRunning`: `paperctl` drives a store from outside the running
    // system. When the host gains a supervisor (WWW-4) it supplies the real
    // answer; claiming to know one here would be inventing it.
    let manager = PackageManager::host(layout, InstallPolicy::deny_all());
    let installed = manager.install(&release, archive, &options, &NothingIsRunning)?;

    println!(
        "installed  {} {}{}",
        installed.app,
        installed.version,
        if installed.already_present {
            " (already installed, unchanged)"
        } else {
            ""
        }
    );
    println!("signed by  {}", release.signer());
    println!(
        "selected   {}",
        if installed.activated { "yes" } else { "no" }
    );
    if let Some(previous) = &installed.previous {
        println!("fallback   {previous}");
    }
    if !installed.pruned.is_empty() {
        println!(
            "pruned     {}",
            installed
                .pruned
                .iter()
                .map(Version::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!(
        "granted    {}",
        if installed.capabilities.is_empty() {
            "nothing".to_owned()
        } else {
            installed
                .capabilities
                .iter()
                .map(|c| c.name().to_owned())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    Ok(())
}

/// Runs `paperctl list`.
pub(crate) fn list(args: &ListArgs) -> Result<(), CommandError> {
    let layout = args.store.layout();
    println!("root       {}", layout.root().display());

    let apps = layout.apps()?;
    if apps.is_empty() {
        println!("installed  nothing");
    }
    for app in &apps {
        let current = layout.current(app)?;
        let previous = layout.previous(app)?;
        let versions = layout.installed_versions(app)?;
        println!(
            "installed  {app} {} (fallback {}; on disk {})",
            current.map_or_else(|| "none selected".to_owned(), |v| v.to_string()),
            previous.map_or_else(|| "none".to_owned(), |v| v.to_string()),
            versions
                .iter()
                .map(Version::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let Some(directory) = &args.catalog else {
        return Ok(());
    };
    let catalog =
        Catalog::new(FileTransport::new(directory), trusted(&args.trust)?).with_cache(layout);
    let view = catalog.view()?;
    println!(
        "catalog    {} serial {}{}",
        view.index.catalog(),
        view.index.serial(),
        if view.stale { " (stale; cached)" } else { "" }
    );
    for entry in view.index.entries() {
        println!(
            "available  {} {} \u{2014} {}",
            entry.app(),
            entry.version(),
            entry.name()
        );
    }
    Ok(())
}

/// Runs `paperctl rollback`.
pub(crate) fn rollback(args: &RollbackArgs) -> Result<(), CommandError> {
    let (id, _) = split_app(&args.app)?;
    let manager = PackageManager::host(args.store.layout(), InstallPolicy::deny_all());
    let rolled = manager.rollback(&id, &NothingIsRunning)?;
    println!(
        "rolled back {} from {} to {}",
        rolled.app, rolled.from, rolled.version
    );
    println!("fallback   {} (roll back again to undo this)", rolled.from);
    Ok(())
}

/// Runs `paperctl recover`.
pub(crate) fn recover(args: &RecoverArgs) -> Result<(), CommandError> {
    let manager = PackageManager::host(args.store.layout(), InstallPolicy::deny_all());
    let report = manager.recover()?;
    if report.is_clean() {
        println!("recover    nothing to do");
        return Ok(());
    }
    for (app, version) in &report.completed {
        println!("completed  {app} -> {version}");
    }
    for (app, version) in &report.reverted {
        println!(
            "reverted   {app} -> {}",
            version
                .as_ref()
                .map_or_else(|| "nothing selected".to_owned(), Version::to_string)
        );
    }
    if report.staging_removed > 0 {
        println!("cleaned    {} staging directories", report.staging_removed);
    }
    for (app, version) in &report.incomplete_removed {
        println!("removed    incomplete {app} {version}");
    }
    Ok(())
}

/// Splits `dev.calum.chess@0.2.0` into its two halves.
fn split_app(spec: &str) -> Result<(AppId, Option<Version>), CommandError> {
    let (id, version) = match spec.split_once('@') {
        Some((id, version)) => (id, Some(version)),
        None => (spec, None),
    };
    let id: AppId = id.parse().map_err(|_| CommandError::AppSpec {
        value: spec.to_owned(),
    })?;
    let version = version
        .map(Version::parse)
        .transpose()
        .map_err(|_| CommandError::AppSpec {
            value: spec.to_owned(),
        })?;
    Ok((id, version))
}

/// Reads a small text file — a key, a signature, a notes file.
pub(crate) fn read_text(path: &Path) -> Result<String, CommandError> {
    std::fs::read_to_string(path).map_err(|source| CommandError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn trusted(paths: &[PathBuf]) -> Result<TrustedKeys, CommandError> {
    let mut keys = TrustedKeys::none();
    for path in paths {
        keys.trust(read_text(path)?.parse::<PublicKey>()?);
    }
    Ok(keys)
}
