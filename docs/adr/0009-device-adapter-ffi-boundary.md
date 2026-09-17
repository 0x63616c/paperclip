# ADR-0009 — The device adapter's FFI boundary

**Status:** accepted, and **exercised on hardware** — three takeover round
trips on 2026-09-17 with stock verified healthy after each (Stage 3, WWW-3).
See `docs/device/www-3-takeover-session.md`.
Implements the decision in [ADR-0007](0007-display-transport-via-vendor-waveform-engine.md);
this ADR is about *how* the vendor engine is called, not whether it is.

## Context

ADR-0007 settled that presentation goes through `libqsgepaper.so`'s
`EPFramebuffer` rather than raw DRM. That library is proprietary
(`LICENSE: CLOSED`), ships no header, and exports a **C++ ABI with Qt types in
its signatures**. Rust cannot call it directly.

§9 permits C++/Qt "only where Stage 1 proved it necessary, isolated under
`platform/device/native`, behind a small C ABI with a safe Rust wrapper", and
requires that ownership, thread affinity, buffer lifetimes, error conversion
and shutdown ordering be documented, with **no C++ exceptions and no Rust
panics crossing the boundary**. This ADR is that documentation.

## Decision

Four layers, each with one job.

```
paper_sdk::Canvas          shared rasterisation — same code as the Mac backend
      |
paper_device::panel        Panel trait, present(), MemoryPanel
      |
paper_device::vendor       unsafe extern "C", status -> Result, RAII handle
      |
native/paperclip_ep.cpp    C ABI: nine functions, no Qt type crosses it
      |
libqsgepaper.so            EPFramebuffer, proprietary, never vendored
```

### Ownership

- The **handle** (`paperclip_ep *`) is created by `paperclip_ep_open` and
  destroyed only by `paperclip_ep_close`. `VendorPanel` owns exactly one and
  closes it in `Drop`, including on the error paths inside `open` itself.
- The **pixel buffer** is owned by the bridge — three `QImage`s allocated in
  `paperclip_ep_open` and handed to `EPFramebuffer::setBuffers`. Rust borrows
  the bits of one of them and frees nothing.
- The **error string** is thread-local storage inside the bridge. Rust copies
  it before returning; it never holds the pointer.

### A Qt application object, owned by the bridge

`EPFramebuffer::instance()` **segfaults with no `QCoreApplication` alive** —
the first thing a container run established. A plain `QCoreApplication` is
enough: no `QGuiApplication`, no QPA platform plugin, no window system.

Paperclip's host is a Rust process with no Qt in it, so the bridge constructs
one itself in `paperclip_ep_open` and destroys it in `paperclip_ep_close`,
after the engine's `QImage` buffers. If the host already has one — it will not,
but a test harness might — the bridge uses it and does not take ownership.

This is better news than ADR-0007's "what would make this wrong" section
feared. The worry was that needing a `QGuiApplication` would make
`libqsgepaper` no simpler than rmweb's QPA route; a bare `QCoreApplication` is
a much smaller dependency than a platform plugin, so the primary path stays
the lighter of the two.

### Thread affinity

`libepaper` asserts *"EPFramebuffer is being created outside the application's
main thread!"* (WWW-1). So:

- `paperclip_ep_open` must be called on the process's main thread.
- The bridge records `pthread_self()` at open and every later call checks it,
  returning `PAPERCLIP_EP_WRONG_THREAD` rather than proceeding.
- `VendorPanel` holds a `NonNull`, so it is neither `Send` nor `Sync` and the
  compiler enforces statically what the C++ only catches at runtime. That is
  deliberate belt and braces on the one rule whose violation is an assertion in
  closed code rather than an error we can handle.

### Buffer lifetimes

`Panel::buffer()` returns a `PanelBuffer<'_>` borrowed from `&mut self`. The
slice cannot outlive the panel, so it cannot outlive the engine that owns the
memory, and no second borrow can exist while one is live. The C side's
`paperclip_ep_buffer` returns a raw pointer with no lifetime at all — the
lifetime is *manufactured* at the Rust boundary, which is the only place it can
be.

### Error conversion

The C ABI returns `int32_t` and nothing else. Seven status codes, defined once
in `native/paperclip_ep.h` and mirrored in `paper_device::VendorStatus`, with
an `Unknown` arm so a bridge from a future build cannot produce a value that
panics a match. Every non-zero status becomes `DeviceError::Vendor { code,
message }`, the message copied out of the bridge's thread-local slot or falling
back to the code's own description.

