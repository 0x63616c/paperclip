# ADR-0033 — The compositor core: pool descriptor, buffer lifecycle, and what stays out of this stage

**Status:** accepted (WWW-52, WWW-77). Foundation only — see "What stays out
of this stage."

## Context

WWW-52 asks for one long-lived process that owns the panel for as long as
Paperclip is up, imitating **tinywl** (wlroots' deliberate minimum) rather
than Weston. It names the mechanism that must exist from day one: a
shared-memory pool sized for two buffers per client, and explicit
`wl_buffer.release`-equivalent semantics — "the single pitfall that will
corrupt the panel" if skipped.

WWW-52 was too large for one change and said so itself ("This is large. If it
needs splitting, split it and say how"). It was split into six sibling
tickets across three stages; WWW-77 is stage 1, the foundation the other five
depend on. Its two acceptance criteria:

- A malformed damage rect is rejected without touching memory outside the
  pool.
- Buffer release is implemented, and a client writing before release is
  detected in tests.

## Prior art consulted

Per the project's "research before theorising" rule and WWW-52's own "Read
first" list:

- `swaywm/wlroots`'s `tinywl/tinywl.c` — the minimum viable compositor shape
  this ADR imitates: one process owns the output, clients get shm pools,
  `wl_buffer.release` gates reuse.
- `Eeems-Org/oxide`'s `blight` — a surface-based display server proven on
  this exact hardware family, cited for the surface lifecycle shape.
- `MaximeRivest/quill` — the takeover shape and no-hardware-alpha rule,
  already the basis for ADR-0007's presentation decision.

No third-party code is vendored; this ADR borrows the *lifecycle shape*
(pool → attach → damage → commit → release), which is Wayland's own public
protocol design, not any one implementation's source.

## Decision

### The pool descriptor is additive to `SurfaceDescriptor`, not a replacement

`ShmPoolDescriptor { buffer: SurfaceDescriptor }` (`platform/protocol/src/
message.rs`) describes two `BufferSlot`s (`A`, `B`) laid out back to back,
each the shape one `SurfaceDescriptor` already describes.
`Hello.surface` — the single fd-mapped area WWW-4's supervisor hands an app
today (ADR-0022) — is untouched: it is still struct-literal-constructed in
every existing entrypoint and test, and this ticket does not migrate any of
them onto the compositor (WWW-81 does, staged three tickets behind this one).
`ShmPoolDescriptor` is the shape a client's pool takes *once* it is a
compositor client instead — described now so `platform/compositor` has
something concrete to validate against, deliberately ahead of anything that
sends one over a wire.

Two slots, not a variable-sized pool: a client draws its next frame into
whichever slot the host is not currently reading, so drawing and presenting
never contend for the same bytes, and two is the minimum that makes that
true. `MAX_POOL_BYTES` (`platform/protocol/src/limits.rs`, 64 MiB — headroom
above two full 1620×2160 ARGB8888 buffers at ~14 MiB each) is checked before
any backing storage is sized or `mmap`ed, following the same pattern
`MAX_MESSAGE_BYTES` and `MAX_DAMAGE_RECTS` already set: a size claim costs a
comparison, not an allocation.

### No new `HostMessage`/`AppMessage` variants in this ticket

WWW-77's own description points at WWW-50's `SystemQuery`/`SystemAnswer`
(ADR-0028) as precedent for growing the closed `HostMessage`/`AppMessage`
enums deliberately. This ticket does not do that. `Session`
(`platform/protocol/src/session.rs`) is the state machine for one app's
*launch* lifecycle — `Hello` → `Ready` → `Draw`/`FrameDone` → `Goodbye` — and
its `on_app_message` match is exhaustive by design, so every existing
consumer (every app's `paper_sdk` link, `paperctl`'s `Session`) would have to
account for a new variant whether or not anything exercises it yet. Wiring
attach/commit/release into that enum only becomes meaningful once a real
client speaks it over the compositor's socket, which is WWW-78/WWW-81's job.
Adding unused variants now would be exactly the speculative-abstraction
WWW-77's own "Out of scope" section rules out ("migrating Home/Settings/App
Store/test-card/Xochitl onto the compositor").

Instead, `platform/compositor` defines its own internal lifecycle
(`Surface::attach`/`commit`/`release`), independent of `Session`. When WWW-81
lands, the two protocols may merge — or the compositor's socket may speak a
distinct wire format `paper_sdk` grows a client for — either is a decision to
make with a real client in hand, not before one exists.

### `pool` and `surface` are deliberately separate: one touches memory, one doesn't

`platform/compositor/src/pool.rs` is the only place in this crate with
`unsafe` or a system call: it owns an `mmap`ed `MAP_SHARED` region (following
`platform/device/src/vendor.rs`'s pattern — `#![allow(unsafe_code)]` at the
module, a written reason on every block) and hands out bounds-checked slices
into its two slots.

`platform/compositor/src/surface.rs` has no `unsafe` and touches no memory.
`Surface` is pure bookkeeping: which slot is `Free`/`Attached`/`Presenting`,
and whether a `Damage` claim's rectangles fit inside the surface's `extent`.
This is what makes the "without touching memory outside the pool" acceptance
criterion true by construction rather than by a runtime check reaching into
the mapping: `Surface::commit` validates purely arithmetically (`validate_rect`
checks `is_finite`, non-negative, and `x + width <= extent.width` before
ever comparing anything to a `Pool`) and never has a `Pool` reference to
dereference in the first place. `platform/compositor/tests/
malformed_damage.rs`'s `a_malformed_damage_rect_is_rejected_without_touching_
the_pool` pins this down against a real `mmap`ed pool: it fills both slots
with a sentinel byte, submits an out-of-bounds rect, and asserts every byte
is still the sentinel afterwards.

### The buffer lifecycle: three states per slot, not a boolean

`SlotState::{Free, Attached, Presenting}`. `attach` refuses a slot that is
not `Free` (`SurfaceError::BufferBusy`) — this is the enforcement point for
"a client writing before release is detected": the only API this crate
offers for a client to start drawing into a slot is `attach`, and `attach`
is exactly what a slot still `Presenting` refuses. `commit` moves the
attached slot to `Presenting` and only `release` (called by whatever reads
`Presenting` content — the panel-copy step, not built in this ticket) moves
it back to `Free`. Both slots may be `Presenting` at once: that is the
backpressure two buffers were sized for, and a client that tries to get a
third frame ahead is refused the same way a first over-eager attach would
be.

A rejected `commit` — bad damage, or nothing attached — is a no-op: it
returns before touching `pending_buffer` or any slot's state, so a client
that got one message wrong is not left in a state a *correct* subsequent
message could not still reach.

## What stays out of this stage

Named explicitly, per WWW-52's own instruction not to "deliver half of it
silently":

- **No socket, no client connection, no running process.** There is nothing
  in this crate that accepts a connection, receives an fd, or runs an event
  loop. `Pool::from_file` takes an already-open, already-sized `File` —
  production will hand it one built from a received fd (WWW-78); this
  ticket's own tests hand it one built from `tempfile`. WWW-78 is exactly
  "the non-blocking accept loop and crash/hang teardown," staged behind this
  ticket because it needs this state to exist first.
- **No panel presentation.** Nothing here reads a `Committed` frame's pixels
  and writes them anywhere — not to a framebuffer, not through ADR-0007's
  vendor engine. `Surface::current()` exposes the committed slot and damage;
  what consumes it is later work.
- **No wiring into `HostMessage`/`AppMessage`** — see above.
- **No system chrome, gesture routing, crash/hang isolation, or Xochitl
  migration** — WWW-78 through WWW-82, all explicitly out of scope in
  WWW-77's own description.

## Consequences

- `platform/protocol` gains `BufferSlot`, `ShmPoolDescriptor` and
  `MAX_POOL_BYTES`, all additive and re-exported from `lib.rs` alongside the
  existing vocabulary.
- New crate `platform/compositor` (`paper-compositor`), a library with no
  `[[bin]]` yet — deliberately, per "No socket..." above.
- 19 new tests: `message.rs` (pool descriptor layout and its degenerate
  cases), `pool.rs` (real `mmap`, slot isolation, oversized/undersized/
  mismatched pools), `surface.rs` (pure state machine: attach/commit/release,
  damage bounds, `MAX_DAMAGE_RECTS`), and `tests/malformed_damage.rs` (both
  acceptance criteria against a real mapping).

## What would make this wrong

- **If WWW-78's socket layer needs a pool shape this ADR did not
  anticipate** — for instance, if receiving an `SCM_RIGHTS` fd needs the
  `Pool` to validate something beyond size (a `seals`/`memfd` check, say).
  `Pool::from_file` is the one seam that would change; `Surface` should not
  need to.
- **If WWW-81 finds that merging the compositor's client protocol into
  `HostMessage`/`AppMessage` is right after all**, once a real client exists
  to design it against — the "No new variants" decision above is scoped to
  *this* ticket having no client to design for, not a permanent position.
- **If two buffers per client turns out not to be enough** once a real
  present pipeline exists and its latency is measured on hardware — nothing
  here has run against the vendor engine; `MAX_POOL_BYTES` and the two-slot
  assumption are both sized for what WWW-52 specified, not for a measured
  cost.
