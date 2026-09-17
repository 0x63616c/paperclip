# ADR-0008 — Input coordinate transforms are a hardware constant, not a calibration

**Status:** accepted (Stage 2, WWW-20)

## Context

WWW-20 planned a fiducial session: render marks at known panel coordinates,
have Calum tap them, fit a transform. Two attempts captured no usable data, and
a third was being planned.

Calum challenged the premise: stock Xochitl never asks anyone to calibrate. A
per-device measurement was the wrong model for a fixed digitizer bonded to a
fixed panel. He was right, and a search would have caught it before any of the
attempts.

## Decision

Take the transforms from prior art and treat them as constants.

```
touch:  screen_x = raw_x * 1620 / 2064      screen_y = raw_y * 2160 / 2832
pen:    screen_x = raw_x * 1620 / 11180     screen_y = raw_y * 2160 / 15340
```

No axis swap and no inversion on the stock firmware path.

## Evidence

Three independent lines agree:

- [rmweb's input research](https://github.com/exp78/rmweb/blob/master/docs/research/remarkable-touch-input.md)
  gives these formulas directly.
- KOReader's verified device table lists the same ratios for the Paper Pro,
  explicitly "no axis swap, no mirror".
- WWW-20's own arithmetic, derived before the search: `11180/2064` and
  `15340/2832` are both exactly `65/12`, so pen space is touch space scaled by
  65/12 and the two transforms compose consistently.

The third was derived independently and corroborates the first two, which is
why it is recorded rather than discarded.

## Consequences

- No calibration session, and no fiducial rendering, is needed for input.
- The transform is a pure scale. An earlier note claiming the digitizer's
  active area is taller than the glass and therefore needs a measured offset is
  **withdrawn**: the aspect mismatch is absorbed by the two axes having
  different scale factors, which is what these formulas express.
- `paper_device::PointerTransform` already implements the pure-scale version.
  Its `mirror_y` flag stays, defaulted off, for the one open item below.
- Identify the nodes by `EVIOCGNAME`, never by event number — the names are
  `Elan marker input` (pen) and `Elan touch input` (touch).

## What would make this wrong

Some firmware reportedly exposes a mainline-kernel input path with Y inverted.
Ruling it out costs one tap on a top-left mark: if it registers bottom-left,
set `mirror_y`. Deferred to WWW-21, which already needs a person at the tablet.
