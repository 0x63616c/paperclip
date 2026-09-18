//! `paperctl upgrade` and `paperctl remove` — replacing Paperclip, and taking
//! it off (§13, §14).
//!
//! # Why these live in `paperctl` and not in a second binary
//!
//! §13 requires the thing that performs an update to be independent of the
//! host being replaced. `paperctl` already is: it sits at
//! `/home/root/paperclip/bin/paperctl`, outside `releases/`, because it is the
//! recovery bootstrap that `paperclip-restore-stock.service` runs when the
//! supervisor has died. A platform update replaces everything under
//! `releases/` and cannot reach it.
//!
//! So the requirement is met by placement, and a second binary in the same
//! directory would meet it no better while being a second thing to build,
//! ship and keep in step. Replacing `paperctl` itself is the one operation
//! that cannot be an ordinary update, and it has its own subcommand with its
//! own fallback — [`bootstrap`].
//!
//! # The word "updater"
//!
//! Does not appear in any of this. The verb is `upgrade`, and later a button
//! that says "Update Paperclip".

#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;

use clap::{Args, Subcommand};
#[cfg(target_os = "linux")]
use paper_packages::signing::{PublicKey, TrustedKeys};
use paper_packages::store::Layout;
use paper_updater::layout::PlatformLayout;
use paper_updater::remove::{AppData, plan};
#[cfg(target_os = "linux")]
use paper_updater::upgrade::Outcome;
use paper_updater::upgrade::{Maintenance, Reconciled, Status};

use crate::error::CommandError;

/// Where Paperclip is installed. Shared by every command here.
#[derive(Debug, Args)]
pub(crate) struct RootArgs {
    /// The platform root. Defaults to the device path.
    #[arg(long, default_value = "/home/root/paperclip")]
    root: PathBuf,
}

impl RootArgs {
    fn layout(&self) -> PlatformLayout {
        PlatformLayout::new(&self.root)
    }
}

/// `paperctl upgrade`.
#[derive(Debug, Args)]
pub(crate) struct UpgradeArgs {
    #[command(subcommand)]
    command: UpgradeCommand,
    /// The tablet to reach, overriding auto-discovery and any pin. Only
    /// `run` and `rollback` reach the device; the rest are answered locally.
    #[command(flatten)]
    device: crate::transport::DeviceArgs,
}

/// Replacing the platform.
#[derive(Debug, Subcommand)]
pub(crate) enum UpgradeCommand {
    /// Install a platform bundle, verify it came up, and roll back if it did
    /// not.
    Run(RunArgs),
    /// Show what is selected, what the fallback is, and what the last
    /// transaction did.
    Status(StatusArgs),
    /// Go back to the previous release.
    Rollback(RollbackArgs),
    /// Finish or undo whatever an interrupted upgrade left behind.
    Reconcile(StatusArgs),
    /// Replace `paperctl` itself. The one operation an ordinary upgrade cannot
    /// do.
    Bootstrap(BootstrapArgs),
    /// Build and sign a platform bundle. Publishing machines only.
    #[cfg(feature = "publishing")]
    Package(PackageArgs),
}

/// Build a platform bundle.
///
/// Behind `publishing` for the reason every signing command is: the device
/// build must not contain a code path that holds a secret key (§12).
#[cfg(feature = "publishing")]
#[derive(Debug, Args)]
pub(crate) struct PackageArgs {
    /// A directory holding `bin/paperclip-host`, `bin/home`, `bin/app-store`
    /// and `bin/settings`.
    #[arg(long)]
    pub(crate) source: PathBuf,
    /// `release.toml`, which declares the version, protocol and state
    /// numbers a plain `package` invocation builds (WWW-61, ADR-0026).
    #[arg(long, default_value = crate::release_manifest::RELEASE_MANIFEST_FILE_NAME)]
    pub(crate) release_manifest: PathBuf,
    /// The release version. Overrides `release.toml` when given.
    #[arg(long)]
    pub(crate) version: Option<semver::Version>,
    /// The protocol this platform speaks. Overrides `release.toml` when
    /// given.
    #[arg(long)]
    pub(crate) protocol: Option<String>,
    /// The persistent state version this release writes. Overrides
    /// `release.toml` when given.
    #[arg(long)]
    pub(crate) state_version: Option<u32>,
    /// The lowest state version that can still read what this release writes.
    /// Overrides `release.toml` when given.
    ///
    /// Leave it equal to `--state-version` (or, in `release.toml`, to
    /// `writes`) only when the change is *not* backward compatible: that is
    /// what makes the updater take a snapshot before it activates, so a
    /// rollback is a rollback rather than a swap over bytes the older
    /// release cannot parse.
    #[arg(long)]
    pub(crate) rollback_to_state: Option<u32>,
    /// Release notes.
    #[arg(long, default_value = "")]
    pub(crate) notes: String,
    /// An extra file to ship inside the release, relative to `--source`.
    /// Repeatable. Covered by the manifest's digests like everything else.
    #[arg(long = "include")]
    pub(crate) include: Vec<String>,
    /// The secret key to sign with.
    #[arg(long)]
    pub(crate) key: PathBuf,
    /// Where to write the bundle.
    #[arg(long)]
    pub(crate) out: PathBuf,
}

