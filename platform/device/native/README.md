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

## Check the ABI — needs nothing installed

```sh
./check-abi.sh                              # declarations only
./check-abi.sh ~/paperclip-vendor/libqsgepaper.so   # and against the library
```

The first form runs on a Mac with no Qt, no SDK and no vendor library, and is
the cheapest way to find out that a firmware update moved the ABI. Run it after
every OS update on the tablet.

## Build it — needs two things this repository does not contain

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

**This has never been compiled.** WWW-3's first pass could reach neither the
tablet nor the SDK. What is verified is `check-abi.sh` steps 1–3; everything
else in `paperclip_ep.cpp` is unvalidated, and the guesses are marked
`UNVERIFIED` in the source. ADR-0009 lists every open gate and what each one
blocks.
