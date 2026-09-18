#!/bin/sh
# Stages a platform bundle for `paperctl upgrade package` (§13, WWW-60):
# cross-compiles the four required components and collects them under one
# `bin/` directory, the layout `platform/updater/src/bundle.rs` describes and
# `paperctl upgrade package --source <dir>` reads.
#
#   ./tools/stage-platform.sh [output-dir]
#
# Defaults to `target/platform-stage`. `target/` is already gitignored, so
# nothing here needs a staging-specific ignore rule.
#
# Not `tools/package-app.sh`: that script stages one catalog app at
# `apps/<app>/bin/<app>`, for `paperctl package apps/<app>`. A platform
# release is a different shape — four named components in one directory, none
# of them living under `apps/` — and conflating the two would make
# `package-app.sh` take an argument that changes what directory layout it
# produces, for two things that are staged for two different commands. None
# of the four components here links the vendor waveform engine (`home`,
# `app-store` and `settings` for the same ADR-0022 reason chess and sudoku do
# not; `paperclip-host` because `paper-device`'s `vendor-engine` feature is
# off by default), so — like `package-app.sh` — this needs only the
# `aarch64-linux-gnu-cc` (`zig cc`) wrapper `.cargo/config.toml` already
# points the target at, not `tools/cross/build-device.sh`'s Docker/Qt-headers
# machinery.

set -eu

target=aarch64-unknown-linux-gnu
here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=${1:-"$here/target/platform-stage"}

# package, bin target, staged name
components="
paper-host:paperclip-host:paperclip-host
paper-home:home:home
paper-app-store:app-store:app-store
paper-settings:settings:settings
"

mkdir -p "$out/bin"
for component in $components; do
    [ -n "$component" ] || continue
    package=${component%%:*}
    rest=${component#*:}
    bin_name=${rest%%:*}
    staged_name=${rest#*:}

    cargo build --release --target "$target" --manifest-path "$here/Cargo.toml" \
        -p "$package" --bin "$bin_name"

    built="$here/target/$target/release/$bin_name"
    [ -f "$built" ] || {
        echo "stage-platform.sh: build produced nothing at $built" >&2
        exit 1
    }
    cp "$built" "$out/bin/$staged_name"
    chmod +x "$out/bin/$staged_name"
    echo "staged: $out/bin/$staged_name (aarch64 ELF)"
done

echo "platform components staged in $out"
