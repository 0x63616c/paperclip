# ADR-0034 — Gesture arbitration: the escape pinch, edge swipes, and app-vs-system

**Status:** accepted (WWW-52, WWW-80)

## Context

WWW-52 names gesture routing as one of the compositor's deliverables: "the
escape pinch and any edge gestures are the system's; everything else passes
through to the focused app," with the arbitration rule — "who wins between a
long-press and a swipe, and between app and system" — to be "decide\[d\] once
and write\[ten\] down." It cited generalising an existing `input/pinch.rs` and
a duplicated `PinchDetector::new()` as the starting point.

That citation does not describe this repository. WWW-77's description
already recorded the correction (`grep -rn PinchDetector` and
`find -iname '*pinch*'` both return nothing across the tree, and
`tools/paperctl/src/watch.rs` does not exist — `run.rs` is the file and has
no pinch logic in it), and this ticket reconfirmed it before writing any
code. There is no prior pinch or gesture code anywhere in this tree to
generalise; this ADR is a fresh design, not a consolidation, per WWW-80's own
instruction.

Two facts shaped the design more than anything else:

- **`PointerEvent` (`platform/protocol/src/input.rs`) carries no
  timestamp.** Only `at`, `phase`, `pointer`, `contact`, and the optional pen
  axes. So "long-press vs swipe" cannot be a dwell-time question — there is
  no wall-clock or event-count-since-down to measure a dwell against without
  inventing a fact the wire does not carry.
- **WWW-77 (ADR-0033) is the compositor core; WWW-78 (the accept loop and
  event-loop wiring) has not landed.** So there is nothing running yet to
  feed this detector a live stream, and nothing to demonstrate on hardware.
  WWW-80's own description anticipated exactly this: "the detector logic
  itself (pure function, pointer events in, verdict out) can be built and
  tested independently" of the wiring. This ADR is scoped to that pure
  detector alone.

## Decision

### The detector is spatial, not temporal

Because there is no timestamp on the wire, every rule here is about *where a
contact began and how far it has moved*, never *how long anything took*.
This resolves "long-press vs swipe" directly: a long press is simply
whatever a contact does that never crosses a movement threshold. There is no
separate long-press detector to arbitrate against a swipe detector — one
movement-distance check *is* the arbitration. A contact sitting still near an
edge for one event or for a thousand is identically `Verdict::App` right up
until it crosses `EDGE_SWIPE_MIN_TRAVEL`, and irrevocably `Verdict::System`
the instant it does.

### App vs system: the system only ever preempts, never claims a first event

A contact's first event is always a `Down` with no movement yet, so nothing
can confirm a system gesture on it. The default is therefore always
`Verdict::App`; `Verdict::System` only appears once a gesture confirms on a
later event for the same contact. A consumer that has already forwarded a
contact's early events to the app (WWW-78's job, not this one's) must treat
a later `System` verdict for that same contact as an implicit cancel — this
detector classifies events, it does not rewind or rewrite ones already sent.
Documented in `platform/compositor/src/gesture.rs`'s module docs as the
handoff contract for whoever wires this in.

### Two system gestures, two independent confirmation rules

- **`SystemGesture::EdgeSwipe(Edge)`** — a contact that began within
  `EDGE_ZONE_DEPTH` (48 canvas px) of exactly one of the panel's four edges,
  and has since travelled at least `EDGE_SWIPE_MIN_TRAVEL` (96 px) inward,
  measured along that edge's inward normal alone. Lateral travel along the
  edge does not count — a finger dragged sideways just inside the zone never
  confirms a swipe, because inward distance is the only quantity compared to
  the threshold.
- **`SystemGesture::Escape`** — exactly two live, unclaimed touch contacts
  (`Pointer::Touch`; the pen and the desktop preview's mouse never
  participate) whose separation has closed by at least `PINCH_MIN_CLOSING`
  (150 px) since both were down. A third live, unclaimed touch contact rules
  pinch recognition out entirely rather than guessing which two of several
  fingers the user meant — v1's escape gesture is defined as two fingers,
  full stop.

### A corner claims no single edge

A contact starting within `EDGE_ZONE_DEPTH` of two edges at once (a corner)
resolves to no edge at all, not an arbitrary pick of one. `edge_of`
(`platform/compositor/src/gesture.rs`) returns `None` for that case
deliberately — a corner start is exactly the input a poorly-chosen tie-break
would misroute, and refusing to guess is cheaper than being wrong half the
time.

### Where it lives, and what it is not wired to

`platform/compositor::gesture` — a third module alongside `pool` and
`surface`, with no dependency on either and none on `paper_sdk` (it takes
the panel's `Size` as a constructor argument rather than importing
`paper_sdk::SCREEN`, keeping the compositor crate's existing dependency
shape: `paper-protocol` and, on Unix, `libc` — see `platform/compositor/
Cargo.toml`). `GestureDetector::arbitrate` is the entire public surface: one
struct, pointer event in, `Verdict` out, no I/O, no `unsafe`, no clock. It is
not wired into a socket, an accept loop, `HostMessage`/`AppMessage`, or
anything on the actual event path — none of those exist yet (WWW-78). Nor is
it demonstrated on hardware: there is nothing running to demonstrate it
against. Both are explicitly out of scope here and named as such so this is
not read as more than it is.

## What it costs

- **No real long-press.** An app that wants an actual dwell-based long press
  (as opposed to "didn't move much") has to build that itself from repeated
  `Moved` events at whatever cadence the input reader produces them —
  arguably correct, since only the app knows what duration threshold matters
  to it, but a real limitation if a future system-level long-press gesture
  is ever wanted; there is no time axis here to hang one on.
- **A trailing pinch contact lags one event.** When the pinch-closing
  contact's own event confirms `Escape`, its partner contact is marked
  claimed in the same call, but the partner's *own* verdict only reads as
  `System` on its own next event — there is no event in flight for the
  partner to relabel retroactively. `two_fingers_closing_together_is_the_
  escape_pinch` (`platform/compositor/src/gesture.rs`) pins this down rather
  than hiding it.
- **Fixed pixel constants, not measured ones.** `EDGE_ZONE_DEPTH` (48),
  `EDGE_SWIPE_MIN_TRAVEL` (96) and `PINCH_MIN_CLOSING` (150) are reasonable
  guesses against the 1620×2160 panel, not numbers pulled from a finger on
  glass. Per the project's decision vocabulary this is a **proposed**
  implementation choice, not evidence of anything about the hardware or the
  gesture actually feeling right.

## What would make this wrong

- **If a future ticket needs true duration-based gestures** (a dwell-based
  long-press, a hold-to-confirm), the wire shape itself has to change first
  — `PointerEvent` would need a timestamp, which is a protocol change with
  its own version bump, not something this detector can retrofit alone.
- **If WWW-78's event loop finds the "implicit cancel on late claim"
  contract awkward to implement** — for instance, if forwarding a `Cancelled`
  synthetically to an app that already saw `Down`/`Moved` turns out to
  conflict with `Session`'s own lifecycle rules (`platform/protocol/src/
  session.rs`) — the preemption rule above is the piece to revisit, not the
  confirmation thresholds.
- **If real fingers on the actual panel confirm or reject at the wrong
  moment** once WWW-78 exists to wire this into a live stream and someone
  can try it on the glass. Nothing here has run against real touch data;
  the three pixel constants are the first thing to tune against that
  evidence, not the detection logic they feed.
