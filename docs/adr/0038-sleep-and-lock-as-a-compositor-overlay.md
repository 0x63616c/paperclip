# ADR-0038 — Sleep and lock as a compositor overlay, not a session hack

**Status:** accepted (WWW-52, WWW-82)

## Context

WWW-52 item 6 asks for "sleep and lock as compositor layers," explicit that
"the sleep cover stops being a session hack," and for "a basic lock screen —
enough to see it working on the glass, not a designed product." It also names
what this supersedes: WWW-21, already `cancelled` — its touch-transform half
shipped elsewhere, and its sleep/lock/PIN device gate was dropped rather than
built as an interim path. There is no shipped `SleepScreenPath` to migrate
away from; this is a clean build on the compositor WWW-77/WWW-78 already
established.

WWW-82 is blocked by WWW-77 (compositor core, ADR-0033) and WWW-79 (system
chrome, ADR-0035) for the reason both name: sleep/lock reuses the same "a
layer drawn on top of client surfaces, an app can't touch it" mechanism chrome
uses. By the time this ticket started, WWW-78 (the accept loop, ADR-0037) had
also landed — unlike WWW-77 and WWW-79, this ticket's `Compositor` already has
a real socket and real clients to test against, not just `MemoryPanel` and
direct calls.

ADR-0035 already named this ticket by number, as a forward reference: "If
WWW-78/WWW-81's frame assembly finds it needs to call `chrome::draw` somewhere
other than strictly last (for example, an overlay above chrome itself, like
WWW-82's lock screen) — the ordering contract this ADR relies on would need
restating." That is the ordering this ADR fixes: sleep/lock sits above
everything, chrome included.

## Decision

### A synthesised frame, styled the opposite way round from `stopped`

`platform/compositor::sleep::render_lock_frame` follows `stopped.rs`'s exact
shape — a fresh panel-sized `Canvas`, drawn without ever reading a client's
committed pixels, presented with `Refresh::Full` because there is no prior
frame this layer trusts enough to diff against. It differs in one deliberate
way: ink background, paper text, the inverse of `render_stopped_frame`'s
paper-on-ink "`<app> stopped`" message — so the two are distinguishable at a
glance, not just by the word printed on them (`sleep.rs`'s
`the_background_is_ink_not_paper` pins this down).

### `Compositor::sleep`/`wake` stop presenting without disturbing state

Unlike WWW-77 and WWW-79, this ticket's `Compositor` already has a socket and
real clients (WWW-78 landed first), so the lock/unlock transition is built
directly into `Compositor` rather than left as a seam later work drives:

- `Compositor::sleep` draws the lock frame once and sets an internal `locked`
  flag; calling it again while already locked is a no-op, not a redundant
  full refresh.
- `Compositor::present_foreground` checks that flag before every blit. While
  set, it still releases the foreground client's committed slot and still
  sends `HostEvent::Released` — nothing reads that slot's bytes on this path,
  so releasing without presenting is safe — so a client that keeps committing
  while locked does not stall on its two-buffer pool waiting for a
  presentation that will not happen until `wake`. `Surface`/`Pool` state
  advances exactly as it would unlocked.
- `Compositor::wake` clears the flag and re-presents whatever `self.foreground`
  says is current: the client's latest committed frame (which may be newer
  than what was current when `sleep` was called), or a re-rendered stopped
  frame if the foreground client died while locked.
- `Compositor::disconnect` — a foreground client crashing — records the
  `Foreground::Stopped` transition and its `CompositorEvent::ForegroundStopped`
  immediately either way, but paints `render_stopped_frame` only when not
  locked. A crash while locked must not show through the lock screen; `wake`
  is what paints the stopped frame once it is safe to.

This is the same "state advances, presentation is what's gated" split
`Surface`'s pending/current model already uses (ADR-0033) — sleep/lock adds a
second gate in front of the same presentation call, rather than a parallel
path with its own rules.

### What triggers `sleep`/`wake` is not this ticket's job

Same split ADR-0033 made for `Surface` ahead of WWW-78's event loop, and
ADR-0035 made for `chrome` ahead of a real client: there is no idle timer, no
power button, and no real device suspend signal calling `Compositor::sleep`
here. `sleep`/`wake` are public methods a caller — a future host process, or a
test — calls directly. Proven end to end in
`platform/compositor/tests/sleep_and_lock.rs` against a real `UnixListener`
and real client peers, the same style `crash_and_hang_isolation.rs` (WWW-78)
established, rather than against `MemoryPanel` alone.

### No PIN, no authentication clearing the lock

WWW-21's sleep/lock/PIN device gate was dropped, not built as an interim path
to replace. WWW-52 asks for "basic" here. `wake` clears the lock
unconditionally; there is no code path that checks anything before clearing
it. A gesture, a button, or a PIN gating `wake` itself is a decision for
whichever ticket wires a real trigger on — this one has no trigger to gate.

## What stays out of this stage

- **No idle timer, power button, or device suspend signal.** See above.
- **No PIN or other authentication.** See above.
- **No interaction with `chrome::draw`.** Chrome is not wired into frame
  assembly yet (ADR-0035, ADR-0037's "what I did not do") — there is no
  composited canvas today for the lock layer to sit above. The ordering this
  ADR decides (lock above chrome above a client's surface) is a contract for
  whichever ticket builds that assembly, the same way ADR-0035's own
  fill-then-overwrite contract was written before a caller existed to rely on
  it.
- **Nothing about the confirmed "save-and-return-to-stock" decision for a real
  device sleep** (project description) is implemented here. That is a
  session-level question — what happens to app processes and to stock Xochitl
  when the tablet actually suspends — separate from what the compositor draws
  while it is still the process holding the panel. This ticket is the latter
  only.

## What is proven, and how

- `platform/compositor/src/sleep.rs`: `render_lock_frame` paints a non-blank,
  full-refresh frame; its background is `palette::INK`, not `palette::PAPER`;
  a zero-sized panel is refused rather than panicking.
- `platform/compositor/tests/sleep_and_lock.rs`, against a real socket and
  real client connections:
  - Sleeping draws the lock frame over a running app's presented pixels.
  - A frame a client commits while locked never reaches the panel — no
    `FramePresented` event fires — until `wake`, which then presents exactly
    that (newer) frame rather than the one current when `sleep` was called.
  - `wake` while not locked is a no-op.
  - A foreground client crashing while locked does not reveal a stopped frame
    through the lock screen; the panel stays locked until `wake`, which then
    shows Home (the existing crash-recovery path, undisturbed).
  - Calling `sleep` twice in a row does not redraw.
- Every added assertion was confirmed to fail against the code without this
  ticket's `present_foreground`/`disconnect` guards before being left in the
  suite green.

## What would make this wrong

- **If a real client's surface is later composited underneath chrome in one
  canvas** (WWW-78/WWW-81's still-pending frame assembly), the lock layer
  needs to be the last thing drawn into that same canvas, not a separate
  panel write racing it. Nothing here assumes a particular assembly order
  beyond "sleep/lock is last"; if the assembly turns out to need the lock
  frame composited rather than presented standalone, this ADR's separation
  between "draws its own frame" and "chrome/client compositing" would need
  restating.
- **If a real sleep trigger needs to know whether it is safe to release a
  buffer without presenting it** — for example, if a future client expects
  release to mean "the host has definitely displayed this," not merely "the
  host will never read this slot again" — the release-without-presenting
  choice in `present_foreground` would need revisiting. Nothing in the current
  protocol distinguishes the two meanings.