/// Install a platform bundle.
#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    /// The bundle.
    bundle: PathBuf,
    /// A public key file to trust. Repeatable.
    #[arg(long = "trust", required = true)]
    trust: Vec<PathBuf>,
    /// Where the supervisor's runtime state is.
    #[arg(long, default_value = "/run/paperclip")]
    state: PathBuf,
    #[command(flatten)]
    root: RootArgs,
}

#[cfg(not(target_os = "linux"))]
impl crate::transport::dispatch::RemoteCommand for RunArgs {
    /// The bundle and trust paths forward verbatim; see the module doc on
    /// why nothing is staged across the link.
    fn remote_argv(&self) -> Vec<String> {
        let mut argv = vec![
            "upgrade".to_owned(),
            "run".to_owned(),
            self.bundle.display().to_string(),
        ];
        for key in &self.trust {
            argv.push("--trust".to_owned());
            argv.push(key.display().to_string());
        }
        argv.push("--state".to_owned());
        argv.push(self.state.display().to_string());
        argv.push("--root".to_owned());
        argv.push(self.root.root.display().to_string());
        argv
    }

    fn shape(&self) -> crate::transport::dispatch::RemoteShape {
        crate::transport::dispatch::RemoteShape::Blocking
    }
}

/// Show what is installed.
#[derive(Debug, Args)]
pub(crate) struct StatusArgs {
    #[command(flatten)]
    root: RootArgs,
}

/// Go back one release.
#[derive(Debug, Args)]
pub(crate) struct RollbackArgs {
    /// A public key file to trust. Repeatable.
    #[arg(long = "trust", required = true)]
    trust: Vec<PathBuf>,
    /// Where the supervisor's runtime state is.
    #[arg(long, default_value = "/run/paperclip")]
    state: PathBuf,
    #[command(flatten)]
    root: RootArgs,
}

#[cfg(not(target_os = "linux"))]
impl crate::transport::dispatch::RemoteCommand for RollbackArgs {
    fn remote_argv(&self) -> Vec<String> {
        let mut argv = vec!["upgrade".to_owned(), "rollback".to_owned()];
        for key in &self.trust {
            argv.push("--trust".to_owned());
            argv.push(key.display().to_string());
        }
        argv.push("--state".to_owned());
        argv.push(self.state.display().to_string());
        argv.push("--root".to_owned());
        argv.push(self.root.root.display().to_string());
        argv
    }

    fn shape(&self) -> crate::transport::dispatch::RemoteShape {
        crate::transport::dispatch::RemoteShape::Blocking
    }
}

/// Replace the bootstrap.
#[derive(Debug, Args)]
pub(crate) struct BootstrapArgs {
    /// The new `paperctl`.
    replacement: PathBuf,
    #[command(flatten)]
    root: RootArgs,
}

/// Take Paperclip off the device.
#[derive(Debug, Args)]
pub(crate) struct RemoveArgs {
    /// Delete app data too — saved games, settings. Off by default (§14).
    #[arg(long)]
    remove_app_data: bool,
    /// Actually do it. Without this the plan is printed and nothing is
    /// touched.
    #[arg(long)]
    yes: bool,
    /// The app store root.
    #[arg(long)]
    store: Option<PathBuf>,
    #[command(flatten)]
    root: RootArgs,
    #[command(flatten)]
    device: crate::transport::DeviceArgs,
}

#[cfg(not(target_os = "linux"))]
impl crate::transport::dispatch::RemoteCommand for RemoveArgs {
    fn remote_argv(&self) -> Vec<String> {
        let mut argv = vec!["remove".to_owned()];
        if self.remove_app_data {
            argv.push("--remove-app-data".to_owned());
        }
        if self.yes {
            argv.push("--yes".to_owned());
        }
        if let Some(store) = &self.store {
            argv.push("--store".to_owned());
            argv.push(store.display().to_string());
        }
        argv.push("--root".to_owned());
        argv.push(self.root.root.display().to_string());
        argv
    }

