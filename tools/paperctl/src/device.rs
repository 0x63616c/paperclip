//! The device-facing commands: isolation, units, session, and stock.
//!
//! `paperctl stock` is the one that matters most. It is the independent
//! recovery path §10 requires: it does not talk to the host, does not need
//! Home or the App Store, opens no Paperclip socket, and is also what
//! `paperclip-restore-stock.service` runs when systemd notices the supervisor
//! died. Over SSH it is a second way in, not the only one.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use paper_host::facilities::Facilities;
use paper_host::report::IsolationReport;
use paper_host::units::{SessionGrants, SessionSpec, UnitSet};
use paper_packages::{AppId, GrantedCapabilities, InstallPolicy, InstalledApp, Manifest};

use crate::error::CommandError;

/// Which facilities to describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Target {
    /// Probe the machine this command is running on.
    Here,
    /// The reMarkable Paper Pro as WWW-1 and WWW-11 recorded it.
    ///
    /// A transcription of someone else's measurement, and the output says so
    /// on every line that matters.
    PaperPro,
}

impl Target {
    fn facilities(self) -> Result<Facilities, CommandError> {
        match self {
            Target::PaperPro => Ok(Facilities::paper_pro()),
            Target::Here => paper_host::probe::probe().map_err(CommandError::Probe),
        }
    }
}

/// `paperctl isolation`.
#[derive(Debug, Args)]
pub(crate) struct IsolationArgs {
    /// Whose facilities to report on.
    #[arg(long, value_enum, default_value_t = Target::Here)]
    target: Target,
    /// Emit Markdown rather than the terminal table.
    #[arg(long)]
    markdown: bool,
}

/// `paperctl units`.
#[derive(Debug, Args)]
pub(crate) struct UnitsArgs {
    /// Whose facilities decide which directives are written.
    #[arg(long, value_enum, default_value_t = Target::Here)]
    target: Target,
    /// The app the session unit is generated for.
    #[arg(long, default_value = "dev.calum.chess")]
    app: String,
    /// Write the units into this directory instead of printing them.
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

/// `paperctl stock`.
#[derive(Debug, Args)]
pub(crate) struct StockArgs {
    /// Set when started by `paperclip-restore-stock.service`, which only
    /// changes how loudly it reports.
    #[arg(long)]
    from_unit: bool,
    /// The session state directory, which is also where the config lives.
    #[arg(long)]
    state: Option<PathBuf>,
    /// Retry even after the supervisor has recorded a terminal failure.
    #[arg(long)]
    force: bool,
    #[command(flatten)]
    device: crate::transport::DeviceArgs,
}

#[cfg(not(target_os = "linux"))]
impl crate::transport::dispatch::RemoteCommand for StockArgs {
    /// `--from-unit` never forwards: it only means something to the copy
    /// `paperclip-restore-stock.service` itself starts.
    fn remote_argv(&self) -> Vec<String> {
        let mut argv = vec!["stock".to_owned()];
        if let Some(state) = &self.state {
            argv.push("--state".to_owned());
            argv.push(state.display().to_string());
        }
        if self.force {
            argv.push("--force".to_owned());
        }
        argv
    }

