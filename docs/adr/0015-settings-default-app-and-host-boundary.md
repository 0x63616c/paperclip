# ADR-0015 — Settings is a default app, and its Host boundary

**Status:** accepted (Stage 6/7, WWW-22).

## Context

Spec §3 and §5 name only Home, App Store and Chess. Calum decided on
2026-09-16 that Settings ships with Paperclip as a third default app,
alongside Home and App Store, with Chess and everything else installed
through the catalog. The reasoning, recorded on WWW-22 and repeated here so it
survives independently of the issue:

A default app is one the environment cannot sensibly function without.
Settings qualifies for the same reason App Store does: you must be able to
reach it when the catalog is unavailable, when an install has failed, or when
the thing that needs fixing is what is stopping other apps working. An
installable Settings could be uninstalled, leaving no way to change the
setting that would let you reinstall it. It also inherits the platform-release
lifecycle from §13 — Host, protocol support, platform integration, Home, App
Store and now Settings are one tested platform release, versioned together by
`paperctl upgrade`, while ordinary apps stay independently versioned.

## The §3/§5 amendment

> **§3 / §5.** Home, App Store and Settings ship with Paperclip and are always
> present. They form one tested platform release with the Host (§13).
> Everything else, including Chess, is installed through the catalog and
> independently versioned.

## Decision: scope, and what Settings is a client of

Settings is a client of platform facilities, never an owner of them — the same
rule §6 states for App Store. `apps/settings` (crate `paper-settings`) draws
six pages (installed apps, storage, grants, catalog, platform, diagnostics)
plus one always-available action (return to stock), and every destructive
action — uninstall, rollback, revoke — confirms first and states plainly what
would be lost, per the issue's requirement.

Settings does not reimplement anything stock owns (frontlight, Wi-Fi, PIN,
sleep timeouts) and does not manipulate the storage layout or an install
policy directly. Every read and every write goes through
`paper_settings::host::SettingsHost`, a trait with six read methods (a
snapshot each of installed apps, storage usage, grants, catalog status,
platform facts and diagnostics) and three write methods — `rollback`,
`uninstall`, `revoke_grant` — which are exactly the Host transactions this app
is scoped to. `host::PlaceholderHost` is fixture data for the desktop preview
and this crate's own tests — not device evidence. `host::LiveHost` (added in
the second pass, below) is the implementation backed by a real store.

## What Settings is granted, and what it deliberately is not

The issue asked for this to be written down rather than left implicit:
**Settings holds none of the four `Capability` variants**
(`storage`, `network`, `sharing`, `packages` — see ADR-0003). This was written
against a three-variant enum while WWW-7 was still in flight; WWW-7 landed
`Capability::Packages` — "install, update and roll back packages" — on `main`
before this ADR was committed, stated explicitly as something "the App Store
holds and nothing else does." That confirms rather than changes the
conclusion below: `storage`/`network`/`sharing` govern what an *ordinary* app
may do with its own sandboxed resources, `packages` is App Store's alone by
design, and none of the four is the right shape for "list every installed
app," "roll one back," or "revoke a grant held by a different app" — those
read and mutate state *about other apps*, which no existing capability
authorizes for anyone.

What that leaves, stated plainly:

- Settings needs **less than App Store**: it never installs a package, so it
  is correctly excluded from `Capability::Packages` by the same design that
  grants it to nobody else.
- Settings needs **more than an ordinary app**: read access to every
  installed app's manifest and storage footprint, and to the platform's
  grant table, that an ordinary app is never given about anyone but itself.
  `PackageManager` exposes `install`, `rollback` and `recover`, scoped to one
  app at a time; `LiveHost` (below) reads across every app through
  `Inventory::survey` and `Layout`'s own directory accessors instead of a
  dedicated cross-app grant, because nothing needs to authorize Settings
  specifically to call functions that already take a `Layout` by value.
  What genuinely does not exist yet — no `uninstall` transaction anywhere in
  the Host layer, no persisted or enumerable grant store behind
  `InstallPolicy` — is recorded in the second pass below, not invented here.
- Settings needs **built on the SDK like any other app** — no special access
  to Qt, systemd, SSH or Xochitl. Nothing in `apps/settings` uses `unsafe` or
  touches `platform/device`.

## Consequences

- `apps/settings` exists as a workspace member (`paper-settings`), with a
  `paper.toml` laid out identically to `apps/home` and `apps/chess`.
- `tools/paperctl` gained a third preview screen (`s` key, `--screen
  settings`) and `screenshot --screen settings` now writes one PNG per page
  plus one confirmation-dialog frame, driven through the same public
  `SettingsScreen::press_apps` a real tap would use — not a hand-built dialog.
