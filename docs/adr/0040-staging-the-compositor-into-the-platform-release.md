# ADR-0040 — Staging the compositor into the platform release

**Status:** accepted (WWW-86)

## Context

ADR-0039 built `paperclip-compositor` as a real `[[bin]]` and wired
`COMPOSITOR_UNIT` into `UnitSet::plan`/`UnitSet::for_supervisor`, but named
staging it into a platform release as explicitly out of scope: `REQUIRED_
COMPONENTS` and `tools/stage-platform.sh` were left untouched on purpose,
because the platform updater (WWW-8) is named in this project's own standing
instructions as needing extra care. Until this ticket, a release built from
`main` contained no `paperclip-compositor` binary at all — the units ADR-0039
wrote name a file that does not exist on an installed device, which is also
why no device session for WWW-52's acceptance criteria (buffer release, crash
isolation, chrome, gestures, Xochitl-as-managed-app, the lock screen) has
happened: there was nothing installable to run one against (WWW-86's own
description).

Two things turned out to matter more than "add a name to an array":

- **The compositor is not a fifth app.** `xtask::plan_release::bundled_app_
  dirs` derives its exclusion list from `REQUIRED_COMPONENTS` specifically so
  a future bundled component cannot be added there and silently double-
  published as a catalog app — and it has a test,
  `every_bundled_component_but_the_host_and_compositor_is_an_app_directory`,
  that exists to catch exactly this. `paperclip-compositor` has no `apps/
  paperclip-compositor/paper.toml`; it lives under `platform/compositor`, the
  same reason `paperclip-host` is already excluded. Both are now excluded by
  name.
