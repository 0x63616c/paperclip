//! Installs Paperclip's git hooks into this checkout on every workspace
//! build (WWW-66): `cargo test --workspace`, `just ci`, and everything else
//! `docs/development.md`'s loop already runs build `xtask` as a workspace
//! member, so this runs before an agent's first commit rather than after —
//! `multica repo checkout` hands out a fresh worktree with no hooks active,
//! and nothing on the Multica side re-runs a setup step for it later.
//!
//! Mirrors what `cargo-husky` does from a crate's own build script, without
//! taking on that crate as a dependency: this workspace already keeps its
//! dev tooling in `xtask`, and every workspace-wide build already compiles
//! it. See [`install_hooks`] for the actual logic, shared with
//! `cargo xtask install-hooks`.
//!
//! Best-effort only: a build must not fail because a hook could not be
//! installed, so any error here is dropped rather than panicking the build.

#[path = "src/install_hooks.rs"]
mod install_hooks;

use std::path::Path;

fn main() {
    println!("cargo::rerun-if-changed=src/install_hooks.rs");

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent directory");

    let _ = install_hooks::run(repo_root);
}
