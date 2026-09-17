#!/bin/sh
# Verify that ep_abi.hpp declares exactly the symbols libqsgepaper.so exports.
#
# Runs anywhere with a C++ compiler; needs no Qt, no reMarkable SDK and no
# vendor library. Given a copy of libqsgepaper.so as $1 it additionally checks
# that every symbol it generates is really exported by that library.
#
#   ./check-abi.sh                          # declaration check only
#   ./check-abi.sh /path/to/libqsgepaper.so # and check against the library
#
# Deliberately no `set -e`: every check must run so the report is complete.

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
expected="$here/vendor-abi.txt"
vendor_lib="$1"
work=$(mktemp -d) || exit 1
trap 'rm -rf "$work"' EXIT INT TERM HUP

CXX=${CXX:-c++}
command -v "$CXX" >/dev/null 2>&1 || { echo "no C++ compiler ($CXX)"; exit 1; }

# A translation unit that names every declared entry point without calling it.
# Pointer-to-member does not need complete parameter types, so this compiles
# with nothing but the stub declarations below.
cat > "$work/probe.cpp" <<'CPP'
namespace std { template <typename...> class tuple; }
class QImage;
class QRect;
class QRegion;
template <typename Enum> class QFlags;
class EPScreenModeMap;
enum EPScreenMode : int;

#include "ep_abi.hpp"

void paperclip_abi_probe();
void paperclip_abi_probe() {
    EPFramebuffer::instance();
    void (EPFramebuffer::*set)(std::tuple<QImage, QImage>, QImage *) =
        &EPFramebuffer::setBuffers;
    void (EPFramebuffer::*swap_rect)(QRect, EPScreenMode,
                                     QFlags<EPFramebuffer::UpdateFlag>) =
        &EPFramebuffer::swapBuffers;
    void (EPFramebuffer::*swap_region)(const QRegion &, const EPScreenModeMap &,
                                       QFlags<EPFramebuffer::UpdateFlag>) =
        &EPFramebuffer::swapBuffers;
    void (EPFramebuffer::*ghost)(EPFramebuffer::GhostControlMode) =
        &EPFramebuffer::ghostControl;
    bool (EPFramebuffer::*check)() = &EPFramebuffer::checkLockFile;
    void (EPFramebuffer::*crash)() = &EPFramebuffer::handleCrash;
    (void)set; (void)swap_rect; (void)swap_region;
    (void)ghost; (void)check; (void)crash;
}
CPP

"$CXX" -std=c++17 -c -I"$here" -o "$work/probe.o" "$work/probe.cpp" || {
    echo "FAIL: ep_abi.hpp does not compile"
    exit 1
}

# Undefined symbols of the probe object are exactly the vendor entry points.
# nm prints a leading underscore on Mach-O and none on ELF; strip one if there.
nm -u "$work/probe.o" 2>/dev/null \
    | tr -d ' ' \
    | sed -e 's/^U//' -e 's/^_\(_Z\)/\1/' \
    | grep '^_ZN13EPFramebuffer' \
    | sort -u > "$work/mangled"

if [ ! -s "$work/mangled" ]; then
    echo "FAIL: the probe generated no EPFramebuffer symbols"
    exit 1
fi

# Demangle. `c++filt` on a Mach-O host strips a leading underscore by default
# and so leaves an ELF name untouched; `-n` turns that off and is accepted by
# both the Apple and the GNU binutils versions. Try the variants and keep the
# first that actually demangles something.
demangle() {
    for candidate in "llvm-cxxfilt" "c++filt -n" "c++filt"; do
        set -- $candidate
        command -v "$1" >/dev/null 2>&1 || continue
        if "$@" < "$work/mangled" 2>/dev/null | grep -q '^EPFramebuffer::'; then
            "$@" < "$work/mangled" 2>/dev/null
            return 0
        fi
    done
    return 1
}
if ! demangle | sort -u > "$work/actual"; then
    echo "SKIP: no working demangler; comparing mangled names only"
    cat "$work/mangled" > "$work/actual"
fi
grep -v '^[[:space:]]*#' "$expected" | grep -v '^[[:space:]]*$' | sort -u > "$work/want"

status=0
if diff -u "$work/want" "$work/actual" > "$work/diff"; then
    echo "OK: ep_abi.hpp generates exactly the $(wc -l < "$work/want" | tr -d ' ') recorded signatures"
else
    echo "FAIL: ep_abi.hpp and vendor-abi.txt disagree"
    cat "$work/diff"
    status=1
fi

if [ -n "$vendor_lib" ]; then
    if [ ! -f "$vendor_lib" ]; then
        echo "FAIL: no such library: $vendor_lib"
        exit 1
    fi
    echo "checking against $vendor_lib"
    exported=$work/exported
    if command -v llvm-nm >/dev/null 2>&1; then
        llvm-nm --dynamic --defined-only "$vendor_lib" 2>/dev/null | awk '{print $NF}' > "$exported"
    elif nm -D "$vendor_lib" >/dev/null 2>&1; then
        nm -D --defined-only "$vendor_lib" 2>/dev/null | awk '{print $NF}' > "$exported"
    else
        # No ELF-aware nm here. strings finds mangled names in .dynstr, which
        # is enough for a presence check.
        strings -a "$vendor_lib" | grep '^_ZN13EPFramebuffer' > "$exported"
    fi
    while read -r symbol; do
        if grep -qx "$symbol" "$exported"; then
            echo "  present: $symbol"
        else
            echo "  MISSING: $symbol"
            status=1
        fi
    done < "$work/mangled"
fi

exit $status
