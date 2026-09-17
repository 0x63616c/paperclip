# ADR-0005 — A greyscale-first palette

**Status:** accepted (Stage 1, WWW-2)

## Context

The reMarkable Paper Pro has a colour e-ink panel. How it actually renders
colour — saturation, refresh cost, whether a given hue is distinguishable at
UI sizes — is a hardware gate owned by WWW-1, which had not reported when
Stage 1 was drawn.

The project's standing constraints are explicit that rendering on the Mac is
not evidence about the tablet.

## Decision

`paper_sdk::palette` is greyscale. Ten values: paper, three inks, a hairline,
a tile fill, two board squares, an emphasis fill, and a letterbox grey that is
preview chrome rather than part of any screen.

No screen carries meaning in colour alone. Selection is a double-stroked ring,
emphasis is a filled shape with inverted text, board squares differ in
luminance.

Contrast is asserted in tests, not eyeballed: body text clears 7:1 against the
background, primary ink clears 14:1, and the dark board square is deliberately
*balanced* at roughly 4:1 against both ink and paper rather than pushed toward
either end.

## Consequences

- The UI is correct if the panel turns out to render these as plain greys,
  which on e-ink is the outcome to design for first.
- Reviewing a screenshot is about layout and weight, not about whether a colour
  looks right on a laptop display that is nothing like the target.
- Adding colour later is additive. Nothing needs redrawing, because nothing
  currently depends on hue.
- **Cost:** the screens are austere. On a paper-like device that reads as
  intentional; the judgement should be revisited once one has been seen on
  glass.

## What would make this wrong

WWW-1 reporting that colour is cheap, stable and legible at UI sizes. Then
colour becomes available — as reinforcement for something already distinguished
by luminance, not as the only signal.
