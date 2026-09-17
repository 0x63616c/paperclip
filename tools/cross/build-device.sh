#!/bin/sh
# Build a Paperclip binary for the tablet, with the vendor waveform engine.
#
#   ./tools/cross/build-device.sh <example-name>
#
# Runs in an aarch64 Debian bookworm container, and both halves of that matter:
#
#   aarch64  — the Mac is already aarch64, so this is native, not emulated.
#   bookworm — glibc 2.36 against the device's 2.39. Linking against an older
#              glibc runs on a newer one; the reverse is the "GLIBC_2.39 not
#              found" failure that only shows up on the tablet.
#
# Why not the `zig cc` wrapper used for pure-Rust device builds: zig ships
# **libc++**, whose `std::tuple` mangles as `St3__16tupleI...`. `libqsgepaper.so`
# was built with **libstdc++**, which mangles it `St5tupleIJ...`. So
# `EPFramebuffer::setBuffers` does not resolve and the link fails on that one
# symbol. Anything that touches the vendor C++ ABI must be built with a GCC
# toolchain. ADR-0009 predicted this class of problem; this is it arriving.
#
# Inputs, neither of them in this repository:
#   PAPERCLIP_QT_INCLUDE      Qt 6.10 headers  (default ~/paperclip-qt/include)
#   PAPERCLIP_VENDOR_LIB_DIR  libqsgepaper.so and the Qt/libstdc++ libraries
#                             copied off the device, so the ABI is the
#                             device's own  (default ~/paperclip-vendor/lib)

example=${1:-takeover}
qt=${PAPERCLIP_QT_INCLUDE:-$HOME/paperclip-qt/include}
vendor=${PAPERCLIP_VENDOR_LIB_DIR:-$HOME/paperclip-vendor/lib}
here=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

[ -d "$qt" ] || { echo "no Qt headers at $qt" >&2; exit 2; }
[ -f "$vendor/libqsgepaper.so" ] || { echo "no libqsgepaper.so in $vendor" >&2; exit 2; }
command -v docker >/dev/null 2>&1 || { echo "docker is required" >&2; exit 2; }

mkdir -p "$here/target/device-container" "$HOME/.cargo/registry"

docker run --rm --platform linux/arm64 \
    -v "$here:/src" \
    -v "$qt:/qt:ro" \
    -v "$vendor:/vendor:ro" \
    -v "$HOME/.cargo/registry:/root/.cargo/registry" \
    -e "EXAMPLE=$example" \
    debian:bookworm-slim sh -euc '
        export DEBIAN_FRONTEND=noninteractive
        apt-get -qq update >/dev/null
        apt-get -qq install -y g++ curl ca-certificates >/dev/null
        if [ ! -x /root/.cargo/bin/cargo ]; then
            curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.94.0 >/dev/null
        fi
        export PATH=/root/.cargo/bin:$PATH
        export PAPERCLIP_QT_INCLUDE=/qt
        export PAPERCLIP_VENDOR_LIB_DIR=/vendor
        # .cargo/config.toml points the device triple at the zig wrapper, which
        # is right on the Mac and absent in here. Inside the container this is a
        # native build, so the system compiler is the linker.
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=cc
        cd /src
        cargo build --release -p paper-device --example "$EXAMPLE" \
            --features vendor-engine \
            --target-dir /src/target/device-container
    '
built="$here/target/device-container/release/examples/$example"
[ -f "$built" ] || { echo "build produced nothing" >&2; exit 1; }
echo "built: target/device-container/release/examples/$example"
