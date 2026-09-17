#!/bin/sh
# The checks that need Qt, but neither `libqsgepaper.so` nor the tablet.
#
#   ./check-host.sh
#
# Two programs, both about the one thing the display path rests on: the vendor
# engine holds a QImage *sharing* our pixel data, so Qt's implicit-sharing rules
# are inverted here — writing through shared data is the transport, and a detach
# is a bug rather than a convenience (WWW-29, WWW-30; ADR-0009).
#
#   check-sharing.cpp  the Qt behaviour on its own, and the detach that breaks
#                      it. Needs nothing but Qt; runs anywhere, Mac included.
#   check-detach.cpp   the bridge's real `paperclip_ep_open` path against a
#                      stub EPFramebuffer that shares its buffers the way the
#                      real one does, and the PAPERCLIP_EP_DETACHED guard.
#                      Needs a disposable /usr/share/remarkable for preflight(),
#                      so it is skipped where one cannot be made — the container
#                      in `check-link.sh` is where it always runs.
#
# `check-abi.sh` checks the declarations, `check-link.sh` checks the link against
# the real library. This checks the semantics. None of the three is a display
# test; only the tablet is.

set -e
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
work=$(mktemp -d) || exit 1
waveform_made=""
cleanup() {
    rm -rf "$work"
    [ -n "$waveform_made" ] && rm -rf "$waveform_made"
    return 0
}
trap cleanup EXIT INT TERM HUP

CXX=${CXX:-g++}
command -v "$CXX" >/dev/null 2>&1 || { echo "FAIL: no C++ compiler ($CXX)" >&2; exit 2; }

# Qt flags: pkg-config where it knows, else Debian multiarch, else Homebrew.
if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists Qt6Gui 2>/dev/null; then
    qt_cflags=$(pkg-config --cflags Qt6Gui Qt6Core)
    qt_libs=$(pkg-config --libs Qt6Gui Qt6Core)
elif [ -d /usr/include/aarch64-linux-gnu/qt6 ]; then
    qt=/usr/include/aarch64-linux-gnu/qt6
    qt_cflags="-I$qt -I$qt/QtCore -I$qt/QtGui"
    qt_libs="-lQt6Core -lQt6Gui"
elif [ -d /opt/homebrew/opt/qt/include ]; then
    qt=/opt/homebrew/opt/qt
    qt_cflags="-I$qt/include -I$qt/include/QtCore -I$qt/include/QtGui"
    qt_libs="-F$qt/lib -framework QtCore -framework QtGui"
else
    echo "FAIL: no Qt 6 headers found. Install qt6-base-dev, or run check-link.sh." >&2
    exit 2
fi
echo "compiler:  $($CXX --version | head -1)"

status=0

echo "== Qt implicit sharing =="
$CXX -std=c++17 -Wall -Wextra -Werror $qt_cflags -o "$work/sharing" \
    "$here/check-sharing.cpp" $qt_libs \
    || { echo "FAIL: check-sharing.cpp does not build"; exit 1; }
"$work/sharing" || status=1

echo "== the bridge's no-detach invariant =="
# preflight() refuses without the vendor's waveform directory, and it is right
# to. Make a throwaway one if it is absent and we can; never touch a real one.
if [ ! -d /usr/share/remarkable ]; then
    if mkdir -p /usr/share/remarkable 2>/dev/null; then
        waveform_made=/usr/share/remarkable
        printf 'not a waveform table\n' > /usr/share/remarkable/paperclip-check.bin
    fi
fi
if [ ! -d /usr/share/remarkable ]; then
    echo "SKIP: needs a disposable /usr/share/remarkable for preflight()."
    echo "      check-link.sh runs this in a container, where making one is free."
else
    $CXX -std=c++17 -Wall -Wextra -Werror -I"$here" $qt_cflags -o "$work/detach" \
        "$here/check-detach.cpp" $qt_libs \
        || { echo "FAIL: check-detach.cpp does not build"; exit 1; }
    "$work/detach" || status=1
fi

[ "$status" -eq 0 ] && echo "all host checks passed" || echo "host checks FAILED"
exit "$status"
