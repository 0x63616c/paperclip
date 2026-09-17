//! `paperctl` — the one entry point into Paperclip from a developer's machine (§4).
//!
//! Stage 1 gave it three jobs: open the desktop preview, capture screenshots
//! at target resolution, and validate a `paper.toml`. Stage 4 adds the three
//! that matter on the device: `isolation` says what a platform really
//! enforces, `units` shows what a session would be given, and `stock` is the
//! independent recovery path — the command a person runs over SSH when the
//! screen is wrong, and the command `paperclip-restore-stock.service` runs
//! when nobody is there to.

mod device;
mod error;
mod manifest;
#[cfg(feature = "desktop")]
mod preview;
#[cfg(feature = "apps")]
mod screens;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::error::CommandError;
#[cfg(feature = "apps")]
use crate::screens::Screen;

/// The Paperclip command line.
#[derive(Debug, Parser)]
#[command(name = "paperctl", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open the desktop preview window.
    #[cfg(feature = "desktop")]
    Preview(preview::PreviewArgs),
    /// Render screens to PNG files without opening a window.
    #[cfg(feature = "apps")]
    Screenshot(ScreenshotArgs),
    /// Validate a `paper.toml` manifest.
    Manifest {
        #[command(subcommand)]
        command: manifest::ManifestCommand,
    },
    /// Report what the isolation actually enforces (§11).
    Isolation(device::IsolationArgs),
    /// Show the runtime units a session would be given.
    Units(device::UnitsArgs),
    /// Return the display to stock — the independent recovery path (§10).
    Stock(device::StockArgs),
}

/// Which screen to draw.
#[cfg(feature = "apps")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ScreenArg {
    /// The home screen.
    Home,
    /// The Chess screen.
    Chess,
    /// Both, one file each.
    All,
}

#[cfg(feature = "apps")]
impl ScreenArg {
    fn screens(self) -> Vec<Screen> {
        match self {
            ScreenArg::Home => vec![Screen::Home],
            ScreenArg::Chess => vec![Screen::Chess],
            ScreenArg::All => vec![Screen::Home, Screen::Chess],
        }
    }
}

/// Render screens to PNG files.
#[cfg(feature = "apps")]
#[derive(Debug, clap::Args)]
struct ScreenshotArgs {
    /// Which screen to capture.
    #[arg(long, value_enum, default_value_t = ScreenArg::All)]
    screen: ScreenArg,
    /// Directory to write PNGs into. Created if it does not exist.
    #[arg(long, default_value = "artifacts")]
    out_dir: std::path::PathBuf,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&error);
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), CommandError> {
    match cli.command {
        #[cfg(feature = "desktop")]
        Command::Preview(args) => preview::run(args),
        #[cfg(feature = "apps")]
        Command::Screenshot(args) => screenshot(&args),
        Command::Manifest { command } => manifest::run(command),
        Command::Isolation(args) => device::run(device::DeviceCommand::Isolation(args)),
        Command::Units(args) => device::run(device::DeviceCommand::Units(args)),
        Command::Stock(args) => device::run(device::DeviceCommand::Stock(args)),
    }
}

#[cfg(feature = "apps")]
fn screenshot(args: &ScreenshotArgs) -> Result<(), CommandError> {
    let mut screens = screens::Screens::new()?;
    for screen in args.screen.screens() {
        let canvas = screens.render_offscreen(screen)?;
        let path = args.out_dir.join(format!("{}.png", screen.slug()));
        canvas
            .write_png(&path)
            .map_err(|source| CommandError::Write {
                path: path.clone(),
                source,
            })?;
        let size = canvas.size();
        println!(
            "{} -> {} ({}x{})",
            screen.slug(),
            path.display(),
            size.width,
            size.height
        );
    }
    Ok(())
}

/// Prints an error and everything under it.
///
/// A one-line "invalid manifest" tells a user nothing; the chain is where the
/// actual answer lives (§7: contextual executable errors).
fn report(error: &CommandError) {
    eprintln!("paperctl: {error}");
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}
