//! `paperctl install`, `list`, `rollback`, `recover` — the installing side (§12).
//!
//! These run against a store: on the Mac for development, on the tablet over a
//! session. Which store is `--root`, or `PAPERCLIP_ROOT`, or the device path.
//! No command here holds a key that can sign anything; all of them verify.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use clap::Args;
use paper_packages::catalog::{Catalog, FileTransport};
use paper_packages::install::{InstallOptions, NothingIsRunning, PackageManager, Progress, Step};
use paper_packages::inventory::Inventory;
use paper_packages::launch::Ledger;
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
    #[command(flatten)]
    device: crate::transport::DeviceArgs,
}

impl InstallArgs {
    /// The argv a remote `paperctl install` on the tablet should be given —
    /// exactly what was typed, so `--catalog` and `--trust` are read as
    /// paths on the tablet, same as if this had been typed there directly.
    #[cfg(not(target_os = "linux"))]
    fn remote_argv(&self) -> Vec<String> {
        let mut argv = vec![
            "install".to_owned(),
            self.app.clone(),
            "--catalog".to_owned(),
            self.catalog.display().to_string(),
        ];
        for key in &self.trust {
            argv.push("--trust".to_owned());
            argv.push(key.display().to_string());
        }
        if self.no_activate {
            argv.push("--no-activate".to_owned());
        }
        if self.no_prune {
            argv.push("--no-prune".to_owned());
        }
        if let Some(root) = &self.store.root {
            argv.push("--root".to_owned());
            argv.push(root.display().to_string());
        }
        argv
    }
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

/// Prints install progress on one rewritten line.
///
/// The App Store will draw the same [`Step`]s on the panel; this is the same
/// information for someone at a terminal, and it exists mostly so the reporting
/// has a second consumer and cannot quietly stop working.
#[derive(Debug, Default)]
struct Printer;

impl Progress for Printer {
    fn step(&self, step: Step) {
        let line = match step {
            Step::Downloading { done, total } if total > 0 => {
                format!("downloading {done}/{total} bytes ({}%)", done * 100 / total)
            }
            Step::Downloading { done, .. } => format!("downloading {done} bytes"),
            Step::Verifying => "verifying size, digest and structure".to_owned(),
            Step::Extracting => "extracting".to_owned(),
            Step::Committing => "committing".to_owned(),
            Step::Activating => "selecting".to_owned(),
        };
        // Rewrite one line on a terminal; print plain lines when stderr is
        // redirected, because a log full of escape sequences is worse than a
        // log with six lines in it.
        if std::io::stderr().is_terminal() {
            eprint!("\r\u{1b}[K  {line}");
        } else {
            eprintln!("  {line}");
        }
    }
}

/// Runs `paperctl install` — against `--root`/`--store` right here by
/// default (a Mac's own scratch store is a normal thing to install into for
/// development), or on the tablet over SSH when `--device` says so (WWW-33).
/// Unlike `open`/`stock`/`setup`, this never auto-discovers: a bare
/// `paperctl install` with no `--device` has always meant "install here",
/// and that stays true.
pub(crate) fn install(args: &InstallArgs) -> Result<(), CommandError> {
    #[cfg(not(target_os = "linux"))]
    if let Some(explicit) = args.device.as_deref() {
        let (host, source) = crate::transport::remote::resolve_device(Some(explicit))?;
        println!("device   {host} ({source})");
        crate::transport::remote::run_blocking(&host, &args.remote_argv())?;
        return Ok(());
    }

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
    let installed = manager.install(&release, archive, &options, &NothingIsRunning, &Printer)?;
    if std::io::stderr().is_terminal() {
        eprintln!();
    }

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
///
/// Built on the same [`Inventory`] the App Store will render, so the two
/// cannot disagree about what is installed or what an update is.
pub(crate) fn list(args: &ListArgs) -> Result<(), CommandError> {
    let layout = args.store.layout();
    println!("root       {}", layout.root().display());

    let view = match &args.catalog {
        Some(directory) => {
            let catalog = Catalog::new(FileTransport::new(directory), trusted(&args.trust)?)
                .with_cache(layout.clone());
            Some(catalog.view()?)
        }
        None => None,
    };

    let inventory = Inventory::survey(&layout, view.as_ref(), &Ledger::new(layout.clone()))?;

    if let Some(status) = inventory.catalog() {
        println!(
            "catalog    {} serial {}{}",
            status.name,
            status.serial,
            if status.stale {
                " (unreachable; showing the last index seen)"
            } else {
                ""
            }
        );
    }

    if inventory.entries().is_empty() {
        println!("apps       none installed, none offered");
        return Ok(());
    }

    for entry in inventory.entries() {
        println!(
            "{:<10} {} \u{2014} {}",
            entry.state.label(),
            entry.app,
            entry.name
        );
        println!(
            "           installed {}   available {}",
            entry
                .installed
                .as_ref()
                .map_or_else(|| "none".to_owned(), Version::to_string),
            entry
                .available
                .as_ref()
                .map_or_else(|| "none".to_owned(), Version::to_string)
        );
        if let Some(prerelease) = &entry.available_prerelease {
            println!("           prerelease {prerelease} (install it by name)");
        }
        if let Some(fallback) = &entry.fallback
            && entry.can_roll_back()
        {
            println!("           fallback {fallback}");
        }
        if let Some(health) = &entry.health
            && !health.started
            && health.attempts > 0
        {
            println!(
                "           {} attempt(s) without a start{}",
                health.attempts,
                health
                    .last_failure
                    .as_deref()
                    .map_or_else(String::new, |note| format!(": {note}"))
            );
        }
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
    if report.locks_broken > 0 {
        println!(
            "cleared    {} lock(s) left by a killed process",
            report.locks_broken
        );
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

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn remote_argv_forwards_catalog_trust_and_flags_verbatim() {
        let args = InstallArgs {
            app: "dev.calum.chess".to_owned(),
            catalog: PathBuf::from("/home/root/catalogs/home"),
            trust: vec![PathBuf::from("/home/root/paperclip/keys/home.pub")],
            no_activate: true,
            no_prune: false,
            store: StoreArgs { root: None },
            device: crate::transport::DeviceArgs::default(),
        };

        assert_eq!(
            args.remote_argv(),
            vec![
                "install",
                "dev.calum.chess",
                "--catalog",
                "/home/root/catalogs/home",
                "--trust",
                "/home/root/paperclip/keys/home.pub",
                "--no-activate",
            ]
        );
    }
}