    fn shape(&self) -> crate::transport::dispatch::RemoteShape {
        crate::transport::dispatch::RemoteShape::Blocking
    }
}

/// `paperctl autostart`.
///
/// The escape hatch WWW-53 asks for: it must work over SSH without the
/// tablet's own UI, so it goes through the same remote dispatch as `stock`
/// rather than depending on anything Paperclip itself renders.
#[derive(Debug, Args)]
pub(crate) struct AutostartArgs {
    #[command(subcommand)]
    command: AutostartCommand,
    #[command(flatten)]
    device: crate::transport::DeviceArgs,
}

/// Boot-time autostart (WWW-53, §10).
#[derive(Debug, Subcommand)]
pub(crate) enum AutostartCommand {
    /// Start Paperclip at boot. The default, once the persistent launcher
    /// unit is installed on the device's root filesystem.
    Enable,
    /// Boot straight to stock, every boot, until re-enabled. Sets the exact
    /// marker `paperclip-launcher.service`'s `ConditionPathExists=!` checks
    /// — the boot-time skip and this command are the same mechanism.
    Disable {
        /// Recorded alongside the marker, for `status` and for the next
        /// person following `docs/recovery.md`'s manual-recovery steps.
        #[arg(long, default_value = "operator request")]
        reason: String,
    },
    /// Whether autostart is enabled, and the durable boot-attempt count.
    Status,
    /// Clear the durable boot-attempt counter and re-enable autostart.
    ///
    /// For after a fix has landed — a new release, or `paperctl upgrade
    /// rollback` — on a device that safe-moded itself and needs to be told
    /// it may try again. Not automatic: §10's "a spent failure budget
    /// refuses requests" applies to a boot budget the same way it applies to
    /// the supervisor's session budget, and only a human clears either.
    Reset,
}

#[cfg(not(target_os = "linux"))]
impl crate::transport::dispatch::RemoteCommand for AutostartArgs {
    fn remote_argv(&self) -> Vec<String> {
        let mut argv = vec!["autostart".to_owned()];
        match &self.command {
            AutostartCommand::Enable => argv.push("enable".to_owned()),
            AutostartCommand::Disable { reason } => {
                argv.push("disable".to_owned());
                argv.push("--reason".to_owned());
                argv.push(reason.clone());
            }
            AutostartCommand::Status => argv.push("status".to_owned()),
            AutostartCommand::Reset => argv.push("reset".to_owned()),
        }
        argv
    }

    fn shape(&self) -> crate::transport::dispatch::RemoteShape {
        crate::transport::dispatch::RemoteShape::Blocking
    }
}

/// The device-facing subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum DeviceCommand {
    /// Report what the isolation actually enforces (§11).
    Isolation(IsolationArgs),
    /// Show the runtime units a session would be given.
    Units(UnitsArgs),
    /// Return the display to stock. The independent recovery path (§10).
    Stock(StockArgs),
    /// Enable, disable or inspect boot-time autostart (§10, WWW-53).
    Autostart(AutostartArgs),
}

pub(crate) fn run(command: DeviceCommand) -> Result<(), CommandError> {
    match command {
        DeviceCommand::Isolation(args) => isolation(&args),
        DeviceCommand::Units(args) => units(&args),
        DeviceCommand::Stock(args) => stock(&args),
        DeviceCommand::Autostart(args) => autostart(&args),
    }
}

fn isolation(args: &IsolationArgs) -> Result<(), CommandError> {
    let facilities = args.target.facilities()?;
    let report = IsolationReport::derive(&facilities);
    if args.markdown {
        print!("{}", report.to_markdown());
    } else {
        print!("{report}");
        println!();
        println!("Not claimed as a protection:");
        for finding in report.unclaimable() {
            println!("  {} — {}", finding.mechanism.title(), finding.caveat);
        }
    }
    Ok(())
}

fn units(args: &UnitsArgs) -> Result<(), CommandError> {
    let facilities = args.target.facilities()?;
    let spec = SessionSpec::device();
    let app: AppId = args
        .app
        .parse()
        .map_err(|source| CommandError::AppId { source })?;
    let granted = granted_for(&app);
    let grants = SessionGrants::derive(&app, &granted, &spec.paths);
    let set = UnitSet::plan(&spec, &grants, &facilities);

    match &args.out_dir {
        None => {
            for file in &set.files {
                println!("# ---- {} ----", file.name);
                print!("{}", file.contents);
                println!();
            }
        }
        Some(directory) => {
            std::fs::create_dir_all(directory).map_err(|source| CommandError::Write {
                path: directory.clone(),
                source,
            })?;
            for file in &set.files {
                let path = directory.join(&file.name);
                std::fs::write(&path, &file.contents).map_err(|source| CommandError::Write {
                    path: path.clone(),
                    source,
                })?;
                println!("{}", path.display());
            }
        }
    }
    Ok(())
}

/// The capabilities a bare app id holds, with nothing granted.
///
/// `paperctl units` is a preview, and previewing the *least* privileged shape
/// is the right default: an operator who wants to see what a storage grant
/// adds can install the app and look at the real unit.
fn granted_for(app: &AppId) -> GrantedCapabilities {
    let manifest = Manifest::parse(&format!(
        "[app]\nid = \"{app}\"\nname = \"Preview\"\nversion = \"0.0.0\"\n\
         protocol = \"1.0\"\nentrypoint = \"bin/preview\"\n"
    ))
    .expect("a manifest built from a validated app id is always valid");
    InstalledApp::install(manifest, &InstallPolicy::deny_all())
        .capabilities()
        .clone()
}

