# Provisional assumptions

Stage 1 (WWW-2) ran in parallel with the device survey (WWW-1) by Calum's
explicit decision. Nothing in Stage 1 needs the tablet, but several choices
are bets on what the tablet turns out to be. They are listed here so the
Stage 2 review gate can check them against WWW-1's report rather than hunt
for them in the code.

Each entry names where the assumption is encoded, so a wrong answer has one
place to change.

| # | Assumption | Encoded at | If WWW-1 says otherwise |
|---|---|---|---|
| A1 | The panel is 1620 × 2160, portrait. | `paper_sdk::SCREEN` (`platform/sdk/src/display.rs`) | Change the constant. Nothing else hard-codes the numbers; layouts are derived and tests assert relationships, not pixel positions. |
| A2 | The panel is around 230 px/inch, so 120 canvas px is a comfortable finger target. | `paper_sdk::chrome::MIN_TOUCH_TARGET` | Change the constant. Chess squares (175 px) and shelf tiles (734 × 500) have headroom; the footer action row is the tightest at 132 px and would need re-checking. |
| A3 | The device presents a CPU-produced framebuffer, so software rasterisation is the right renderer. | ADR-0002; `paper_sdk::Canvas` over `tiny-skia` | If the device only accepts GPU-composited output, the `Canvas` interface survives but its backing changes. Drawing code above it does not move. |
| A4 | A greyscale UI is the safe default; colour is a bonus, not a dependency. | ADR-0005; `paper_sdk::palette` | If colour is confirmed and useful, add values to the palette. Nothing needs to be redrawn, because no screen currently carries meaning in colour alone. |
| A5 | Pointer input arrives as press / move / release with a position. | `paper_sdk::PointerEvent` | If the pen reports pressure, tilt or hover, the type gains fields. It deliberately has none today rather than fields nothing fills. |
| A6 | The device target triple is not yet known. | Deliberately **not** encoded — `rust-toolchain.toml` installs no cross target. | Add the target to `rust-toolchain.toml` once WWW-1 reports it. Adding one now would be a guess dressed as configuration. |
| A7 | Stock Xochitl handoff will be reachable from the home screen. | The "Return to stock" shelf tile draws but does nothing. | The tile is a placeholder for a real action (WWW-3). If handoff turns out to work differently, the tile changes or goes. |

## What Stage 1 does not establish

None of the following is touched by anything in this repository, and no test
here should be read as evidence about them:

- Whether the Paper Pro can present a framebuffer Paperclip produces at all.
- E-ink refresh behaviour, ghosting, or perceived latency.
- Pen pressure, tilt, palm rejection, or input latency.
- Colour rendering.
- Whether stock Xochitl can be reliably restored after a custom session.

The desktop preview simulates a pen with a mouse. That proves coordinates are
plumbed correctly and nothing else.
