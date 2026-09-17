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
is scoped to. This is the one module this pass introduces that a real Host
client should replace; nothing above it (`nav`, `pages`, `confirm`, `screen`)
should need to change shape when that happens. Until then,
`host::PlaceholderHost` is fixture data for the desktop preview and the test
suite — not device evidence, not a Host implementation.

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
  As of this pass, nothing in the landed `paper-packages`/`paper-host` surface
  models that access either — `PackageManager` exposes `install`, `rollback`
  and `recover`, scoped to one app at a time, and no `uninstall` yet exists
  anywhere in the Host layer. Defining the shape of a cross-app,
  read-plus-administer surface (and whether `uninstall` belongs on
  `PackageManager` at all) is follow-up work, not invented here.
- Settings needs **built on the SDK like any other app** — no special access
  to Qt, systemd, SSH or Xochitl. Nothing in `apps/settings` uses `unsafe`,
  touches `platform/device`, or shells out.

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
- A real `SettingsHost` implementation, and the shape of the cross-app
  read/administer surface it needs from `paper-packages`/`paper-host`
  (including whether `uninstall` is ever added to `PackageManager`), are
  follow-up work against the `paper-packages` surface WWW-7 landed on `main`
  while this issue was in progress.

## What would make this wrong

If a future decision concludes the cross-app surface Settings needs *is*
better expressed as `Capability` variants after all (rather than a separate,
non-sandbox authorization concept), this ADR's reasoning would be superseded
by that decision, not by this document.
