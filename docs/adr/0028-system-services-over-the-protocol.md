# ADR-0028 — System services over the protocol, and what stays out of this pass

**Status:** accepted. The no-grant tier (Stage 2, WWW-50) and Settings'
admin surface (WWW-71) are both real and tested. `apps/app-store`'s
dependency on `paper-packages` is still **not** done — the "What stays out of
this pass" section below still describes why, updated for what WWW-71
actually shipped.

## WWW-71 — Settings' admin surface, done

`SettingsHost`'s nine methods (`installed_apps`, `storage_usage`, `grants`,
`catalog_status`, `platform_info`, `diagnostics`, `rollback`, `uninstall`,
`revoke_grant`) are gone as a trait `apps/settings` implements against a
process-local `paper_packages::install::PackageManager`. They are now
`platform/protocol/src/admin.rs`'s `AdminQuery`/`AdminValue` — a fourteenth
and fifteenth wire type, carried as `SystemQueryKind::Admin`/
`SystemValue::Admin` rather than new `AppMessage`/`HostMessage` variants, per
this ADR's own "Recommended shape" below. `apps/settings/Cargo.toml` no
longer names `paper-packages` at all, transitively or otherwise (`cargo tree
-p paper-settings` confirms it).

**The shape, as built, matches the recommendation with one refinement.** The
gate is the connection's `AppId` against `dev.calum.settings`, checked host
side in `tools/paperctl/src/admin.rs`'s `AdminResponder` — not a new
`Capability`, so `GrantedCapabilities` is untouched exactly as planned. The
refinement: `AdminValue`'s three write variants (`Rollback`, `Uninstall`,
`RevokeGrant`) each carry their own `Result<(), AdminError>`, distinct from
the outer `SystemAnswer`'s `Result<SystemValue, SystemDenial>`. A
`SystemDenial` says the *question* was refused (wrong caller, asking too
fast); an `AdminError` says the *transaction* the question named did not go
through (`NoPreviousRelease`, `NotInstalled`, `GrantNotHeld`,
`NotSupported`, `Failed`). Folding the two together, as a first draft of this
work did, loses exactly the distinction `SettingsScreen`'s confirmation
dialogs exist to report.