### No exception, no panic — and the one thing this cannot promise

- Every `extern "C"` function in the bridge catches `const std::exception &`
  and `...`, mapping both to `PAPERCLIP_EP_EXCEPTION`. No *exception*
  propagates.
- **The vendor calls `abort()`.** Observed, not inferred: with no waveform
  tables present, `EPFramebuffer`'s constructor prints
  `Failed to initialize SWTCON.` and raises `SIGABRT`. A `catch (...)` cannot
  intercept that — there is no unwinding to catch. §9 asks that no C++
  exception cross the boundary and none does, but "the bridge converts every
  vendor failure into a status code" would have been a false claim, and the
  earlier draft of this ADR implied it.
- The only defence is to not reach it. `preflight()` runs before
  `EPFramebuffer::instance()` and refuses with `PAPERCLIP_EP_NOT_OPEN` when a
  precondition the engine aborts on is absent. `check-link.sh` asserts this
  by treating a 134 or 139 exit as a failure, so a regression in the guard is
  a red test rather than a tablet that dies mid-session.
- Preconditions guarded today: a live `QCoreApplication`, and the existence
  and non-emptiness of the four waveform tables. Others certainly exist and
  will be found the same way — by running it somewhere that is not the
  tablet.
- Exceptions stay *enabled* in the build. `-fno-exceptions` would turn a
  vendor throw into `std::terminate` rather than into a status code, which is
  the opposite of the goal.
- On the Rust side, no `unwrap`, no indexing and no arithmetic that can
  overflow occurs between entering a wrapper and returning a `Result`. The
  workspace lints (`panic_in_result_fn`, `undocumented_unsafe_blocks`) apply to
  this crate like any other; `unsafe_code` is allowed in exactly two modules,
  each with the justification the workspace manifest anticipated.

### Shutdown ordering

1. Clear the panel — `Panel::clear()`, a settled full refresh.
2. Drop the `VendorPanel`, which closes the handle and frees the buffers.
3. Release the wakelock (`WakeLock::release`, or its `Drop`).
4. The host restarts `xochitl.service`. **Cleanly. Never by killing it** — see
   ADR-0008's by-product.

`Drop` on `VendorPanel` deliberately does *not* clear. A drop during an unwind
should hand the display back as fast as possible, not spend a settled full
refresh doing it; the clear belongs on the ordinary exit path where its failure
can be reported. This means **a crashed adapter leaves residue on the glass**,
which WWW-20 photographed. That is a known consequence, not an oversight.

## How the declarations are verified without the library

This is the part worth reading twice. Calling a proprietary C++ library with no
header means redeclaring its interface and trusting Itanium name mangling to
match. A redeclaration that is subtly wrong does not fail to build — it fails
to link, or links to the wrong overload.

So the declarations live alone in `native/ep_abi.hpp`, depend on nothing but
type *names*, and `native/check-abi.sh`:

1. compiles them against stub Qt declarations — **no Qt, no SDK, no vendor
   library, runs on a Mac**;
2. dumps the symbols the translation unit generates;
3. demangles them and diffs against `native/vendor-abi.txt`, the seven
   signatures WWW-20 read off `libqsgepaper.so` on image `20260827113527`;
4. given a copy of the library, additionally checks each mangled name is
   really exported by it.

**All four steps pass as of 2026-09-17**, against `libqsgepaper.so` sha256
`3f76b7db328f7e16cde2d360ebe4a1db043a113cd2386d4c995cc4d25a0eeaec`, pulled
read-only from image `20260827113527` (`IMG_VERSION 3.28.0.172`). The seven
mangled names, each confirmed exported by that library:

```
_ZN13EPFramebuffer8instanceEv
_ZN13EPFramebuffer10setBuffersESt5tupleIJ6QImageS1_EEPS1_
_ZN13EPFramebuffer11swapBuffersE5QRect12EPScreenMode6QFlagsINS_10UpdateFlagEE
_ZN13EPFramebuffer11swapBuffersERK7QRegionRK15EPScreenModeMap6QFlagsINS_10UpdateFlagEE
_ZN13EPFramebuffer12ghostControlENS_16GhostControlModeE
_ZN13EPFramebuffer13checkLockFileEv
_ZN13EPFramebuffer11handleCrashEv
```

