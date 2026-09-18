#!/bin/sh
# Stages a catalog app for `paperctl package` (§12, ADR-0022): cross-compiles
# its entrypoint for the tablet and drops it where the manifest says to find
# it.
#
#   ./tools/package-app.sh chess
#   ./tools/package-app.sh render-test-card
#
# Takes the app's directory name under `apps/`. The crate and binary names are
# read from the app's own `Cargo.toml` and `paper.toml` rather than held in a
# list here: a list means adding a catalog app silently breaks the release
# workflow, which is exactly what happened when WWW-47 added the render test
# card and `.github/workflows/release.yml` tried to package it (WWW-62).
#
# Leaves `<app>/bin/<app>` in place so `paperctl package apps/<app>` — the
# command `docs/packaging.md` names — can be run directly afterwards.
# `apps/*/bin/` is gitignored: it is a build artefact, not something to
# commit.
#
# Pure Rust, unlike `tools/cross/build-device.sh`: neither app links the
# vendor waveform engine (ADR-0016 — pixels never travel on the wire, and an
# entrypoint draws into its own `paper_sdk::LocalSurfaces` buffer until WWW-4
# gives it a host-mapped one), so the `aarch64-linux-gnu-cc` wrapper in
# `.cargo/config.toml` is enough; no Docker, no Qt headers, no vendor library.

set -eu

app=${1:?"usage: package-app.sh <app-directory-name>"}
target=aarch64-unknown-linux-gnu
here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source="$here/apps/$app"

[ -d "$source" ] || {
    echo "package-app.sh: no app directory at $source" >&2
    exit 2
}

[ -f "$source/paper.toml" ] || {
    echo "package-app.sh: no $source/paper.toml" >&2
    exit 2
}

[ -f "$source/Cargo.toml" ] || {
    echo "package-app.sh: no $source/Cargo.toml" >&2
    exit 2
}

# The `[package] name`, which is the first `name =` in the file — `[[bin]]`'s
# own name comes later and must not win here.
crate=$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$source/Cargo.toml" | head -n 1)
[ -n "$crate" ] || {
    echo "package-app.sh: no [package] name in $source/Cargo.toml" >&2
    exit 2
}

# The manifest says where the entrypoint must land, so it is also what the
# binary has to be called. Deriving it from the manifest rather than assuming
# it matches the directory keeps the two from drifting apart silently.
bin=$(sed -n 's|^entrypoint *= *"bin/\(.*\)"|\1|p' "$source/paper.toml" | head -n 1)
[ -n "$bin" ] || {
    echo "package-app.sh: $source/paper.toml has no \`entrypoint = \"bin/<name>\"\`" >&2
    exit 2
}

cargo build --release --target "$target" --manifest-path "$here/Cargo.toml" -p "$crate" --bin "$bin"

built="$here/target/$target/release/$bin"
[ -f "$built" ] || {
    # The output file from a previous run survives a failed build, so its mere
    # existence proves nothing — `cargo build`'s own non-zero exit above is
    # what would have already stopped this script. This check is for the
    # stranger failure: a build that reports success but the target layout
    # does not match what this script expects.
    echo "package-app.sh: build produced nothing at $built" >&2
    exit 1
}

mkdir -p "$source/bin"
cp "$built" "$source/bin/$bin"
chmod +x "$source/bin/$bin"
echo "staged: $source/bin/$bin (aarch64 ELF)"
