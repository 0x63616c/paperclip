#!/bin/sh
# Stages a catalog app for `paperctl package` (§12, ADR-0022): cross-compiles
# its entrypoint for the tablet and drops it where the manifest says to find
# it.
#
#   ./tools/package-app.sh chess
#   ./tools/package-app.sh sudoku
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

app=${1:?"usage: package-app.sh <chess|sudoku>"}
target=aarch64-unknown-linux-gnu
here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source="$here/apps/$app"

case "$app" in
    chess) crate=paper-chess ;;
    sudoku) crate=paper-sudoku ;;
    *)
        echo "package-app.sh: don't know how to build \`$app\` — try chess or sudoku" >&2
        exit 2
        ;;
esac

[ -f "$source/paper.toml" ] || {
    echo "package-app.sh: no $source/paper.toml" >&2
    exit 2
}

cargo build --release --target "$target" --manifest-path "$here/Cargo.toml" -p "$crate" --bin "$app"

built="$here/target/$target/release/$app"
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
cp "$built" "$source/bin/$app"
chmod +x "$source/bin/$app"
echo "staged: $source/bin/$app (aarch64 ELF)"