- **The compositor is the one component of the five that needs the vendor
  engine, and building it plainly did not work.** `tools/stage-platform.sh`'s
  own header explained why it needed no Docker/Qt: none of the four original
  components link `paper-device`'s `vendor-engine` feature. The compositor
  breaks that invariant on purpose — ADR-0039's whole point is that it is now
  the *only* process that opens the panel — so it needs `vendor-engine` or
  `default_panel_kind` (`platform/compositor/src/bin/paperclip-compositor.rs`)
  falls back to `MemoryPanel` and the shipped binary never reaches the glass.
  Building it that way under Docker with `tools/cross/build-device.sh --bin
  paperclip-compositor paper-compositor` (extended here to take a package name
  distinct from the binary name — `paperctl`'s package and binary share a
  name, `paper-compositor`/`paperclip-compositor` do not) failed its final
  link with several hundred undefined Qt Quick/Qml/DBus symbols. The cause was
  already documented one crate over and simply not yet true of this one:
  `platform/device/build.rs`'s doc comment explains that its `-Wl,--allow-
  shlib-undefined`/RPATH `cargo::rustc-link-arg`s only reach targets in *that
  package*, and that `tools/paperctl/build.rs` has to repeat them for the same
  reason. `platform/compositor` linked the vendor engine without ever having
  its own copy of that build script. Confirmed by building
  `paperclip-compositor` under Docker with `--features vendor-engine` before
  and after adding `platform/compositor/build.rs`: the same command that
  failed with the Qt link errors above produced a real aarch64 ELF once the
  build script existed.

## Decision

**`REQUIRED_COMPONENTS` gains `paperclip-compositor`**
(`platform/updater/src/manifest.rs`), between `paperclip-host` and the three
apps — boot order, not alphabetical: the host brings the session up, the
compositor is the next thing that has to exist before any client (Home,
Settings, the App Store) can present anything. `PlatformManifest::check`'s
missing-component error and the module's own doc comment are updated to say
five components, not four; `bundle.rs`'s tar layout and `layout.rs`'s release
tree diagram gain the `bin/paperclip-compositor` line.

**`xtask::plan_release::bundled_app_dirs` excludes `paperclip-compositor` by
name, alongside `paperclip-host`.** Its own tripwire test caught this the
moment `REQUIRED_COMPONENTS` grew a fifth entry — exactly the failure mode its
doc comment describes — and is renamed and re-asserted rather than only
patched, so a future reader sees why two names are excluded, not one.

**`tools/stage-platform.sh` stages all five, through two different build
paths.** The plain `zig cc` loop is unchanged for `paperclip-host`, `home`,
`app-store` and `settings`. `paperclip-compositor` is built separately through
`tools/cross/build-device.sh --bin paperclip-compositor paper-compositor` (its
own Docker/Qt-headers machinery, now taking an optional package-name argument
for the one caller whose package and binary names differ) and copied into the
same `bin/` directory before packaging. The script's header comment is
rewritten to say this plainly rather than continue to claim no component here
needs Docker.

**`platform/compositor/build.rs` is new**, copying `tools/paperctl/build.rs`
verbatim in shape: under `vendor-engine`, it emits the same `-Wl,--allow-shlib-
undefined` and scenegraph-plugin RPATH `cargo::rustc-link-arg`s that
`platform/device/build.rs` already documents as needing to be repeated by
every package that links the vendor engine directly. `paperclip-compositor`
is now the second such package, and the third file carrying this explanation
(the first two already cross-reference each other; this one cross-references
both).

**The boot units needed no change.** `UnitSet::for_supervisor` and
`UnitSet::plan` already include `compositor_service` unconditionally
(ADR-0039) — nothing about which app is chosen affects the compositor unit,
so there was nothing deferred to pick up here. Confirmed by reading
`platform/host/src/units.rs` rather than assumed: this ticket's own
description asked for exactly that confirmation as its third scope item.

**Test fixtures that build all-required-components trees are updated to
build five, not four**: `platform/updater/tests/upgrade.rs`'s five call sites
that assemble a complete release now include `paperclip-compositor`; the one
that deliberately assembles an *incomplete* release
(`a_manifest_missing_a_required_component_is_refused`) is left short on
purpose, and its assertion message is generalised since which required
component it is now missing is no longer pinned to "Settings" by the test's
own construction. `tests/failure-harness/src/fixture.rs`'s two release-staging
helpers add `paperclip-compositor` to the set of names copied from the
`paper-fault-app` stand-in, the same way `home`, `app-store` and `settings`
already were. `tools/device-acceptance/run.sh` (§17, WWW-41, not exercised by
this ticket) is updated the same way, with its own real `vendor-engine` build
rather than a stand-in, since it is the one script here that stages against
the actual tablet.

## What it costs

`tools/stage-platform.sh` and `tools/device-acceptance/run.sh` now need
Docker, the Qt 6.10 headers and the vendor libraries copied off the tablet
(`PAPERCLIP_QT_INCLUDE`, `PAPERCLIP_VENDOR_LIB_DIR`) to produce a real
release — inputs that, per `tools/cross/build-device.sh`'s own doc comment,
are neither vendored nor universally available. A platform release can no
longer be staged from a clean checkout with only `zig cc`; this was true
already for anyone running `paperctl open` (`--bin paperctl` has needed the
same inputs since ADR-0007), and is now also true for staging a release, not
just for developing against the device interactively.

## What is proven, and what is not

**Proven on the Mac**: `./tools/stage-platform.sh` staged all five
components and produced five real aarch64 ELF files, including a `paperclip-
compositor` linked against the real `libqsgepaper.so`/Qt libraries copied off
this device (`file` reports `ELF 64-bit LSB pie executable, ARM aarch64 …
dynamically linked`). `paperctl upgrade package --source <staged dir>
--version 0.1.0 --protocol 1.0 --state-version 1 --rollback-to-state 1`
produced a signed bundle whose manifest lists all five components with their
digests and whose tar listing carries `bin/paperclip-compositor` alongside
the other four — the same round trip ADR-0022 verified for four. `cargo test
--workspace` passes with the updated fixtures and the renamed tripwire test.

**Not proven — this ticket is the precondition for these, not the gate
itself** (WWW-86's own framing):

| Gate | Status | What it blocks |
|---|---|---|
| A release built from `main` installs a working `paperclip-compositor` on the tablet | **open** — nothing here has run `paperctl setup`/`upgrade` against real hardware with a compositor in the bundle | every on-device claim below |
| `paperclip-compositor` opening a real `VendorPanel` and presenting a client's frame, on the device rather than under this Mac's Docker cross-build | **open**, unchanged from ADR-0039 | whether anything reaches the glass through this path |
| `COMPOSITOR_UNIT` starting cleanly under the device's real systemd, `xochitl.service` never entering a failed state across a switch | **open**, unchanged from ADR-0039 | the acceptance criteria WWW-52 closed against `MemoryPanel` only |
| `tools/device-acceptance/run.sh`'s updated compositor build path, against the real tablet | **open** — the script is written and syntax-checked, not run; it is the §17 acceptance run, not something this ticket's scope includes exercising | the §17 sign-off |

A passing local build is not device qualification, and a linked ELF is not a
screen.

## What would make this wrong

- **If a future component needs `vendor-engine` conditionally** — built with
  it for a device release and without it for, say, a desktop preview build —
  `stage-platform.sh`'s two-path split would need a third case rather than
  the binary "four plain, one vendor" split this ADR makes. Nothing today
  needs that; `paperclip-compositor` is the only component for which the
  feature is anything other than always-off.
- **If `platform/compositor` grows a second binary that does not link the
  vendor engine** — `build.rs` here is entirely gated on the `vendor-engine`
  feature already, so this is expected to be safe, but it has not been
  exercised.
