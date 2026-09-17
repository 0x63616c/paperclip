# WWW-20 calibration attempt 2 — partial data, transform not yet derivable

Five-minute capture, 2026-09-17 03:03–03:08, on the hardened path (wakelock
held, start budget checked, clean restore). Both nodes recorded: 1129 pen
events, 580 touch events.

Real taps arrived this time, but not the requested ordered sequence. All
activity fell in the first ~20 seconds and then stopped.

Touch (`event3`, axis 0–2064 × 0–2832), nine contacts:

| # | t+s | x | y | looks like |
|---|---|---|---|---|
| 1 | 0.0 | 76 | 67 | top-left |
| 2 | 0.8 | 80 | 0 | top-left |
| 3 | 2.9 | 1134 | 1062 | mid |
| 4 | 14.4 | 2003 | 114 | top-right |
| 5 | 14.6 | 1975 | 116 | top-right |
| 6 | 14.8 | 1946 | 99 | top-right |
| 7 | 16.0 | 1716 | 2726 | lower-right |
| 8 | 20.1 | 33 | 673 | left edge |
| 9 | 20.3 | 199 | 1340 | left |

Pen (`event2`, axis 0–11180 × 0–15340), six tip contacts: two clustered at
(215, 363)/(353, 429) — top-left — and four clustered near (7850, 12250).

## What this does and does not establish

**Does:** the digitizer's reported range is physically reachable. Touch reached
x=33 and x=2003 against an axis maximum of 2064, and y=0 and y=2726 against
2832. So there is no large dead margin — the reported axis corresponds closely
to the visible glass.

**Does not:** an affine transform. Nine contacts whose intended targets are
unknown cannot be fitted to panel coordinates; guessing which tap meant which
corner would manufacture a transform rather than measure one.

## Change of protocol for the next attempt

Ordered discrete taps have now failed twice, both times on timing rather than
on anything physical. Replace them with a **single slow trace around the
perimeter of the screen** — one continuous drag, finger held down, following
the edge all the way round, then the same with the pen.

That is strictly better here:

- The bounding box of the trace gives min/max on both axes directly, which is
  the transform, with no dependence on tap ordering or counting.
- It is one instruction, not five, and it self-corrects: a longer trace simply
  gives more samples of the same edges.
- Straightness of each edge in digitizer space is itself the linearity check
  the affine fit assumes.

It also removes the failure mode that wasted both attempts: nothing has to
happen at a particular moment.