    fn shape(&self) -> crate::transport::dispatch::RemoteShape {
        crate::transport::dispatch::RemoteShape::Blocking
    }
}

/// Runs an `upgrade` subcommand.
///
/// # Errors
///
/// Whatever the transaction failed with, a refusal on a machine where the
/// session cannot be controlled, or — on a Mac running `run`/`rollback` — no
/// tablet the transport could resolve or reach.
pub(crate) fn run(args: UpgradeArgs) -> Result<(), CommandError> {
    #[cfg(not(target_os = "linux"))]
    let device = args.device.as_deref();
    match args.command {
        #[cfg(not(target_os = "linux"))]
        UpgradeCommand::Run(run_args) => upgrade(&run_args, device),
        #[cfg(target_os = "linux")]
        UpgradeCommand::Run(run_args) => upgrade(&run_args),
        UpgradeCommand::Status(status_args) => status(&status_args),
        #[cfg(not(target_os = "linux"))]
        UpgradeCommand::Rollback(rollback_args) => rollback(&rollback_args, device),
        #[cfg(target_os = "linux")]
        UpgradeCommand::Rollback(rollback_args) => rollback(&rollback_args),
        UpgradeCommand::Reconcile(status_args) => reconcile(&status_args),
        UpgradeCommand::Bootstrap(bootstrap_args) => bootstrap(&bootstrap_args),
        #[cfg(feature = "publishing")]
        UpgradeCommand::Package(package_args) => package(&package_args),
    }
}

/// Builds and signs a platform bundle.
#[cfg(feature = "publishing")]
pub(crate) fn package(args: &PackageArgs) -> Result<(), CommandError> {
    use paper_packages::signing::{Domain, SecretKey};
    use paper_packages::store;
    use paper_updater::bundle;

    let declared = crate::release_manifest::DeclaredRelease::read(&args.release_manifest)?;
    let version = args.version.clone().unwrap_or(declared.version);
    let protocol_str = args.protocol.clone().unwrap_or(declared.protocol);
    let state_version = args.state_version.unwrap_or(declared.state_writes);
    let rollback_to_state = args
        .rollback_to_state
        .unwrap_or(declared.state_readable_back_to);

    let protocol = protocol_str
        .parse::<paper_protocol::ProtocolVersion>()
        .map_err(|source| CommandError::Protocol {
            value: protocol_str,
            source,
        })?;
    let secret: SecretKey = crate::install::read_text(&args.key)?.parse()?;
    let extras: Vec<&str> = args.include.iter().map(String::as_str).collect();
    let manifest = bundle::describe(
        &args.source,
        &bundle::Description {
            version,
            protocol,
            published: store::now(),
            state_version,
            rollback_to_state,
            notes: args.notes.clone(),
            extras: &extras,
        },
    )?;

    // Rendered once, signed, and shipped. Never re-rendered on the way in.
    let document = manifest.to_document();
    let signature = secret.sign(Domain::PLATFORM, document.as_bytes());
    let file = std::fs::File::create(&args.out).map_err(|source| CommandError::Write {
        path: args.out.clone(),
        source,
    })?;
    bundle::build(
        &args.source,
        &manifest,
        document.as_bytes(),
        &signature,
        file,
    )?;

    println!("platform   {}", manifest.version());
    println!("protocol   {}", manifest.protocol());
    println!(
        "state      writes v{}, readable back to v{}",
        manifest.state_version(),
        manifest.rollback_to_state()
    );
    for component in manifest.components() {
        println!(
            "component  {:<16} {:>9} bytes  {}",
            component.name(),
            component.size(),
            component.digest()
        );
    }
    println!("bundle     {}", args.out.display());
    println!("signed by  {}", secret.public_key().id());
    Ok(())
}

fn status(args: &StatusArgs) -> Result<(), CommandError> {
    let layout = args.root.layout();
    print_status(&Maintenance::new(&layout).status()?);
    Ok(())
}

fn print_status(status: &Status) {
    println!(
        "current   {}",
        status
            .current
            .as_ref()
            .map_or_else(|| "none".to_owned(), ToString::to_string)
    );
    println!(
        "previous  {}",
        status
            .previous
            .as_ref()
            .map_or_else(|| "none".to_owned(), ToString::to_string)
    );
    let installed: Vec<String> = status.installed.iter().map(ToString::to_string).collect();
    println!(
        "installed {}",
        if installed.is_empty() {
            "none".to_owned()
        } else {
            installed.join(", ")
        }
    );
    match &status.last {
        Some(record) => println!("last      {}", record.summary()),
        None => println!("last      nothing has been upgraded here yet"),
    }
}

