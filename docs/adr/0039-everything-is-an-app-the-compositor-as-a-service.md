# ADR-0039 — Everything is an app: the compositor as a long-running service

**Status:** accepted (WWW-52, WWW-81). Builds on ADR-0033 (WWW-77) and
ADR-0037 (WWW-78) — see their own "What stays out of this stage" sections for
exactly what this ticket picks up. Amends ADR-0022's "surface is still
`LocalSurfaces`" decision for the four entrypoints named below, without
withdrawing it for Chess or Sudoku — see "What this does not change" below.

## Context

WWW-52 asked for Home, Settings, the App Store, the test card and stock
Xochitl to become ordinary compositor clients, replacing
`platform/host::state::Foreground` and `session_unit_name`'s
`paperclip-session.target` `Conflicts=` as the mechanism that decides who is
on the panel. ADR-0037 built the compositor's client wire and its own
foreground arbitration but deliberately shipped it with "no `[[bin]]`, no
well-known socket path, and no wiring to the vendor panel or to a real
client" — this ADR is that wiring.

Two things turned out to matter more than the ticket description anticipated,
found while reading the code this ticket touches rather than assumed going
in:

- **No real client has ever spoken to a real host process on the device.**
  ADR-0022, read in full: `paper_sdk::run`'s Hello/Draw/Ready/exit lifecycle
  is proven by `paperctl dev`'s in-process loopback and by
  `apps/*/tests/entrypoint.rs`'s spawn tests, but "no host process on the
  device has spawned one yet" — `paperclip-app@.service`'s `ExecStart=`
  starts the binary directly, with nothing on the other end of its stdio.
  Every entrypoint that used `LocalSurfaces` therefore never touched the real
  panel or the vendor advisory locks on the device, ever — not a regression
  this ticket introduces, a gap that predates it.
- **`platform/host::linux::runtime`'s display-ownership check could never
  have succeeded for a real launched app, for the same reason.**
  `display_owned_by_session` read `/tmp/epframebuffer.lock` for a pid inside
  the switching session's own cgroup — right for a process that opens the
  panel itself, and no app ever did that. See "What this does not change"
  below for what replaces it and why that is still correct once Home,
  Settings, the App Store or the test card *does* open a real connection to
  something that owns the panel.

## Decision

### The compositor is the one process that ever opens the panel

