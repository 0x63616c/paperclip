//! `paperctl devices` — list what auto-discovery found, and pin one.
//!
//! Config lives at `~/.config/paperctl/config.toml` (`$PAPERCTL_CONFIG_DIR`
//! or `$XDG_CONFIG_HOME/paperctl` override it; `Config::default_path` is the
//! one place that decides, so this doc comment cannot drift from it).

use clap::{Args, Subcommand};

use crate::error::CommandError;
use crate::transport::OutputFormat;
use crate::transport::config::Config;
use crate::transport::discover::{self, LIST_PROBE_TIMEOUT, PROBE_TIMEOUT, SystemProber};

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
    /// Spend the full wake budget on each candidate, so a *sleeping* tablet
    /// has time to answer. Slower by design: listing otherwise gives a
    /// sleeping tablet 4s and reports it unreachable.
    #[arg(long)]
    wake: bool,
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
        None => list(args.output, args.wake),
        Some(DevicesCommand::Pin { host }) => pin(host),
        Some(DevicesCommand::Unpin) => unpin(),
    }
}

/// How long each candidate's probe may take. Listing is not reaching, so the
/// default is the cheap bound; `--wake` buys the full budget back for the one
/// case that needs it. See [`discover::LIST_PROBE_TIMEOUT`].
fn probe_budget(wake: bool) -> std::time::Duration {
    if wake {
        PROBE_TIMEOUT
    } else {
        LIST_PROBE_TIMEOUT
    }
}

fn list(output: OutputFormat, wake: bool) -> Result<(), CommandError> {
    let config = Config::load(&crate::transport::config::default_path())?;
    let inputs = discover::Inputs {
        // `paperctl devices` is unconditional, but the `--device` flag it would
        // carry is Mac-only, so the field does not exist on a device build.
        #[cfg(not(target_os = "linux"))]
        flag: None,
        env: crate::transport::device_env(),
        pinned: config.pinned().map(str::to_owned),
        usb: Some(discover::USB_HOST.to_owned()),
        cache: config.cached().map(str::to_owned),
    };
    let budget = probe_budget(wake);
    // Every candidate is probed in turn, so this is a per-candidate bound, not
    // a total. Said up front and on stderr: the command used to print nothing
    // at all until it finished, which is what made a slow answer look like a
    // hang. stderr so a `--output json` consumer's stdout stays clean.
    eprintln!(
        "probing up to {}s per candidate{}...",
        budget.as_secs(),
        if wake {
            ""
        } else {
            ", --wake for sleeping tablets"
        }
    );
    let found = discover::discover_all(&inputs, &SystemProber, budget);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_does_not_spend_the_wake_budget_by_default() {
        assert_eq!(probe_budget(false), LIST_PROBE_TIMEOUT);
        assert!(
            probe_budget(false) < PROBE_TIMEOUT,
            "the default must be cheaper than the wake budget, or `devices` is \
             back to costing 30s per absent candidate"
        );
    }

    #[test]
    fn wake_buys_the_full_budget_back() {
        assert_eq!(probe_budget(true), PROBE_TIMEOUT);
    }
}
