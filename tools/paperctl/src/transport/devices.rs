//! `paperctl devices` — list what auto-discovery found, and pin one.
//!
//! Config lives at `~/.config/paperctl/config.toml` (`$PAPERCTL_CONFIG_DIR`
//! or `$XDG_CONFIG_HOME/paperctl` override it; `Config::default_path` is the
//! one place that decides, so this doc comment cannot drift from it).

use clap::{Args, Subcommand};

use crate::error::CommandError;
use crate::transport::OutputFormat;
use crate::transport::config::Config;
use crate::transport::discover::{self, PROBE_TIMEOUT, SystemProber};

/// `paperctl devices`. The doc comment that actually reaches `--help` is on
/// the `Command::Devices` variant in `main.rs` — clap derive takes a
/// subcommand's about from the enum variant, not from the wrapped `Args`
/// struct, so this one is for readers of the source.
#[derive(Debug, Args)]
pub(crate) struct DevicesArgs {
    #[command(subcommand)]
    command: Option<DevicesCommand>,
    /// Table for a terminal, or one JSON array.
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
}

#[derive(Debug, Subcommand)]
pub(crate) enum DevicesCommand {
    /// Pin a device, skipping auto-discovery until it is unpinned.
    Pin {
        /// The tablet, as `ssh` would take it.
        host: String,
    },
    /// Clear the pin. Auto-discovery resumes on the next resolution.
    Unpin,
}

pub(crate) fn run(args: &DevicesArgs) -> Result<(), CommandError> {
    match &args.command {
        None => list(args.output),
        Some(DevicesCommand::Pin { host }) => pin(host),
        Some(DevicesCommand::Unpin) => unpin(),
    }
}

fn list(output: OutputFormat) -> Result<(), CommandError> {
    let config = Config::load(&crate::transport::config::default_path())?;
    let inputs = discover::Inputs {
        flag: None,
        env: crate::transport::device_env(),
        pinned: config.pinned().map(str::to_owned),
        usb: Some(discover::USB_HOST.to_owned()),
        cache: config.cached().map(str::to_owned),
    };
    let found = discover::discover_all(&inputs, &SystemProber, PROBE_TIMEOUT);

    match output {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&found).map_err(CommandError::Json)?
            );
        }
        OutputFormat::Table => {
            if found.is_empty() {
                println!("no devices found; see `paperctl devices --help` for how they are found");
                return Ok(());
            }
            println!("{:<24} {:<12} {:<10} PINNED", "HOST", "SOURCE", "REACHABLE");
            for device in &found {
                println!(
                    "{:<24} {:<12} {:<10} {}",
                    device.host, device.source, device.reachable, device.pinned
                );
            }
        }
    }
    Ok(())
}

fn pin(host: &str) -> Result<(), CommandError> {
    let mut config = Config::load(&crate::transport::config::default_path())?;
    config.pin(host)?;
    println!("pinned {host}");
    Ok(())
}

fn unpin() -> Result<(), CommandError> {
    let mut config = Config::load(&crate::transport::config::default_path())?;
    config.unpin()?;
    println!("unpinned");
    Ok(())
}
