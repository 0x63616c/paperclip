#!/bin/sh
# Compile, link and run the bridge against the real libqsgepaper.so — without
# the reMarkable SDK and without the tablet.
#
#   ./check-link.sh ~/paperclip-vendor
#
# How: an aarch64 Debian sid container. Sid carries Qt 6.10.2 against the
# device's 6.10.3, and a Mac is already aarch64, so the container is the
# device's architecture running natively. That is close enough to answer "does
# this link" definitively, which the ABI check alone cannot.
#
# What it does NOT answer: anything about the panel. The container has no
# /dev/dri, no waveform tables and no e-ink. The run stage is expected to stop
# at the vendor's own precondition check, and the point is *which* check.
#
# Needs docker. `check-abi.sh` is the version that needs nothing.

vendor=$1
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if [ -z "$vendor" ] || [ ! -f "$vendor/libqsgepaper.so" ]; then
    echo "usage: $0 <directory containing libqsgepaper.so>" >&2
    echo "  get one with tools/device-probe/pull-vendor-lib.sh" >&2
    exit 2
fi
command -v docker >/dev/null 2>&1 || { echo "docker is required" >&2; exit 2; }

work=$(mktemp -d) || exit 1
trap 'rm -rf "$work"' EXIT INT TERM HUP

cat > "$work/inside.sh" <<'INNER'
apt-get -qq update >/dev/null 2>&1
DEBIAN_FRONTEND=noninteractive apt-get -qq install -y g++ qt6-base-dev >/dev/null 2>&1 \
    || { echo "FAIL: could not install the toolchain"; exit 1; }

qt=/usr/include/aarch64-linux-gnu/qt6
inc="-I/native -I$qt -I$qt/QtCore -I$qt/QtGui"
echo "toolchain: $(g++ --version | head -1)"
echo "qt:        $(dpkg-query -W -f='${Version}' qt6-base-dev)"

echo "== compile =="
g++ -std=c++17 -c -fPIC -Wall -Wextra -Werror $inc -o /tmp/ep.o /native/paperclip_ep.cpp \
    || { echo "FAIL: the bridge does not compile"; exit 1; }
echo "ok"

echo "== link =="
cat > /tmp/probe.cpp <<'CPP'
#include "paperclip_ep.h"
#include <cstdio>
int main() {
    paperclip_ep *ep = nullptr;
    printf("abi=%u\n", paperclip_ep_abi_version());
    int32_t rc = paperclip_ep_open(&ep);
    printf("open rc=%d err=[%s]\n", rc, paperclip_ep_last_error());
    paperclip_ep_close(ep);
    return rc == 0 ? 0 : 10;
}
CPP
g++ -std=c++17 $inc -o /tmp/probe /tmp/probe.cpp /tmp/ep.o \
    -L/vendor -Wl,-rpath,/vendor -lqsgepaper -lQt6Core -lQt6Gui \
    || { echo "FAIL: the bridge does not link against libqsgepaper.so"; exit 1; }
echo "ok — every EPFramebuffer symbol resolved"

echo "== run =="
# The container has no waveform tables, so preflight must refuse *before* the
# engine gets a chance to abort the process. A SIGABRT here (134) means the
# guard has regressed and the tablet would see a hard crash instead of an error.
QT_QPA_PLATFORM=offscreen timeout 30 /tmp/probe
rc=$?
echo "probe exit=$rc"
case $rc in
  10) echo "ok — refused cleanly, which is the expected result off-device" ;;
  0)  echo "UNEXPECTED: the engine opened. There is no e-ink panel here." ; exit 1 ;;
  134|139) echo "FAIL: the process aborted or segfaulted. preflight() no longer guards the vendor's abort path." ; exit 1 ;;
  *)  echo "FAIL: unexpected exit $rc" ; exit 1 ;;
esac
INNER

docker run --rm --platform linux/arm64 \
    -v "$here:/native:ro" \
    -v "$vendor:/vendor:ro" \
    -v "$work/inside.sh:/inside.sh:ro" \
    debian:sid-slim sh /inside.sh