**Step 4 earned its keep on its first run.** Two of the seven came back
`MISSING`: the signatures recorded from WWW-20's inspection had
`QFlags<UpdateFlag>`, and the library has `QFlags<EPFramebuffer::UpdateFlag>` —
the enum is nested, not global. That is a one-token difference that mangles to
`NS_10UpdateFlagE` rather than `10UpdateFlagE` and would have linked to
nothing. `ep_abi.hpp`, `vendor-abi.txt` and the bridge's `swapBuffers` call are
corrected.

This is also the firmware-drift alarm. When an OS update replaces the rootfs,
re-running the check against the new library says immediately whether the ABI
moved, rather than leaving it to be discovered by a black screen.

## Building it: GCC, not zig

Worth recording because it cost a debugging cycle. The `zig cc` wrapper that
cross-links pure Rust for the tablet **cannot build this bridge**. Zig ships
libc++, whose `std::tuple` mangles as `St3__16tupleI…`; `libqsgepaper.so` was
built with libstdc++, which mangles it `St5tupleIJ…`. So
`EPFramebuffer::setBuffers` fails to resolve, and only that one symbol does.

`tools/cross/build-device.sh` therefore builds in an aarch64 Debian **bookworm**
container: GCC for the libstdc++ ABI, and glibc 2.36 against the device's 2.39,
because an older glibc runs on a newer one and the reverse is the
`GLIBC_2.39 not found` failure that only appears on the tablet. Qt headers come
from a 6.10 extract; the libraries linked against are **copies pulled off the
device itself**, so the ABI is the device's own rather than a distribution's.

## Does it link? Yes — `check-link.sh`

`check-abi.sh` proves the declarations match the exports. It does not prove the
bridge builds, and the reMarkable SDK was not available to find out.

`native/check-link.sh` answers it without the SDK, using an aarch64 Debian sid
container: sid carries Qt **6.10.2** against the device's 6.10.3, and a Mac is
already aarch64, so the container is the device's architecture running
natively. It compiles `paperclip_ep.cpp` with `-Wall -Wextra -Werror`, links it
against the real `libqsgepaper.so`, and runs it.

Result, 2026-09-17: **compiles clean, links clean — every `EPFramebuffer`
symbol resolved** — and the run stops exactly where it should, in
`preflight()`, because a container has no waveform tables.

What it does not answer: anything about the glass. No `/dev/dri`, no e-ink, no
waveform data. It is a build and initialisation check, not a display test, and
the script says so.

## What else the library says about itself

Read out of the pulled copy and out of running it:

- **`EPFramebuffer` is a `QObject`.** It exports `qt_metacall`, `qt_metacast`
  and `staticMetaObject`, and has a `framebufferUpdated(const QRect &)`
  signal. It also has a public constructor and destructor, a
  `forceInstance(EPFramebuffer *)`, and `showForWindow(QWindow *)` /
  `hideForWindow(QWindow *)` — an API that expects windows to exist.
- **It links Qt Quick, but does not appear to need it running.**
  `DT_NEEDED` lists `libQt6Core.so.6`, `libQt6Gui.so.6`, `libQt6Qml.so.6`,
  `libQt6Quick.so.6`, `libdrm.so.2`, `libstdc++.so.6` — it is a *scenegraph*
  plugin, which is what its path says. Those are satisfied by the dynamic
  linker; construction itself needed only a `QCoreApplication`.
- **It reaches the hardware itself.** Strings name `/dev/dri/card0`,
  `/sys/devices/platform/cumulus-panel` and the `g2194-regulator` at i2c
  `1-0048` — so the DRM and power work WWW-20 did by hand is inside the
  engine, which is the point of using it.
- **It participates in the advisory locks**, naming `/tmp/epd.lock` and
  `/tmp/epframebuffer.lock` directly, and carries the string
  `"Failed to lock epframebuffer. Is there another EPFramebuffer instance?"`.
- **Waveform tables are files, per panel lot, and mandatory.** Four of them:
  `/usr/share/remarkable/ct33_{std,best,fast,pen}.bin`. The engine loads them
  at construction and validates their size — a wrong-sized file produced
  `unexpected size: 65536`. `ct33` is this panel's lot; strings show a
  `wf_search` keyed by lot and TFT, so a different panel wants different
  names. **`preflight()` therefore checks existence and non-emptiness only**;
  hard-coding a size would trade the engine's abort for our own wrong
  refusal.
- The backend in play is `epframebuffer_acep2.cpp`, and success prints
  `SWTCON initialized \o/`.

Net: the primary path is in better shape than it looked. The compile-and-link
gate is closed, `QCoreApplication` is a far lighter requirement than the
`QGuiApplication` this ADR previously feared, and rmweb's QPA route stays the
fallback rather than becoming the cheaper option.

