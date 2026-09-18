//! Points this checkout's git hooks at `.githooks/` (WWW-66), so the fmt,
//! clippy and test checks CI runs also run locally before a commit or push
//! lands, instead of surfacing only after CI catches them.
//!
//! `xtask/build.rs` calls [`run`] on every workspace build, so a fresh
//! `multica repo checkout` gets the hooks installed the first time anything
//! in the workspace is built — `cargo test --workspace`, `just ci`, or any
//! of the other workspace-wide commands `docs/development.md`'s loop already
//! asks for. `cargo xtask install-hooks` reruns it by hand, for a checkout
//! that only ever builds a single crate and so never triggers the build
//! script.

use std::path::Path;
use std::process::Command;

/// Relative to the repo root; committed, so every checkout already has it.
pub(crate) const HOOKS_DIR: &str = ".githooks";

/// Git prefers these over `current_dir` when resolving which repository a
/// command targets. A git hook sets them for every process it spawns — and
/// `pre-push` spawning `cargo test`, which spawns this, is exactly that
/// path — so left alone they redirect `git config`/`git init` below to
/// whichever repository invoked us instead of `repo_root`. Clearing them is
/// what makes `repo_root` the actual target regardless of what called us.
const GIT_ENV_VARS_TO_CLEAR: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
];

/// A `git` invocation pinned to `repo_root`, immune to the ambient
/// environment redirecting it elsewhere (see [`GIT_ENV_VARS_TO_CLEAR`]).
fn git_in(repo_root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(repo_root);
    for var in GIT_ENV_VARS_TO_CLEAR {
        command.env_remove(var);
    }
    command
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HooksOutcome {
    /// `core.hooksPath` already pointed at `.githooks`; nothing to do.
    AlreadyInstalled,
    /// `core.hooksPath` now points at `.githooks`.
    Installed,
    /// Not a git checkout, or `.githooks/pre-commit` is missing — a packaged
    /// source tarball, say. Nothing to point at, so nothing was done.
    Skipped,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InstallHooksError {
    #[error("cannot run git")]
    RunGit(#[source] std::io::Error),
    #[error("git config core.hooksPath failed: {0}")]
    GitConfigFailed(std::process::ExitStatus),
}

/// The checkout's current local `core.hooksPath`, or `None` if it is unset.
fn current_hooks_path(repo_root: &Path) -> Option<String> {
    let output = git_in(repo_root)
        .args(["config", "--local", "--get", "core.hooksPath"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Sets `core.hooksPath` to `.githooks`, local to this checkout only, unless
/// it already points there. Does nothing outside a git checkout, or in one
/// with no `.githooks/pre-commit` to point at, rather than treating either as
/// an error: a build must not fail over a dev-convenience side effect.
pub(crate) fn run(repo_root: &Path) -> Result<HooksOutcome, InstallHooksError> {
    if !repo_root.join(".git").exists() || !repo_root.join(HOOKS_DIR).join("pre-commit").exists() {
        return Ok(HooksOutcome::Skipped);
    }

    if current_hooks_path(repo_root).as_deref() == Some(HOOKS_DIR) {
        return Ok(HooksOutcome::AlreadyInstalled);
    }

    let status = git_in(repo_root)
        .args(["config", "--local", "core.hooksPath", HOOKS_DIR])
        .status()
        .map_err(InstallHooksError::RunGit)?;

    if !status.success() {
        return Err(InstallHooksError::GitConfigFailed(status));
    }

    Ok(HooksOutcome::Installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn init_repo(dir: &Path) {
        let status = git_in(dir).args(["init", "-q"]).status().unwrap();
        assert!(status.success());
    }

    #[test]
    fn skips_a_directory_with_no_git_checkout() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run(dir.path()).unwrap(), HooksOutcome::Skipped);
    }

    #[test]
    fn skips_a_git_checkout_with_no_githooks_dir() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        assert_eq!(run(dir.path()).unwrap(), HooksOutcome::Skipped);
    }

    #[test]
    fn installs_then_reports_already_installed() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::create_dir_all(dir.path().join(HOOKS_DIR)).unwrap();
        fs::write(dir.path().join(HOOKS_DIR).join("pre-commit"), "#!/bin/sh\n").unwrap();

        assert_eq!(run(dir.path()).unwrap(), HooksOutcome::Installed);
        assert_eq!(run(dir.path()).unwrap(), HooksOutcome::AlreadyInstalled);
        assert_eq!(current_hooks_path(dir.path()).as_deref(), Some(HOOKS_DIR));
    }
}
