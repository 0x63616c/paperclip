//! Shell completions and a man page for `paperctl`, generated from the same
//! `clap` derive that already defines its command line (WWW-48) — so they
//! cannot drift from what the binary actually accepts the way a hand-written
//! copy could.

use std::path::{Path, PathBuf};

use clap::CommandFactory as _;
use clap_complete::Shell;

/// Every shell `clap_complete` knows how to generate a script for.
const SHELLS: [Shell; 5] = [
    Shell::Bash,
    Shell::Elvish,
    Shell::Fish,
    Shell::PowerShell,
    Shell::Zsh,
];

#[derive(Debug, thiserror::Error)]
pub(crate) enum DocsError {
    #[error("cannot create {path}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot generate a {shell} completion script")]
    Completion {
        shell: Shell,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot generate a man page")]
    Manpage(#[source] std::io::Error),
}

/// What [`run`] wrote, for the caller to report.
pub(crate) struct Generated {
    pub(crate) completions: Vec<PathBuf>,
    pub(crate) man_dir: PathBuf,
}

/// Writes a completion script per shell and a man page per subcommand under
/// `out_dir`.
pub(crate) fn run(out_dir: &Path) -> Result<Generated, DocsError> {
    let completions_dir = out_dir.join("completions");
    let man_dir = out_dir.join("man");
    for dir in [&completions_dir, &man_dir] {
        std::fs::create_dir_all(dir).map_err(|source| DocsError::CreateDir {
            path: dir.clone(),
            source,
        })?;
    }

    let mut completions = Vec::with_capacity(SHELLS.len());
    for shell in SHELLS {
        let path = clap_complete::generate_to(
            shell,
            &mut paperctl::Cli::command(),
            "paperctl",
            &completions_dir,
        )
        .map_err(|source| DocsError::Completion { shell, source })?;
        completions.push(path);
    }

    clap_mangen::generate_to(paperctl::Cli::command(), &man_dir).map_err(DocsError::Manpage)?;

    Ok(Generated {
        completions,
        man_dir,
    })
}
