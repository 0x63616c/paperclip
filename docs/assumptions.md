# Provisional assumptions

Stage 1 (WWW-2) ran in parallel with the device survey (WWW-1) by Calum's
explicit decision. Nothing in Stage 1 needs the tablet, but several choices
are bets on what the tablet turns out to be. They are listed here so the
Stage 2 review gate can check them against WWW-1's report rather than hunt
for them in the code.

Each entry names where the assumption is encoded, so a wrong answer has one
place to change.

## Verdicts at the Stage 2 review gate (WWW-10)

Checked against the WWW-1 gate report and the WWW-20 device session. A verdict
here means "the device said so", not "the code still compiles".

| # | Assumption | Encoded at | Verdict | Evidence |
|---|---|---|---|---|
| A1 | The panel is 1620 × 2160, portrait. | `paper_sdk::SCREEN` (`platform/sdk/src/display.rs`) | **confirmed** | Device tree `display-width` = 1620, `display-height` = 2160 (WWW-1 §1). |
| A2 | The panel is around 230 px/inch, so 120 canvas px is a comfortable finger target. | `paper_sdk::chrome::MIN_TOUCH_TARGET` | **confirmed, corrected to 228** | Device tree `display-dpi` = 228, not the ~230 estimated from published dimensions. 120 px = 13.4 mm; the conclusion is unchanged and the constant does not move. |
| A3 | The device presents a CPU-produced framebuffer, so software rasterisation is the right renderer. | ADR-0002; `paper_sdk::Canvas` over `tiny-skia` | **confirmed, with a caveat** | No `libEGL`/`libGLESv2`/`libgbm`/Mesa anywhere on the device; `libepaper.so` drives `QPlatformBackingStore`, a software raster path (WWW-1 §3). A CPU pixel buffer is exactly the currency the display stack wants. **Caveat:** it is not *this* buffer. The DRM dumb buffer is 1620 bytes × 1084 rows for 1620 × 2160 pixels — 4 bits per pixel, two panel rows per buffer row (WWW-20). The adapter owes a packing and quantisation step; the exact packing is an open gate. |
| A4 | A greyscale UI is the safe default; colour is a bonus, not a dependency. | ADR-0005; `paper_sdk::palette` | **confirmed** | The panel is ACeP colour (`EPFramebufferAcep2` inside `libepaper.so`), but the framebuffer arithmetic above implies 16 levels, and waveform selection is vendor-owned. Designing for grey first was right. See the note under "What the gate reports added" about palette separation at 16 levels. |
| A5 | Pointer input arrives as press / move / release with a position. | `paper_sdk::PointerEvent` | **refuted as sufficient** | The device reports considerably more: pen `ABS_PRESSURE` 0–4096, `ABS_DISTANCE` (hover), `ABS_TILT_X/Y` ±9000, `BTN_TOOL_RUBBER` for the eraser, and touch as multitouch protocol B with **10 simultaneous slots** (WWW-1 §3). `PointerEvent` as it stands cannot represent a second finger at all. It is now `#[non_exhaustive]` so WWW-5 can add pressure, tilt and a per-contact id without breaking every call site; the type itself is WWW-5's to design. |
| A6 | The device target triple is not yet known. | Deliberately **not** encoded — `rust-toolchain.toml` installs no cross target. | **refuted — it is known now** | `aarch64-unknown-linux-gnu`, glibc (`ld-linux-aarch64.so.1`, `libc.so.6`), Qt 6.10.3, `libstdc++.so.6` (WWW-1, "Downstream stages"). Deliberately still not added to `rust-toolchain.toml`: nothing cross-compiles yet, and a target no build exercises is configuration ahead of functionality. WWW-3 adds it with the first cross build that proves it. |
| A7 | Stock Xochitl handoff will be reachable from the home screen. | The "Return to stock" shelf tile draws but does nothing. | **open, and now load-bearing** | Taking the display means stopping `xochitl.service`, and Xochitl must run before `/home` mounts because it unlocks dm-crypt via `pincode-rs` (WWW-1 §5, WWW-20). So "return to stock" is a real recovery operation, not a cosmetic tile. WWW-3 owns it. Until then the tile is a placeholder that looks identical to a working one — `ShelfEntry` has no way to say "this does nothing". |

## What the gate reports added

Facts established after Stage 1 was written that Stage 1's code does not yet
account for. None of them is a Stage 1 defect; they are the inputs WWW-3 and
WWW-5 must design against.

- **The framebuffer is packed, not linear.** `Canvas` produces 1620 × 2160
  RGBA. The panel wants 4 bits per pixel with two panel rows per framebuffer
  row. The conversion has no owner yet.
- **Grey levels are scarce.** If 4 bpp holds, there are 16 of them. The
  palette's light end — `PAPER` (0xF7), `TILE` (0xEC), `BOARD_LIGHT` (0xE4) —
  separates by roughly one level once quantised, and nothing tests that it
  stays separable. Worth a test once the packing is identified rather than
  before, since the quantisation curve is not yet known.
- **Suspend is continuous, not an event.** The tablet suspends roughly every
  two minutes under stock, and `/sys/power/state` returns `EBUSY` — Paperclip
  cannot request or refuse a suspend except by holding a wakelock on
  `/sys/power/wake_lock` (WWW-20). Every resume costs ~77 ms of kernel FPGA
  bridge reprogramming before the panel can be presented to.
- **Display ownership is more than DRM master.** Xochitl also holds
  `/tmp/epd.lock` and `/tmp/epframebuffer.lock`, the latter a registry of
  PID/process/hostname/machine-id/boot-id. The Host must participate.

## What Stage 1 does not establish

None of the following is touched by anything in this repository, and no test
here should be read as evidence about them:

- Whether the Paper Pro can present a framebuffer Paperclip produces at all.
  WWW-20 established that a non-Qt process can drive the panel visibly, but
  the pixel packing that makes an image legible is still unidentified.
- E-ink refresh behaviour, ghosting, or perceived latency.
- Pen pressure, tilt, palm rejection, or input latency.
- The touch and pen coordinate transforms. Pen (11180 × 15340) and touch
  (2064 × 2832) are the same space at a fixed 65/12 scale, and neither matches
  the panel's aspect — the digitizer's active area is taller than the glass, so
  the transform needs a measured offset. `DisplayMapping` handles a uniform
  centred fit and nothing else; it is not that transform.
- Colour rendering.
- Whether stock Xochitl can be reliably restored after a custom session.
  WWW-20 restored it repeatedly after short holds; that is not the same as
  after a crash, a reboot mid-session, or a suspend while holding the display.

The desktop preview simulates a pen with a mouse. That proves coordinates are
plumbed correctly and nothing else.