`platform/compositor/src/bin/paperclip-compositor.rs` is a new `[[bin]]`:
binds a `Compositor` to a socket, opens a `Panel` (`MemoryPanel` by default;
`VendorPanel` under this crate's new `vendor-engine` feature, forwarding to
`paper-device`'s own feature of the same name), and drives `run_once` in a
loop until `SIGTERM`/`SIGINT`, at which point it clears the panel (ADR-0009's
shutdown ordering) and drops it before exiting.

`platform/host/src/units.rs` gains `COMPOSITOR_UNIT` /
`paperclip-compositor.service`: long-running for the whole session, unlike
the per-owner `paperclip-app@.service` it presents on behalf of.
`SessionPaths::compositor_socket`/`compositor_status` name where its socket
and status file live, both under `/run/paperclip` alongside the supervisor's
own command/status files. The compositor unit is included in both
`UnitSet::plan` and `UnitSet::for_supervisor` — unlike the per-app unit,
nothing about it depends on which app is chosen, so there is nothing correct
to defer.

### Home, Settings, the App Store and the test card are ordinary compositor clients

`platform/compositor/src/client.rs` is new: `CompositorSurfaces` /
`CompositorSurface` implement `paper_sdk::surface::{SurfaceProvider,
Surface}` by speaking the compositor's own wire
(`ClientHello`/`ClientRequest`/`HostEvent`, ADR-0037) instead of holding a
directly-mapped buffer. Each of the four entrypoints' `main.rs` now passes
`CompositorSurfaces::new(socket_path(), role, label)` where it used to pass
`LocalSurfaces::new()` — nothing else in any of the four apps changes. This
is the whole of "ordinary client with no special privileges": the app's
`Hello`/`Draw`/`Ready`/exit conversation with whatever eventually drives it
(ADR-0022's still-open gap, not this ticket's to close) is byte-for-byte
unchanged, and only *where the pixels go* differs.

Chess and Sudoku are explicitly not touched. WWW-52 names four entrypoints,
not six, and neither is a default app (§13) — migrating them is a decision
to make with their own ticket, not a silent extension of this one's scope.

Deliberately reusing the existing `SurfaceProvider`/`Surface` split
(`platform/sdk/src/surface.rs`) rather than inventing a new transport
abstraction: it already exists exactly to let a caller swap "where an app's
pixels land" without touching the app or the launch-time protocol above it,
and `LocalSurfaces` already proved the shape for the desktop preview and
every test in the repository.

A client's pool is backed by `memfd_create` on Linux (anonymous, referenced
only by fd — nothing to leak or clean up, matching "Paperclip installs
nothing persistent") and by an unlinked temp file elsewhere (the Mac desktop
preview and this crate's own tests, where `memfd_create` does not exist).

### Which socket a client connects to: an environment variable, not a literal

`paper_compositor::SOCKET_ENV` (`PAPERCLIP_COMPOSITOR_SOCKET`) is the one
constant both sides reference — `units.rs`'s `app_service` sets it,
`paper_compositor::socket_path()` reads it (falling back to
`/tmp/paperclip-compositor.sock` for `paperctl dev` and manual runs that
predate a real host setting it). A literal string duplicated in two crates
was rejected on purpose: ADR-0011 already recorded what a
merely-similar-looking duplicate cost once (`RecoveryConfig::wakelock_name`,
WWW-35, corrected to reference `paper_device::takeover::WAKELOCK_TAG`
directly) — the fix here is the same shape, not a new one.

### Stock Xochitl becomes a managed client — by stopping and starting the compositor, not by touching `Takeover`

`platform/device::takeover::Takeover` is untouched. Nothing in this ticket
calls it, imports it more than the existing `RecoveryConfig` already did, or
changes a line inside `platform/device`. What changes is what
`linux::runtime::Supervisor::start_foreground` does around the switch:

- **Going to stock**: before `self.restore(1)` (unchanged — still
  `StockRecovery::restore`, still the guarded-start policy, still the
  journal check for "active but does not own the panel"), the supervisor
  stops `COMPOSITOR_UNIT`. Best-effort: its outcome does not gate the
  restore, because `StockRecovery::restore` already reaps any stray DRM
  holder and clears stale advisory locks before starting stock — exactly the
  defence a compositor that failed to stop cleanly needs, already built,
  already tested (`platform/host/tests/host.rs`), and not duplicated here.
- **Leaving stock**: before starting the session target and the app unit,
  the supervisor starts `COMPOSITOR_UNIT` — idempotent against one already
  active from an app-to-app switch within the same session, the same
  property the unconditional `SESSION_TARGET` start already relies on. A
  refusal is queued as `Event::SessionExited { status: ExitKind::Error }`,
  the same signal a refused `SESSION_TARGET` start already produces, so a
  systemd-level compositor refusal is known immediately rather than only at
  the switch deadline.

This is deliberately the narrowest change that makes "stock is a grantable
app, mechanically special because of the vendor lock/wakelock/start-limiter
hazards Takeover exists for, not because it is privileged in the model"
(ADR-0027) extend onto the compositor: the compositor now owns the panel for
every non-stock moment, and stock's own safety mechanics are reused exactly
as they stood, not re-derived.

### What this does not change: the state machine's shape, and what replaces the display-ownership check

`platform/host::state::Foreground` is unchanged — still `Stock`/`Home`/
`App(AppId)`. Windowing stays out of scope (WWW-81's own text), so there is
still exactly one foreground *app*, matching the compositor's own "newest App
wins" policy (ADR-0037) — the two models were never in tension.

`display_owned_by_session` (the vendor-lock-registry-and-cgroup check) is
replaced by `compositor_reports_a_client_foreground`, reading a line the
compositor's own `write_status` writes to its status file
(`foreground=client:<label>` / `foreground=stopped:<label>` /
`foreground=none`) — the same "control arrives through a file" choice
`linux::runtime`'s own module doc already makes for the supervisor's command
file, applied to reading the compositor's state rather than writing the
supervisor's own.

The new check does not match on *which* client's label is foreground; it
only asks whether the compositor is presenting a connected client at all.
That is deliberate, not a missed opportunity for a tighter check: the
project's v1 scope is single-foreground-app (ADR-0037), the supervisor never
has more than one app unit active at a time, and Home/Settings/the App
Store/the test card are the only clients that exist — so "the compositor is
presenting *some* client" and "it is presenting *this* session's app"
coincide for as long as that invariant holds, without needing the state
machine's `AppId` and the compositor wire's free-form `label` to be kept in
lock-step by two independent authors (an app's `main.rs` picks its own
`ClientHello.label`; nothing enforces it matches anything).

**Chess and Sudoku still never complete a real on-device switch.** They are
not compositor clients, so nothing ever writes `foreground=client:` on their
behalf. This is the same gap `display_owned_by_session` always had for them
— unchanged by this ticket, not introduced by it — and is exactly the "what
would make this wrong" ADR-0022 already flagged for a future migration.

## What stays out of this stage

- **Chess, Sudoku, and any other catalog app.** Named explicitly in "What
  this does not change" above.
- **A real host process piping `Hello`/`Draw` to a launched app on the
  device.** ADR-0022's gap; this ticket does not close it, because nothing
  about the compositor migration requires it to be closed first — the
  compositor wire is independent of that launch-time protocol, which is the
  whole reason ADR-0033 kept them as two wires rather than merging them.
- **Narrowing `app_service`'s `DeviceAllow=`.** An app no longer needs
  `/dev/dri/card0` or input-device access — the compositor is the only
  process that opens them now — but `app_service`'s own device grants are
  unchanged from before this ticket. Narrowing them is a real hardening
  improvement this ticket found and did not make: it is a separable,
  independently reviewable change, and bundling it here would have made this
  diff harder to review for the thing it is actually about.
- **Staging `paperclip-compositor` into a platform release.**
  `platform/updater::manifest::REQUIRED_COMPONENTS` and
  `tools/stage-platform.sh` are untouched. The platform updater (WWW-8) is
  named in this project's own standing instructions as needing extra care
  ("it can leave the environment unbootable"), and changing what a release
  must contain is squarely inside that boundary. Until it is updated,
  `paperclip-compositor` is buildable and testable but **not staged into any
  platform release** — the units this ADR writes name a binary that does not
  yet exist at `{root}/bin/paperclip-compositor` on a real installed device.
- **A wakelock held for the whole Paperclip session.** Neither
  `linux::runtime` nor this ticket's changes to it acquire one while
  Home/an app is foreground; only `StockRecovery::restore` releases one on
  the way back to stock. This predates WWW-81 and is not this ticket's gap
  to close.
- **Chrome composition and gesture routing wired into the compositor's own
  event loop.** `ChromeState`/`GestureDetector` (ADR-0035, ADR-0034) still
  have no event loop feeding them — `present_foreground` still blits a
  client's raw pool slot, unchanged.

## What is proven, and what is not

**Proven on the Mac**: the client wire round-trips against a real
`Compositor` bound to a real `UnixListener` (`platform/compositor/src/
client.rs`'s own tests: connecting registers a client, publishing alternates
slots and reaches the panel); all four migrated entrypoints build and, for
the one with a spawn-based integration test
(`apps/render-test-card/tests/entrypoint.rs`), answer `Hello` with `Ready`
and exit cleanly against a real compositor the test itself binds; the whole
workspace's existing test suite (`cargo test --workspace`, 1200+ tests
including `platform/host`'s and `platform/compositor`'s) is unaffected;
`cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D
warnings` are clean.

**Not proven — every one of these needs the VM harness or the device**:

| Gate | Status | What it blocks |
|---|---|---|
| `linux::runtime`'s compositor start/stop wiring, against real systemd | **open** — `platform/host`'s own Mac-side tests do not exercise `collect()`/`start_foreground` against a filesystem or a real `systemctl`, by the crate's own `linux` vs. platform-free split (`lib.rs`'s module doc); only `tests/failure-harness`'s VM proves that layer, for any code in `linux::runtime`, not only this ticket's | whether a real switch to Home ever reaches `SessionState::Home` |
| `compositor_reports_a_client_foreground` reading a real status file written by a real `paperclip-compositor` process | **open**, same reason | the same |
| The compositor opening a real `VendorPanel` and presenting a client's frame | **open** — nothing here has run against `libqsgepaper.so`; ADR-0009's open hardware gates are unaffected by this ticket | whether anything reaches the glass through this path |
| Stopping the compositor cleanly under systemd (`SIGTERM`, the panel clear, the process actually exiting before `StockRecovery::restore` runs) | **open** | the "xochitl.service never enters a failed state" acceptance criterion, on real hardware |
| `paperclip-compositor` staged and executable at `{root}/bin/paperclip-compositor` on an installed device | **closed, and it is not** — see "What stays out of this stage" | running any of the above at all on a real install |

A passing test in this repository is not device qualification. A build is
not a screen.

## What would make this wrong

- **If the single-foreground-app invariant ever stops holding** — real
  multi-app arbitration, a second simultaneously-connected app — then
  `compositor_reports_a_client_foreground`'s "some client, not necessarily
  this one" check stops being sound and needs the label-matching this ADR
  deliberately did not build (see "What this does not change").
- **If a real host-driven launch (closing ADR-0022's gap) turns out to need
  the compositor socket path or role passed some other way than an
  environment variable** — `Environment=` on a systemd unit is easy to miss
  when a process is spawned by something other than systemd (a future
  in-process `paperctl dev`-style launcher, say). `socket_path()`'s
  environment-variable-with-a-default shape would need revisiting, not the
  wire itself.
- **If narrowing `app_service`'s `DeviceAllow=` (left undone, see above)
  turns out to matter for §11 sooner than expected** — nothing currently
  depends on apps keeping that access, so the gap is safe to leave, but it
  should not be read as settled.