fn reconcile(args: &StatusArgs) -> Result<(), CommandError> {
    let layout = args.root.layout();
    match Maintenance::new(&layout).reconcile()? {
        Reconciled::Nothing => println!("nothing was interrupted"),
        Reconciled::DiscardedStaging { candidate } => {
            println!("discarded a staged {candidate} that was never activated");
        }
        Reconciled::Reverted {
            candidate,
            restored,
            state_restored,
        } => {
            let restored = restored.map_or_else(|| "nothing".to_owned(), |v| v.to_string());
            println!("{candidate} never committed; {restored} is selected again");
            if state_restored {
                println!("platform state was restored from the snapshot taken before activation");
            }
        }
    }
    Ok(())
}

/// Replaces `paperctl` itself, keeping the outgoing one beside it.
///
/// §13: updating the bootstrap is a separate operation with its own recovery
/// plan, and this is the plan — the outgoing binary is *renamed*, not
/// overwritten, so it keeps working for anything already running it and stays
/// on disk under `paperctl.previous` for a person to move back by hand over
/// SSH. Overwriting in place would fail with `ETXTBSY` against a running
/// process anyway, and succeed against a stopped one in a way that leaves no
/// way back.
fn bootstrap(args: &BootstrapArgs) -> Result<(), CommandError> {
    let layout = args.root.layout();
    layout.ensure().map_err(CommandError::from)?;
    let live = layout.bin_dir().join("paperctl");
    let kept = layout.bin_dir().join("paperctl.previous");

    let replacement = std::fs::read(&args.replacement).map_err(|source| CommandError::Read {
        path: args.replacement.clone(),
        source,
    })?;
    if live.exists() {
        let _ = std::fs::remove_file(&kept);
        std::fs::rename(&live, &kept).map_err(|source| CommandError::Write {
            path: kept.clone(),
            source,
        })?;
    }
    paper_packages::store::atomic_write(&live, &replacement).map_err(CommandError::from)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&live, std::fs::Permissions::from_mode(0o755)).map_err(
            |source| CommandError::Write {
                path: live.clone(),
                source,
            },
        )?;
    }
    println!("replaced {}", live.display());
    if kept.exists() {
        println!(
            "the previous one is at {} if this one is wrong",
            kept.display()
        );
    }
    Ok(())
}

/// Prints what removing Paperclip would do, and does it when asked.
///
/// # Errors
///
/// A root that is not a Paperclip root, or a filesystem failure part-way
/// through.
pub(crate) fn remove(args: &RemoveArgs) -> Result<(), CommandError> {
    #[cfg(not(target_os = "linux"))]
    if let Some(explicit) = args.device.as_deref() {
        return crate::transport::dispatch::dispatch(Some(explicit), args);
    }

    let platform = args.root.layout();
    let store = match &args.store {
        Some(root) => Layout::new(root),
        None => Layout::from_environment(),
    };
    platform.separate_from(&store)?;

    let data = if args.remove_app_data {
        AppData::Remove
    } else {
        AppData::Keep
    };
    let removal = plan(&platform, &store, data);
    print!("{}", removal.describe());

    if !args.yes {
        println!(
            "\nNothing was removed. Add --yes to do it.\n\
             Return the display to stock first with `paperctl stock`; this command \n\
             deletes files and does not touch the running session."
        );
        return Ok(());
    }
    let removed = removal.execute()?;
    println!("\nremoved {} paths", removed.removed.len());
    println!("left {} paths alone", removed.kept.len());
    Ok(())
}

/// Reads the public keys a bundle must be signed by.
///
/// Only the Linux half needs it: `status`, `reconcile`, `bootstrap` and
/// `remove` verify nothing, because none of them chooses a release.
#[cfg(target_os = "linux")]
fn trusted(paths: &[PathBuf]) -> Result<TrustedKeys, CommandError> {
    let mut keys = TrustedKeys::none();
    for path in paths {
        let armoured = std::fs::read_to_string(path).map_err(|source| CommandError::Read {
            path: path.clone(),
            source,
        })?;
        keys.trust(armoured.trim().parse::<PublicKey>()?);
    }
    Ok(keys)
}

/// On a Mac, a forward to the tablet's own `paperctl upgrade run` over SSH
/// (WWW-33) — `--bundle` and every `--trust` path are read on the tablet,
/// exactly as if typed there directly; nothing is staged across the link.
#[cfg(not(target_os = "linux"))]
fn upgrade(args: &RunArgs, device: Option<&str>) -> Result<(), CommandError> {
    crate::transport::dispatch::dispatch(device, args)
}