## Provenance

The wrapper's *shape* — a pixel buffer accessor, a mono-fast swap for live ink,
a mono-quality swap, a colour swap, and a raw escape hatch taking mode, full
flag and content type — follows [quill](https://github.com/MaximeRivest/quill),
MIT, which wraps the same library for the same reason and arrived at it
independently. Borrowing an interface shape from an MIT-licensed project is
fine; this paragraph is the record. **No Quill code is copied.**
`libqsgepaper.so` itself is proprietary and is never committed to this
repository; it is copied off the tablet into a directory outside the tree and
pointed at by `PAPERCLIP_VENDOR_LIB_DIR`.

## What is proven, and what is not

**Proven on the Mac**, by tests in `platform/device`:

- swap-rectangle arithmetic, including outward rounding and panel clamping;
- the ARGB8888 conversion, and that the device fill is opaque where the desktop
  fill is not — same rasteriser, one byte different;
- pen and touch coordinate transforms, and the 65/12 relationship between the
  two digitizer spaces;
- the evdev state machines: multitouch protocol B with ten slots, per-slot
  lift, and `SYN_DROPPED` cancelling every live contact rather than leaving a
  stuck finger;
- node resolution by advertised capability, and refusing to guess when two
  nodes claim the same role;
- the wakelock releasing on every exit path including drop;
- advisory-lock parsing against the exact bytes the tablet wrote, and that the
  description leaks neither the machine id nor the boot id;
- that `ep_abi.hpp` generates exactly the recorded vendor signatures.

**Not proven — every one of these is an open hardware gate:**

| Gate | Status | What it blocks |
|---|---|---|
| The declared symbols are exported by the real library | **closed** — all seven present | linking at all |
| The bridge compiles and links against `libqsgepaper.so` | **closed** | everything below |
| Whether `EPFramebuffer` needs a live `QGuiApplication` | **closed** — a bare `QCoreApplication` suffices; without one it segfaults | whether the primary path is simpler than the fallback |
| Whether the vendor can kill the process past our error handling | **closed, and it can** — `abort()` on init failure, guarded by `preflight()` | the honesty of the no-exception contract |
| Which waveform table the engine loads | **closed** — panel-specific, selected by lot/TFT; not the `ct33_*` files | a guard that wrongly refuses another panel |
| The engine opens and presents on the real tablet | **closed** — three round trips, stock healthy after each | the stage |
| The `EPScreenMode` encoding | **closed, and it was wrong** — mode alone, no content bit | every colour swap |
| **That what reaches the glass is correct** | **open** — nothing has seen the panel | §18 item 2 |
| Panel settle time, as against API call latency | **open** — connect `framebufferUpdated` or use a camera | the §9 latency budget |
| Input during a session | **open** — needs a finger on the glass | the input half of the round trip |
| Ghosting, memory, CPU, behaviour across a real suspend | **open** — the tablet was on charge, so it never suspended | the §9 baseline |
| A Paperclip surface is legible on the glass | **open** | §18 item 2, the stage's whole point |
| Which tuple element the engine presents from | **guessed** in `paperclip_ep_open` | drawing landing on the wrong page |
| The `EPScreenMode` / `UpdateFlag` / `GhostControlMode` numeric values | **guessed** | wrong waveform, silently |
| Mono 0 / mono 3 / colour 4 being the right waveforms | **proposed** | latency and legibility |
| Aux buffer byte order `B, G, R, 0xFF` | **inferred** from quill, not measured | inverted colour |
| The measured digitizer-to-panel offset, and orientation | **open** (WWW-21) | every touch target |
| Refresh timing, ghosting, switch latency, memory, CPU | **unmeasured** | §9's performance baseline |
| Behaviour across a real suspend while holding the display | **open** | the wakelock's whole justification |

A passing test in this repository is not device qualification. A build is not a
screen.

## What would make this wrong

- **The C++ ABI proves unstable across firmware.** ADR-0007's fallback stands:
  the `epaper` QPA plugin fed ARGB8888, as rmweb does. Simpler, less waveform
  control. `check-abi.sh` is the detector.
- **`EPFramebuffer` turns out to need a live `QGuiApplication`.** The bridge
  assumes it does not. If it does, the fallback becomes the primary path,
  because at that point we are running Qt anyway.
- **The main-thread assertion turns out to be advisory.** Then the thread
  affinity rule could relax — but it would still be the right default, because
  nothing above the adapter benefits from presenting off the main thread.
