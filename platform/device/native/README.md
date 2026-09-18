# platform/device/native

The only C++ in Paperclip. Nine functions, no Qt type crossing the boundary,
no exception escaping it. See
[ADR-0009](../../../docs/adr/0009-device-adapter-ffi-boundary.md) for the full
contract; this file is how to build it.

| File | What it is |
|---|---|
| `paperclip_ep.h` | the C ABI, and the contract in comments |
| `paperclip_ep.cpp` | the bridge — Qt in, C out |
| `ep_abi.hpp` | redeclarations of the vendor's `EPFramebuffer`, nothing else |
| `vendor-abi.txt` | the signatures WWW-20 read off the device |
| `check-abi.sh` | proves `ep_abi.hpp` generates exactly those signatures |
| `check-link.sh` | compiles, links and runs the bridge against the real library |
| `check-host.sh` | runs the two semantic checks below — Qt, no vendor library |
| `check-sharing.cpp` | the Qt implicit-sharing behaviour the display path rests on |
| `check-detach.cpp` | the bridge's no-detach invariant, against a stub engine |

## Check the ABI — needs nothing installed

```sh
./check-abi.sh                              # declarations only
./check-abi.sh ~/paperclip-vendor/libqsgepaper.so   # and against the library
```

The first form runs on a Mac with no Qt, no SDK and no vendor library, and is
the cheapest way to find out that a firmware update moved the ABI. Run it after
every OS update on the tablet.

## Check that it builds — needs docker, not the SDK

```sh
./check-link.sh ~/paperclip-vendor
```

An aarch64 Debian sid container: sid has Qt 6.10.2 against the device's
6.10.3, and a Mac is already aarch64, so this is the device's architecture
running natively. Compiles with `-Werror`, links against the real
`libqsgepaper.so`, and runs far enough to hit `preflight()`.

A `134` or `139` exit is a **failure**: it means the vendor's `abort()` path
was reached, which on the tablet is a dead session rather than an error.

## Check that it is *correct* — needs Qt, not the vendor library

```sh
./check-host.sh
```

`check-abi.sh` checks the declarations and `check-link.sh` checks the link.
Neither would have caught WWW-29, where the bridge built, linked, ran and
returned success while presenting white for four issues. This is the check that
does.

`EPFramebuffer::setBuffers` is an engine-internal init call (WWW-32, on
hardware) — the bridge never calls it. Instead it finds the engine's own
`Format_RGB32` drawing surface inside the already-constructed engine object,
identified by format and stride rather than a fixed offset, and borrows it
through `constBits()`. The bridge's own `QImage` then **shares** the engine's
pixels. Writing through that shared data is how a frame reaches the panel, and
a detach — which Qt performs silently on any non-const accessor — severs it
without an error. So:

- `check-sharing.cpp` pins the Qt behaviour on its own. Runs anywhere with Qt,
  a Mac included.
- `check-detach.cpp` runs the real `paperclip_ep_open` against a stub
  `EPFramebuffer` that carries a real `Format_RGB32` `QImage` at the offset
  `find_draw_surface` scans, plus a same-size `Format_Grayscale8` decoy at the
  offset WWW-32 found holds the engine's *other* internal buffer — and asserts
  that the bridge finds the RGB32 one, that the handed-out buffer *is* the
  engine's memory, that a clear reaches it, and that a forced detach turns
  swap, clear and front readback into `PAPERCLIP_EP_DETACHED`. It needs a
  disposable `/usr/share/remarkable` for `preflight()`, so it is skipped where
  one cannot be made and always runs in `check-link.sh`'s container.

`check-link.sh` runs both as its last stage. See ADR-0009, "Which buffer the
engine presents from".

## Build it for the tablet — needs two things this repository does not contain

### 1. `libqsgepaper.so`, copied off the tablet

It is proprietary: `/usr/share/common-licenses/libqsgepaper/recipeinfo`
records `LICENSE: CLOSED`. It is linkable on the device and **not
redistributable**, so it never enters this repository. Copy it read-only into a
directory outside the tree:

```sh
tools/device-probe/pull-vendor-lib.sh ~/paperclip-vendor
```

That script copies the file, records its SHA-256 and the firmware build it came
from, and changes nothing on the tablet. Keep the recorded digest: it is how a
firmware update that silently replaced the library gets noticed.

### 2. The reMarkable SDK, for Qt 6.10.3 headers

The bridge includes `QImage` and `QRect` and links `Qt6Core` and `Qt6Gui`. The
target is `aarch64-unknown-linux-gnu`, glibc, `libstdc++.so.6` (WWW-1).

The `zig cc` wrapper in `tools/cross/` is **not** a substitute here — it
cross-links pure Rust for the device with nothing installed, which is why the
rest of `platform/device` builds for the tablet on a bare Mac, but it has no Qt
headers and no vendor sysroot.

## Building

```sh
export PAPERCLIP_QT_INCLUDE=/path/to/sdk/sysroots/.../usr/include/qt6
export PAPERCLIP_VENDOR_LIB_DIR=~/paperclip-vendor
export PAPERCLIP_SYSROOT=/path/to/sdk/sysroots/aarch64-remarkable-linux   # optional

cargo build --release -p paper-device \
    --features vendor-engine \
    --target aarch64-unknown-linux-gnu
```

`build.rs` names whichever variable is missing rather than letting it become a
linker error, and refuses outright on a non-aarch64 target.

## Status

**Compiles and links against the real library. Confirmed on hardware: the
Home shelf appears on the panel (WWW-32).**

Verified: `check-abi.sh` passes all four steps against `libqsgepaper.so`
sha256 `3f76b7db…`, image `20260827113527` — after it caught that `UpdateFlag`
is nested in `EPFramebuffer` rather than global. `check-link.sh` then builds
the bridge with `-Werror` against Qt 6.10.2 on aarch64, links it, and runs it
to a clean refusal.

Two things running it established that reading the exports never would: the
engine **segfaults without a live `QCoreApplication`** (so the bridge owns
one), and it **`abort()`s** rather than failing when it cannot initialise (so
`preflight()` checks its preconditions first — `catch (...)` cannot help). A
third, WWW-32, needed the tablet rather than a container: `setBuffers` is
inert from outside, and the engine presents from its own surface, found by
scanning rather than by a fixed offset.

Still unvalidated: the waveform mode numbers, the aux byte order, and most of
what happens once a frame is actually on the glass — panel settle time,
ghosting, and behaviour across a real suspend. Guesses are marked
`UNVERIFIED` in the source; ADR-0009 lists every open gate.