- `tests/system` now validates the settings manifest alongside home's and
  chess's (id distinctness, no self-granted capability, runnable protocol),
  and checks the settings screen renders at the real panel geometry.

## Second pass: wiring `LiveHost` (2026-09-17, same day)

WWW-5 and WWW-7 landed for real while this issue was open — the app contract,
the install transaction, catalog and rollback machinery (`paper_packages`:
`inventory`, `install`, `launch`, `store`, `catalog`). Evee asked for the
placeholder restriction to be lifted and the pages wired to what now exists.
Verified against the landed code (not assumed) before wiring anything:

- **Installed apps and rollback: real.** `host::LiveHost` builds
  `InstalledAppSummary` rows from `paper_packages::inventory::Inventory`, and
  `rollback` calls `PackageManager::rollback` — the same calls
  `tools/paperctl install|list|rollback` make against `Layout::from_environment()`.
  Tested against a real temp-directory store, seeded the same way
  `paper_packages`' own tests seed one — not a mock of the store, the store.
- **Storage: real.** `Layout` gained `data_bytes`/`shared_bytes`/
  `staging_bytes`/`releases_bytes`/`app_data_bytes` (recursive, safe directory
  walks; missing directories count as `0`, not an error) and `free_bytes`,
  which shells out to `df -Pk` — matching `platform/host/src/probe.rs`'s
  existing pattern of reading system facts via a command rather than adding
  FFI for one number. No `unsafe` added anywhere in this pass.
- **Uninstall: still not supported, confirmed rather than assumed.** There is
  no `uninstall`/`remove`/`delete` transaction anywhere in `paper_packages` —
  `PackageManager` has `install`, `rollback`, `recover` and nothing else. A
  hand-rolled uninstall in `apps/settings` — deleting `apps/<id>`, `data/<id>`
  and the selection files directly — would bypass the journal-and-fsync
  durability `store.rs` exists to guarantee (see its own module doc: "what a
  power cut is allowed to leave"), for the operation with the most to lose
  from getting that wrong. `LiveHost::uninstall` returns
  `HostOpError::NotSupported` with the reason attached, rather than doing
  that or silently pretending to succeed.
- **Grants: still not supported, confirmed rather than assumed.**
  `InstallPolicy` (ADR-0003) is constructed fresh per install and is never
  persisted or enumerated — there is no "every grant currently in force" to
  read, and no store to revoke one from. `LiveHost::grants` returns an empty
  list (the honest answer: nothing is known to be granted) and
  `LiveHost::revoke_grant` returns `HostOpError::NotSupported`.
- **Catalog: still not supported, confirmed rather than assumed.** No catalog
  endpoint is persisted anywhere a Settings-launched process could read it —
  `paperctl install`/`list` take `--catalog` explicitly on every invocation,
  with no default. `LiveHost::catalog_status` reports "not configured",
  which is the real state, not a fixture standing in for one.
- **Platform facts: partially real.** `paperclip_version` now comes from
  `env!("CARGO_PKG_VERSION")` rather than a literal. `firmware` and
  `active_release` stay "not verified" — nothing in this pass runs on the
  device, and inventing a firmware string would violate the project's own
  standing rule against claiming a hardware gate without device evidence.
- **Diagnostics: real, plus one addition.** Failed-launch entries come from
  `paper_packages::launch::Ledger::health` per app. A synthetic entry states
  plainly that grants and catalog have no persisted Host state yet, so the
  page does not read as "verified empty" when it is actually "nothing to
  read."
- Nothing in `tools/paperctl`'s preview or screenshot path changed to use
  `LiveHost` — it still draws from `PlaceholderHost`, deterministic and
  already covered by existing tests. `LiveHost` is proven by 7 new tests in
  `apps/settings/src/host.rs` against real temp-directory stores (installed
  apps, rollback, rollback-with-nothing-to-roll-back-to, storage byte counts,
  and that uninstall/grants/catalog report their real absence rather than
  fake presence), not by a screenshot.

## What would make this wrong

If a future decision concludes the cross-app surface Settings needs *is*
better expressed as `Capability` variants after all (rather than a separate,
non-sandbox authorization concept), this ADR's reasoning would be superseded
by that decision, not by this document. If `paper_packages` grows an
`uninstall` transaction or a persisted grant store, `LiveHost`'s two
`NotSupported` returns are exactly what should be replaced, and nothing above
`host.rs` should need to change shape when that happens.
