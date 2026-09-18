# The human entry point. `just --list` shows every recipe; each one here
# collects a workflow that used to live only in a shell script, a paragraph of
# docs/development.md, or the runner's head. `just ci` is required to run
# exactly what `.github/workflows/ci.yml` runs (WWW-45) — if you change one,
# change the other in the same commit.

default:
    @just --list

# The checks docs/development.md calls "the loop" — fmt, clippy, clippy-device,
# doc, and test. All must pass before anything is pushed. See the justfile's
# individual recipe comments and docs/development.md for why each is necessary.
fmt:
    cargo fmt --all --check

# `--keep-going` throughout: cargo stops at the first crate that fails, so one
# run reports one crate's errors and each fix only unmasks the next. That cost
# four round-trips through CI in one evening (WWW-59, WWW-65, WWW-61's doc
# links). Reporting every crate's findings at once is the difference between
# one fix and four.
clippy:
    cargo clippy --workspace --all-targets --keep-going -- -D warnings

# The same lints against the device target. Not redundant: `paperctl`'s Mac
# half is `#[cfg(not(target_os = "linux"))]`, so code that is live on a Mac can
# be dead on the tablet — and a Mac-only `just clippy` cannot see it. CI runs on
# Linux and caught six such findings that were invisible locally (WWW-65).
# `check-device` does not cover this: it runs `cargo check`, not `clippy`.
clippy-device:
    cargo clippy --workspace --all-targets --keep-going --target aarch64-unknown-linux-gnu -- -D warnings

test:
    cargo test --workspace

# `-D rustdoc::broken_intra_doc_links` only — the workspace does not yet ask
# rustdoc for `-D warnings` across the board.
#
# `--target aarch64-unknown-linux-gnu`: an intra-doc link to a
# `#[cfg(target_os = "linux")]` item (WWW-67) cannot resolve on the host
# triple's own `target_os`, so this recipe documents the device rather than
# the Mac it runs on — same split `clippy-device`/`check-device` already
# make, for the same reason.
doc:
    RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --workspace --no-deps --keep-going --target aarch64-unknown-linux-gnu

# The docs site under book/: stages the API reference `cargo doc` just built
# into book/src/api (a build artefact, gitignored — the introduction's rustdoc
# link resolves against it), then `mdbook build`, which also runs
# mdbook-linkcheck (WWW-18). `tools/check-book-includes.sh` runs first because
# mdbook 0.4's `{{#include}}` preprocessor only *warns* on a missing target and
# still exits 0 — verified locally — so it is not a gate on its own.
# Needs `cargo install mdbook --version "^0.4"` and `cargo install
# mdbook-linkcheck` (0.7.7 is the release that still speaks mdbook 0.4's
# backend protocol; mdbook 0.5 broke it).
docs: doc
    ./tools/check-book-includes.sh
    rm -rf book/src/api
    mkdir -p book/src/api
    cp -r target/aarch64-unknown-linux-gnu/doc/. book/src/api/
    mdbook build book

# Does the device half compile? Needs nothing but rustup (docs/development.md).
check-device:
    cargo check --workspace --target aarch64-unknown-linux-gnu

# What ships to /home/root/paperclip/bin — no windowing stack, no signing key
# (§12, docs/development.md).
check-paperctl-device:
    cargo check -p paperctl --no-default-features

# Every paperctl / paper-packages / paper-updater feature combination
# compiles, short of `vendor-engine` (needs the vendor SDK; see
# platform/device/native/README.md). Needs `cargo install cargo-hack`.
feature-matrix:
    cargo hack check -p paperctl -p paper-packages -p paper-updater \
        --feature-powerset --exclude-features vendor-engine

# §12: the device build must not contain a code path that can sign a release.
signing-boundary:
    cargo build -p paperctl --no-default-features --release
    ./tools/assert-no-signing-path.sh target/release/paperctl

# Exactly the jobs in .github/workflows/ci.yml, in the same order.
ci: fmt clippy clippy-device test doc docs check-device check-paperctl-device feature-matrix signing-boundary

# Fuzz one of platform/packages/fuzz's targets (`archive` or `manifest`) for
# SECONDS. Needs a nightly toolchain and `cargo install cargo-fuzz`.
fuzz TARGET SECONDS="60":
    cd platform/packages && cargo +nightly fuzz run --fuzz-dir fuzz {{TARGET}} -- -max_total_time={{SECONDS}}

# Open a screen in the desktop preview (docs/development.md: h·c·s·u·f·n·tab·esc).
preview SCREEN="home":
    cargo run -p paperctl -- preview --screen {{SCREEN}}

# Offscreen screenshots of every screen, full target resolution.
screenshot:
    cargo run -p paperctl -- screenshot --screen all --out-dir artifacts

# The §10 failure harness. Needs `brew install qemu`; about three minutes to
# build the VM, about a minute to run (docs/development.md).
vm-create PATH:
    tools/vm-harness/create-vm.sh {{PATH}}

vm-harness PATH:
    tools/vm-harness/run-harness.sh {{PATH}}

# Cross-compiles and stages a catalog app's entrypoint (tools/package-app.sh).
package-app APP:
    ./tools/package-app.sh {{APP}}

# The Docker-based vendor-engine device build and staging — real logic, so it
# lives in xtask rather than growing into a second shell script here.
device-bundle *ARGS:
    cargo xtask device-bundle {{ARGS}}

# Scaffolds docs/adr/NNNN-<slug>.md from the next free ADR number and adds its
# row to docs/adr/README.md, in the same commit.
new-adr TITLE:
    cargo xtask new-adr "{{TITLE}}"

# Points this checkout's git hooks at .githooks/ (WWW-66). Runs automatically
# on every workspace build (xtask/build.rs); this is for confirming it took,
# or a checkout that never triggers that.
install-hooks:
    cargo xtask install-hooks

# "What needs publishing?" — declared app and platform versions against what
# GitHub already lists as released (WWW-61).
plan-release *ARGS:
    cargo xtask plan-release {{ARGS}}

# Shell completions and a man page for `paperctl`, from its own `clap`
# derive — near-free, and cannot drift from what it actually accepts (WWW-48).
docs-paperctl *ARGS:
    cargo xtask docs {{ARGS}}
