//! `paperctl` — the one entry point into Paperclip from a developer's machine (§4).
//!
//! Stage 1 gives it three jobs: open the desktop preview, capture screenshots
//! at target resolution, and validate a `paper.toml`. Device commands arrive
//! when there is a device adapter to drive (WWW-3).

mod error;
mod manifest;
mod preview;
mod screens;

use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use crate::error::CommandError;
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
    Preview(preview::PreviewArgs),
    /// Render screens to PNG files without opening a window.
    Screenshot(ScreenshotArgs),
    /// Validate a `paper.toml` manifest.
    Manifest {
        #[command(subcommand)]
        command: manifest::ManifestCommand,
    },
}

/// Which screen to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScreenArg {
    /// The home screen.
    Home,
    /// The Chess screen.
    Chess,
    /// Both, one file each.
    All,
}

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
        Command::Preview(args) => preview::run(args),
        Command::Screenshot(args) => screenshot(&args),
        Command::Manifest { command } => manifest::run(command),
    }
}

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
