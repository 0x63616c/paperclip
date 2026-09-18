//! `cargo xtask` — the `just` recipes with real logic behind them (WWW-45):
//! the device bundle, and ADR scaffolding. Everything simple enough to be a
//! one-line `cargo` invocation lives in the `justfile` instead; this crate
//! exists for the two recipes that are not.

mod adr;
mod device_bundle;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, thiserror::Error)]
enum XtaskError {
    #[error(transparent)]
    DeviceBundle(#[from] device_bundle::DeviceBundleError),
    #[error(transparent)]
    Adr(#[from] adr::AdrError),
}

#[derive(Debug, Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Cross-builds `paperctl` for the tablet and stages it with its digest.
    DeviceBundle,
    /// Scaffolds a new ADR and adds its row to `docs/adr/README.md`.
    NewAdr {
        /// The ADR's title, e.g. "A fourth request: `Launch`".
        title: String,
    },
}

/// The workspace root: two directories up from this crate (`xtask/`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent directory")
        .to_path_buf()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = repo_root();

    let result: Result<(), XtaskError> = match cli.command {
        Command::DeviceBundle => device_bundle::run(&root)
            .map(|bundle| {
                println!("staged: {}", bundle.binary.display());
                println!("sha256: {}", bundle.digest_hex);
            })
            .map_err(XtaskError::from),
        Command::NewAdr { title } => adr::run(&root, &title)
            .map(|stem| println!("scaffolded: {}/{stem}.md", adr::ADR_DIR))
            .map_err(XtaskError::from),
    };

    if let Err(error) = result {
        eprintln!("xtask: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