/// On a Mac, this is a forward to the tablet's own `paperctl stock` over SSH
/// (WWW-33) — the independent recovery path still runs on the device; this
/// side only reaches it.
#[cfg(not(target_os = "linux"))]
fn stock(args: &StockArgs) -> Result<(), CommandError> {
    crate::transport::dispatch::dispatch(args.device.as_deref(), args)
}

#[cfg(target_os = "linux")]
fn stock(args: &StockArgs) -> Result<(), CommandError> {
    use paper_host::linux::recovery::StockRecovery;
    use paper_host::linux::runtime::RuntimeConfig;

    let config = match &args.state {
        Some(state) => {
            let path = state.join("config");
            let mut config =
                RuntimeConfig::load_or_device(&path).map_err(|source| CommandError::Read {
                    path: path.clone(),
                    source,
                })?;
            config.paths.state = state.clone();
            config
        }
        None => RuntimeConfig::device(),
    };

    let recovery = StockRecovery::new(config.recovery());
    match recovery.restore() {
        Ok(outcome) => {
            println!("{}", outcome.summary());
            if args.force {
                // A forced restore is a human saying "try again anyway". Clear
                // the terminal marker so the supervisor may be started again;
                // leave the diagnostics exactly where they are.
                let _ = std::fs::remove_file(config.paths.state.join("halted"));
            }
            Ok(())
        }
        Err(error) => {
            // The §10 row that matters most: preserved logs, independent
            // diagnosis, and no claim that recovery worked.
            let where_to_look = recovery.capture(&error.to_string());
            Err(CommandError::StockUnavailable {
                detail: error.to_string(),
                diagnostics: where_to_look.display().to_string(),
            })
        }
    }
}

/// On a Mac, this is a forward to the tablet's own `paperctl autostart` over
/// SSH — the escape hatch must never depend on anything Paperclip itself
/// renders, so it reaches the device the same way `stock` does.
#[cfg(not(target_os = "linux"))]
fn autostart(args: &AutostartArgs) -> Result<(), CommandError> {
    crate::transport::dispatch::dispatch(args.device.as_deref(), args)
}

#[cfg(target_os = "linux")]
fn autostart(args: &AutostartArgs) -> Result<(), CommandError> {
    let root = paper_host::units::SessionPaths::device().root;
    match &args.command {
        AutostartCommand::Enable => {
            paper_boot::autostart::enable(&root)?;
            println!("autostart enabled");
        }
        AutostartCommand::Disable { reason } => {
            paper_boot::autostart::disable(&root, reason)?;
            println!("autostart disabled: {reason}");
        }
        AutostartCommand::Status => {
            match paper_boot::autostart::reason(&root)? {
                Some(reason) => println!("autostart: disabled ({reason})"),
                None => println!("autostart: enabled"),
            }
            let counter = paper_boot::BootCounter::read(&root)?;
            println!(
                "boot attempts since the last time Home was reached: {} of {}",
                counter.attempts,
                paper_boot::MAX_BOOT_ATTEMPTS
            );
        }
        AutostartCommand::Reset => {
            paper_boot::counter::mark_good(&root)?;
            paper_boot::autostart::enable(&root)?;
            println!("boot counter cleared and autostart enabled");
        }
    }
    Ok(())
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;
    use crate::transport::dispatch::RemoteCommand as _;

    #[test]
    fn remote_argv_carries_state_and_force_but_never_from_unit() {
        let args = StockArgs {
            from_unit: true,
            state: Some(PathBuf::from("/run/paperclip")),
            force: true,
            device: crate::transport::DeviceArgs::default(),
        };

        assert_eq!(
            args.remote_argv(),
            vec!["stock", "--state", "/run/paperclip", "--force"]
        );
    }

    #[test]
    fn remote_argv_omits_state_when_not_given() {
        let args = StockArgs {
            from_unit: false,
            state: None,
            force: false,
            device: crate::transport::DeviceArgs::default(),
        };

        assert_eq!(args.remote_argv(), vec!["stock"]);
    }
}