**`apps/settings` became a cache-and-query client, at Settings' own scale.**
`SettingsScreen` holds a cached snapshot (`SettingsScreen::loading()` until
the first answers land) and no longer calls a host at all —
`SettingsApp::draw` sends all six reads on first launch and again on
`Event::Resumed` (`apps/settings/src/app.rs`'s `READS` constant), and
`Event::System` files each answer under the field it belongs to
(`SettingsScreen::apply`). `rollback`/`uninstall`/`revoke_grant` are exactly
the fire-then-observe shape predicted below: `SettingsScreen::confirm` closes
the dialog and hands back what was confirmed without waiting for the host,
`SettingsApp` sends the write as an `AdminQuery`, and the eventual answer
either logs a warning (`context.warn`) or is silent — followed either way by
re-sending all six reads, rather than guessing which fields a write could
have changed.

**Where the answer comes from is unchanged from the no-grant tier**:
`tools/paperctl/src/session.rs`'s `Session` is still the one real process
that plays host, now over two responders — `SystemResponder` for the
no-grant tier and a separate `AdminResponder` for `Admin` queries, gated and
rate-limited independently (its own `RateWindow`, not a shared counter: a
misbehaving app asking for battery readings must not cost Settings its own
budget, and vice versa). `AdminResponder`'s domain logic — reading a real
`Layout`, rolling back through a real `PackageManager` — is `LiveHost`'s old
implementation moved host-side essentially verbatim; the honest gaps
`LiveHost` recorded (`grants()` always empty, `uninstall`/`revoke_grant`
always `NotSupported`, no persisted catalog endpoint) are carried forward
unchanged, because nothing about moving them across the process boundary
gave `paper_packages` a durable grant store or an uninstall transaction it
did not already lack (ADR-0015 still names what would have to land first).

**Preview and test fixtures moved with it.** `PlaceholderHost` and the
`SettingsHost` trait are gone; `SettingsScreen::preview()` is the one fixture
every test and `tools/paperctl/src/screens.rs`'s desktop preview now share
(previously the preview built its own `PlaceholderHost` and this crate's
tests built theirs independently). The preview window still lets a press
open and close a confirmation — real, interactive — but asks no host
anything on confirm, the same "the preview navigates; it does not install"
choice the App Store preview already made, for the same reason: there is no
`AdminResponder` behind a bare render call.

## Context

Before this stage, an app could not ask the platform anything.
`platform/protocol/src/message.rs`'s own doc said so: "An app's whole
vocabulary is five messages, three of which are answers to something the host
asked for." The one app that needed system facts, Settings, got them by
constructing `paper_packages::install::PackageManager` inside its own process
(`apps/settings/src/host.rs`'s `LiveHost`) — a direct violation of ADR-0016's
"apps depend on `paper_sdk` alone," and the thing this ticket exists to fix.

WWW-50's own description settles the shape from prior art rather than
reinventing one: Sailjail's model (permissions declared and granted at launch,
no runtime dialog) over Flatpak's portal machinery, and Android's precedent
for splitting *informational* facts (current battery state, no permission)
from *granted* ones (historical/per-app accounting, signature-only). Two parts
of the portal design are explicitly out: the `handle_token`
subscribe-before-call race (this wire is already ordered) and an
`xdg-dbus-proxy` equivalent (the process boundary already is the proxy).

## Decision: the no-grant tier, real end to end

Four facts an app may ask for with **no grant, no capability check**: time,
battery, network connectivity, platform version. `platform/protocol/src/system.rs`
is the whole vocabulary:

- `AppMessage::SystemQuery` — an app asks, carrying a `QueryId`
  (`platform/protocol/src/message.rs`).
- `HostMessage::SystemAnswer` — the host answers, correlated by `QueryId`
  because the wire is asynchronous: an answer may arrive after a `Draw` or a
  `Lifecycle` message, not necessarily right after the query.
- `HostMessage::SystemEvent` — **push, not poll** (the ticket's own
  requirement): the host tells every app when battery or network changes,
  without being asked. Time is deliberately absent from this one — a clock
  ticking is not a change worth an e-ink app redrawing over.
- `SystemDenial { reason, detail }` — machine-readable. `BackendUnavailable`
  (no battery node found, `nmcli` would not run), `RateLimited`
  (`MAX_SYSTEM_QUERIES_PER_SECOND`, `platform/protocol/src/limits.rs`), and
  `Unsupported` (a query kind newer than this host build — the
  `#[non_exhaustive]` forward-compatibility case). A generic failure was
  exactly what the ticket asked not to ship.

The vocabulary grew from five app-side messages to six, and from a closed set
with no "ask" verb to one with exactly one. `message.rs`'s module doc is
updated to say so; that comment was a deliberate design claim and this is the
deliberate amendment the project description asked for.

### Where each backend lives

`platform/sys` (the effects spine, ADR-0025/WWW-46) gained three more small
traits, each with one real adapter, matching the pattern `Clock`/`Process`/
`UnitControl`/`Storage` already set:

- `WallClock` / `SystemWallClock` — `SystemTime::now()`. Kept separate from
  `Clock` (monotonic, no fixed origin) rather than widening it.
- `PowerSource` / `SystemPowerSource` — scans `/sys/class/power_supply` for
  whichever node reports `type: Battery`, rather than a hard-coded device
  path. WWW-1 never confirmed this tablet's node name, and this project has
  already paid once for guessing a hardware detail instead of scanning for it
  (the 4bpp-packing premise the project description records as the reason for
  its "research before theorising" rule). **Unverified on hardware** — open
  until a device session confirms the node exists and reads as expected.
- `Network` / `NmcliNetwork<Process>` — built on `Process` the same way
  `Systemctl` is, shelling out to `nmcli -t -f active,ssid,signal dev wifi`.
  NetworkManager is already established as what owns Wi-Fi on this device
  (the project description's own backup-scrubbing rule names "NetworkManager
  profiles with Wi-Fi PSKs"). **Unverified on hardware** — whether `nmcli` is
  on `PATH` in the environment Paperclip's processes run under is open.

`tools/paperctl/src/system.rs`'s `SystemResponder` answers queries over these
backends and is where **paperctl plays host** — the only real process this
workspace runs apps under today (`tools/paperctl/src/session.rs`'s `Session`,
not `platform/host`, which is the systemd/cgroup supervisor and never touches
the wire protocol). Rate limiting is a per-session token-bucket
(`RateWindow`), and `SystemResponder::changes()` is polled once per
`request_frame` call to push `SystemEvent`s before the next `Draw`.

`paper_sdk` gained the client half: `Context::query_system(kind) -> QueryId`
(queues a query the way `Context::log` queues a diagnostic) and
`Event::System` / `Event::SystemChanged` for the answer and the push. Neither
needed a synchronous "wait for the answer" primitive — the existing
single-queue cooperative loop (`platform/sdk/src/runtime.rs`'s module doc:
"No executor, no polling, no locks in an app") already delivers an
out-of-band `HostMessage` to the next `event()` call in order, so a query
answer is just another message in that same queue, dispatched to `Event::System`
like a `Pointer` or a `Lifecycle` event.

### The one real consumer: Home shows battery

`apps/home` asks for `Battery` once, on its first `draw()` (fire-and-forget;
`draw` must not block, and queuing a message does not), and files the answer
under a `"Battery"` fact via `HomeScreen::set_fact` — a small addition to a
type whose own doc already says "the home screen should display platform
facts, not go looking for them." `HomeApp::apply_battery` also reacts to
`Event::SystemChanged`, so the number updates on its own when the host pushes
a change. This is deliberately the smallest real consumer: it proves the pipe
end to end (protocol → `platform/sys` → `paperctl`'s responder → SDK →
`HomeScreen`) without touching `apps/home`'s existing `paper_packages`
dependency (manifest parsing for the shelf, unrelated to this ADR).

## Decision: what stays out of this pass, and why

WWW-50's acceptance criteria asked for two things beyond the no-grant tier:
`SettingsHost`'s nine methods served over the protocol, and `apps/home` and
`apps/app-store` no longer depending on `paper-packages` at all. The first
shipped in WWW-71, described above. What follows is why the second still has
not, unchanged from WWW-50 except where WWW-71 touched it directly.

**`apps/app-store`'s dependency is not analogous to Settings' — it is the
install transaction itself.** `apps/app-store/src/source.rs`, `app.rs`,
`screen.rs` and `render.rs` (~2,600 lines combined) call
`paper_packages::install`, `catalog`, `inventory`, `signing` and `store`
directly to browse a catalog, download, verify and install a package with
live progress. WWW-50's own "Read first" list and its "The design" section
name only the nine `SettingsHost` methods and the no-grant tier — neither
proposes protocol messages for catalog browsing or install execution, which
would be new wire vocabulary of its own. Moving that logic host-side is
squarely the install transaction and packaging surface the project
description already carves out as needing a reviewer ("Signing, packaging and
the install transaction (WWW-7)... these keep their review and they are not
negotiable"). Doing it under `ship-without-review` would be doing reviewed
work without the review.

**`apps/home`'s remaining dependency is unrelated to this ticket's sandbox
concern.** `apps/home/src/main.rs`/`shelf.rs` use `paper_packages::Manifest`
only to parse three manifests compiled in with `include_str!` — pure text
parsing, no `Inventory`, no `Layout`, no `PackageManager`, nothing that
reaches installed-app state at runtime. It is not the "escaping the sandbox
to learn something" pattern the ticket's problem statement describes.
Removing it would mean splitting `Manifest`'s pure-parsing half out of
`paper_packages` into `paper_protocol` (feasible — `manifest.rs` imports
nothing from `store`/`install`/`catalog`/`signing`/`archive` today) but is a
separate, small, low-risk refactor with no bearing on isolation, tracked
separately rather than folded into this ADR's scope.

### Recommended shape for the remaining deferred work

When `apps/app-store`'s dependency is picked up: it needs its own ADR, under
the packaging review gate, because it is install-transaction design, not
system-services design. WWW-71's `Admin` variant is not the template for it —
catalog browsing, download and live install progress are a different, wider
wire vocabulary than nine request/response pairs, and folding them into
`SystemQuery`/`SystemAnswer` the way `AdminQuery`/`AdminValue` were would
strain the "one no-grant tier plus one identity-gated tier" shape this ADR
otherwise keeps closed.

## Constraints preserved

- `GrantedCapabilities` is untouched: no `Deserialize`, no `Default`, no
  public constructor, no new `Capability` variant. The no-grant tier needed no
  capability check at all; the admin surface (deferred) is scoped to gate on
  identity, not on this type.
- Rate limiting: `MAX_SYSTEM_QUERIES_PER_SECOND` is the one constant
  (`platform/protocol/src/limits.rs`) both `paperctl`'s `SystemResponder` and
  `AdminResponder` (WWW-71) check against — two independent `RateWindow`
  instances, one per tier, so a misbehaving app on one budget cannot cost the
  other tier's caller its own. The number is single-sourced even though the
  count is not.

## What would make this wrong

- If `platform/host` ever becomes the process that actually drives live app
  sessions (rather than the systemd/cgroup supervisor it is today),
  `SystemResponder` and `AdminResponder` both move there and
  `tools/paperctl/src/system.rs`/`admin.rs` become the desktop-preview-only
  copies, the same split `Session` itself already has.
- If the battery or network backend turns out not to work as scanned/shelled
  out to on the actual tablet, `SystemDenialReason::BackendUnavailable` is
  exactly the value that should carry that fact back to an app, and nothing
  above `platform/sys` needs to change shape.
