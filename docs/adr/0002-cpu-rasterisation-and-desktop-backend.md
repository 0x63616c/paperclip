# ADR-0002 — CPU rasterisation, and where the desktop backend lives

**Status:** accepted (Stage 1, WWW-2). The device half is provisional until WWW-1 reports.

## Context

§15 asks for a desktop window at the target resolution with correct input
mapping, a drawn chess board and a home screen. The renderer chosen here is
the one apps will be written against, so it outlives the experiment even
though the experiment is all that exists today.

The device is an e-ink tablet. E-ink panels are driven by handing them a
framebuffer. The project's standing constraints say not to assume the Paper Pro
exposes an older reMarkable framebuffer interface, and not to treat rendering
on the Mac as proof the tablet can present it.

## Decision

Draw with **`tiny-skia`**, a pure-Rust CPU rasteriser, into a `Pixmap` wrapped
by `paper_sdk::Canvas`. Present on the Mac with **`winit`** for the window and
**`softbuffer`** for the pixel buffer.

No GPU, no driver, no shader pipeline, on either side.

The desktop backend lives in `platform/sdk` behind a non-default `desktop`
feature, not in a separate crate. §7 permits this explicitly for Stage 1 and
says to extract it only when build separation demands it.

Scaling from canvas to window is **box-filtered**, not nearest-neighbour. The
preview normally runs well below 1:1 on a laptop, and point-sampling a stroke
font at 0.35× drops whole strokes — the preview would then lie about
legibility, which is the one thing it exists to tell the truth about.

## Consequences

- The same drawing code produces the same pixels on the Mac and, if A3 holds,
  on the device. No "works in the simulator" class of bug.
- `cargo test` can render a whole screen and assert on it, because rendering
  needs no display. Several tests do exactly that.
- Screenshots are a normal function call rather than a screen-capture tool.
- Performance ceiling is CPU-bound. At 1620 × 2160 a full screen render is
  well inside a frame, and an e-ink refresh is orders of magnitude slower than
  the drawing anyway.
- `winit` and `softbuffer` are optional dependencies that a device build never
  compiles.

## What would make this wrong

WWW-1 finding that the Paper Pro will only accept GPU-composited output. The
`Canvas` interface would survive — everything above it draws through that — but
its backing would change, and this ADR would be superseded rather than amended.