/// The Mac-side half of `upgrade rollback`; see [`upgrade`].
#[cfg(not(target_os = "linux"))]
fn rollback(args: &RollbackArgs, device: Option<&str>) -> Result<(), CommandError> {
    crate::transport::dispatch::dispatch(device, args)
}

#[cfg(target_os = "linux")]
fn upgrade(args: &RunArgs) -> Result<(), CommandError> {
    let layout = args.root.layout();
    let keys = trusted(&args.trust)?;
    let session = session(&args.state, &layout)?;
    let clock = paper_updater::SystemClock;
    let transaction = paper_updater::Upgrade::new(&layout, &session, &clock, &keys);

    report_outcome(&transaction.run(&args.bundle)?);
    Ok(())
}

#[cfg(target_os = "linux")]
fn rollback(args: &RollbackArgs) -> Result<(), CommandError> {
    let layout = args.root.layout();
    let keys = trusted(&args.trust)?;
    let session = session(&args.state, &layout)?;
    let clock = paper_updater::SystemClock;
    let transaction = paper_updater::Upgrade::new(&layout, &session, &clock, &keys);

    report_outcome(&transaction.rollback()?);
    Ok(())
}

/// Builds the session control from the supervisor's own configuration, so the
/// two cannot disagree about which unit is stock or where the status file is.
#[cfg(target_os = "linux")]
fn session(
    state: &Path,
    _layout: &PlatformLayout,
) -> Result<paper_updater::linux::SystemdSession, CommandError> {
    use paper_host::linux::runtime::RuntimeConfig;

    let config_path = state.join("config");
    let config = if config_path.exists() {
        RuntimeConfig::load(&config_path).map_err(|source| CommandError::Read {
            path: config_path,
            source,
        })?
    } else {
        let mut config = RuntimeConfig::device();
        config.paths.state = state.to_path_buf();
        config
    };
    Ok(paper_updater::linux::SystemdSession::new(
        config.recovery(),
        config.paths.clone(),
    ))
}

/// Prints what happened, in the words a person needs.
#[cfg(target_os = "linux")]
fn report_outcome(outcome: &Outcome) {
    match outcome {
        Outcome::Upgraded { from, to, health } => {
            let from = from
                .as_ref()
                .map_or_else(|| "nothing".to_owned(), ToString::to_string);
            println!("Paperclip {from} -> {to}: {}", health.summary());
        }
        Outcome::RolledBack {
            candidate,
            restored,
            failure,
            health,
        } => {
            println!("{candidate} was refused: {failure}");
            println!("{restored} is back: {}", health.summary());
        }
        Outcome::Stranded {
            candidate,
            failure,
            fallback,
        } => {
            println!("{candidate} was refused: {failure}");
            match fallback {
                Some(why) => println!("the previous release did not come back either: {why}"),
                None => println!("there was no previous release to go back to"),
            }
            println!(
                "The tablet is at stock and reachable over SSH. Nothing will be retried \n\
                 automatically; `paperctl upgrade status` says where things stand."
            );
        }
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;
    use crate::transport::dispatch::RemoteCommand as _;

    #[test]
    fn run_args_remote_argv_forwards_bundle_and_trust_verbatim() {
        let args = RunArgs {
            bundle: PathBuf::from("/home/root/staging/platform-0.4.0.paperpkg"),
            trust: vec![PathBuf::from("/home/root/paperclip/keys/platform.pub")],
            state: PathBuf::from("/run/paperclip"),
            root: RootArgs {
                root: PathBuf::from("/home/root/paperclip"),
            },
        };

        assert_eq!(
            args.remote_argv(),
            vec![
                "upgrade",
                "run",
                "/home/root/staging/platform-0.4.0.paperpkg",
                "--trust",
                "/home/root/paperclip/keys/platform.pub",
                "--state",
                "/run/paperclip",
                "--root",
                "/home/root/paperclip",
            ]
        );
    }

    #[test]
    fn remove_args_remote_argv_carries_yes_and_remove_app_data() {
        let args = RemoveArgs {
            remove_app_data: true,
            yes: true,
            store: None,
            root: RootArgs {
                root: PathBuf::from("/home/root/paperclip"),
            },
            device: crate::transport::DeviceArgs::default(),
        };

        assert_eq!(
            args.remote_argv(),
            vec![
                "remove",
                "--remove-app-data",
                "--yes",
                "--root",
                "/home/root/paperclip",
            ]
        );
    }
}
