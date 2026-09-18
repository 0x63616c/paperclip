# The human entry point. `just --list` shows every recipe; each one here
# collects a workflow that used to live only in a shell script, a paragraph of
# docs/development.md, or the runner's head. `just ci` is required to run
# exactly what `.github/workflows/ci.yml` runs (WWW-45) — if you change one,
# change the other in the same commit.

default:
    @just --list

# The three checks docs/development.md calls "the loop." All three must pass
# before anything is pushed.
fmt:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# The same lints against the device target. Not redundant: `paperctl`'s Mac
# half is `#[cfg(not(target_os = "linux"))]`, so code that is live on a Mac can
# be dead on the tablet — and a Mac-only `just clippy` cannot see it. CI runs on
# Linux and caught six such findings that were invisible locally (WWW-65).
# `check-device` does not cover this: it runs `cargo check`, not `clippy`.
clippy-device:
    cargo clippy --workspace --all-targets --target aarch64-unknown-linux-gnu -- -D warnings

test:
    cargo test --workspace

# `-D rustdoc::broken_intra_doc_links` only — the workspace does not yet ask
# rustdoc for `-D warnings` across the board.
doc:
    RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --workspace --no-deps

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
ci: fmt clippy clippy-device test doc check-device check-paperctl-device feature-matrix signing-boundary

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
