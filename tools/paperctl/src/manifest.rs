//! `paperctl manifest` — reading and checking `paper.toml`.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use paper_packages::{MANIFEST_FILE_NAME, Manifest};

use crate::error::CommandError;

/// Manifest subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum ManifestCommand {
    /// Validate a manifest, and optionally the payload beside it.
    Validate {
        /// A package directory, or a `paper.toml` file.
        path: PathBuf,
        /// Also check that the entrypoint and every declared asset is present.
        #[arg(long)]
        payload: bool,
    },
}

/// Runs a manifest subcommand.
pub(crate) fn run(command: ManifestCommand) -> Result<(), CommandError> {
    match command {
        ManifestCommand::Validate { path, payload } => validate(&path, payload),
    }
}

fn validate(path: &Path, check_payload: bool) -> Result<(), CommandError> {
    let root = package_root(path);
    let manifest_path = root.join(MANIFEST_FILE_NAME);

    let manifest = Manifest::read_package(&root).map_err(|source| CommandError::Manifest {
        path: manifest_path.clone(),
        source,
    })?;

    println!("id         {}", manifest.id());
    println!("name       {}", manifest.name());
    println!("version    {}", manifest.version());
    println!("protocol   {}", manifest.protocol());
    println!("entrypoint {}", manifest.entrypoint());
    println!(
        "assets     {}",
        if manifest.assets().is_empty() {
            "none".to_owned()
        } else {
            manifest
                .assets()
                .iter()
                .map(|asset| asset.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!("capabilities  none declared \u{2014} granted at install time by host policy");

    match manifest.ensure_runnable() {
        Ok(()) => println!(
            "runnable   yes (platform speaks {})",
            paper_protocol::CURRENT
        ),
        Err(source) => {
            return Err(CommandError::Manifest {
                path: manifest_path,
                source,
            });
        }
    }

    if check_payload {
        manifest
            .validate_payload(&root)
            .map_err(|source| CommandError::Payload {
                path: root.clone(),
                source,
            })?;
        println!("payload    complete");
    }

    Ok(())
}

/// Accepts either the package directory or the manifest file inside it.
fn package_root(path: &Path) -> PathBuf {
    if path
        .file_name()
        .is_some_and(|name| name == MANIFEST_FILE_NAME)
    {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf()
    } else {
        path.to_path_buf()
    }
}
