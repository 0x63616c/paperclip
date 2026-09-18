# ADR-0035 — System chrome: a drawing-order guarantee, not a permission check

**Status:** accepted (WWW-79)

## Context

WWW-52 asks for system chrome — status bar, clock, battery, Wi-Fi — drawn by
the compositor itself, outside any app's surface, such that "an app cannot
draw over it or remove it." WWW-79 splits this out as a stage-2 sibling of
WWW-78 (crash/hang isolation), both depending only on WWW-77 (ADR-0033):
the pending/current commit model has to exist before there is a layer to
draw chrome on top of.

Two things ADR-0033 named explicitly as still out of scope apply here too:
there is no socket (WWW-78) and no real client running against the
compositor yet (WWW-81, staged behind WWW-78). There is therefore nothing
today that actually assembles a panel-sized frame from a client's committed
surface — that assembly is later work. What this ticket can build now is the
part that does not depend on any of that: the reserved geometry, the state
those elements render from, and the render call itself, all pure and
testable in isolation, the same split ADR-0033 made for `Surface` ahead of
WWW-78's event loop.

## Decision

### Enforcement is a drawing-order guarantee, not a check

`platform/compositor::chrome::draw` does not ask whether a client is allowed
to draw in the reserved rectangle, check any coordinate, or reject anything.
It reuses `paper_sdk::chrome::draw_status_bar`, which fills the whole
reserved rectangle with an opaque background before drawing the clock and
the battery/network line into it. Calling `draw` after a client's surface has
already been blitted into the same canvas therefore leaves nothing of the
client's pixels in that rectangle — not because a permission was checked and
denied, but because there is no code path by which a byte painted earlier
survives an unconditional full-rectangle fill painted later.

This is deliberately not a coordinate check on client damage. WWW-77's
`Surface::commit` already validates damage against a client's own `extent`
(`platform/compositor/src/surface.rs`); reducing that extent so it excludes
the chrome band is a real option, but it is a decision about how a client's
surface is *shaped*, which belongs with whatever WWW-78/WWW-81 wires an
actual client onto the compositor's socket — not with this ticket, which has
no client to shape a surface for yet. `chrome::draw`'s guarantee holds
regardless of that later decision: however big or small a client's surface
ends up being, whatever the compositor draws in the reserved rectangle after
compositing that surface is what appears there.

### `ChromeState` reuses the `SystemQuery`/`SystemEvent` vocabulary, not a new backend

`platform/compositor::chrome::ChromeState` holds an `Option<TimeFact>`,
`Option<BatteryFact>` and `Option<NetworkFact>` — the exact types
`platform/protocol::system` already defines (WWW-50, ADR-0028) — filed via
`apply_time`/`apply_battery`/`apply_network`/`apply_event`. This follows the
precedent `apps/home/src/app.rs`'s `HomeApp::apply_battery` already set: a
fact arrives from outside and is filed; nothing re-reads
`/sys/class/power_supply` or `nmcli` a second time to produce it. The
existing responder for those facts, `tools/paperctl::system::SystemResponder`,
stays exactly where it is — it answers an app's `SystemQuery` over a wire
`paperctl`'s `Session` speaks, which the compositor does not implement yet
and which is a different question from what chrome renders once it has an
answer.

Time is not part of `SystemEvent` (see the module doc on
`paper_protocol::system`: a clock ticking is not a change worth an app
redrawing over), so `ChromeState::apply_time` exists separately from
`apply_event`, for whatever later polls a `SystemQuery::Time` answer on some
interval.

### The clock renders in UTC, and says so

`TimeFact` carries milliseconds since the Unix epoch and nothing else — no
offset, no zone name. Nothing in this workspace resolves a timezone today;
`platform/sys/src/wallclock.rs`'s `SystemWallClock` reads `SystemTime`, which
has none either. Rather than invent a timezone source for this ticket alone,
`chrome::draw` renders `HH:MM UTC` and says so in the label, honest about
what it is rather than silently wrong about local time.

### The reserved height reuses `paper_sdk::chrome::STATUS_BAR_HEIGHT`

`platform/compositor::chrome::reserved_rect` is `paper_sdk::chrome`'s own
`STATUS_BAR_HEIGHT` at the top of the panel, not a second constant for the
same number. `content_rect` is `paper_sdk::chrome::status_bar_content_area`
applied to the whole panel, for the same reason `paper_sdk::chrome` itself
splits `status_bar_content_area` out from `draw_status_bar`: a caller that
needs the geometry without drawing anything gets it without a `Canvas`.

## What stays out of this stage

- **No socket, no real client, no frame assembly.** Nothing here reads a
  `Surface::current()` or a `Pool` slot and blits it into a panel-sized
  canvas before calling `chrome::draw` on top — that assembly is WWW-78's
  event loop and WWW-81's client migration, in whichever order they land.
  `chrome::draw`'s contract (fill-then-overwrite, called last) is what that
  code will rely on; it does not exist yet.
- **No live facts.** Nothing populates a real `ChromeState` from
  `paper_sys`'s backends or a running `SystemResponder`. The tests in
  `platform/compositor/src/chrome.rs` construct `ChromeState` directly.
- **No reduction of a client's own surface extent** to exclude the chrome
  band — see "Enforcement is a drawing-order guarantee" above for why that
  is a separate decision for later work, not a gap in this one.
- **No local time / timezone resolution.**

## What it costs

- Two places now know `STATUS_BAR_HEIGHT` matters for layout — an app's own
  `paper_sdk::chrome` and the compositor's reserved rectangle — coupled
  through a shared constant rather than independently chosen. If an app's
  in-surface status bar and the compositor's system chrome ever need
  different heights, that coupling has to be deliberately broken, not
  silently drifted apart.
- The clock is UTC-only until something in this workspace resolves a
  timezone. On the one device this project targets, that is very likely
  wrong for the user reading it, not merely imprecise.

## What would make this wrong

- If WWW-78/WWW-81's frame assembly finds it needs to call `chrome::draw`
  somewhere other than strictly last (for example, an overlay above chrome
  itself, like WWW-82's lock screen) — the ordering contract this ADR relies
  on would need restating, not just a call-site change.
- If a real client's surface is later given an extent that already excludes
  the chrome band, `chrome::draw`'s full-rectangle overwrite becomes
  redundant defence-in-depth rather than the only mechanism — worth keeping
  either way, but the module doc's claim that it is "the entire enforcement
  mechanism" would need updating.
- If timezone data becomes available on the tablet (WWW-1 or a later
  hardware gate), UTC-only becomes a defect to fix rather than an honest
  limitation to document.
