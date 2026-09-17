# ADR-0001 — Workspace layout and crate naming

**Status:** accepted (Stage 1, WWW-2)

## Context

§7 fixes the destination layout: `platform/{host,updater,sdk,protocol,packages,device}`,
`apps/{home,app-store,chess}`, `tools/paperctl`, `tests/system`,
`packaging/systemd`, `docs/`. Stage 1 needs a workspace that grows into that
without either inventing a different shape per stage or committing a dozen
empty crates that exist only to be filled in later.

Two smaller questions came with it. Crate names need to be unambiguous inside
the workspace without colliding with crates.io. And §7 warns that a `tests/`
directory at a *virtual* workspace root is not automatically a Cargo
integration-test target.

## Decision

A virtual workspace at the root, with crates created as their functionality
lands. Stage 1 creates `platform/protocol`, `platform/packages`,
`platform/sdk`, `apps/home`, `apps/chess`, `tools/paperctl` and
`tests/system`. The rest of the §7 layout is documented in
`docs/development.md` as the destination and left uncreated.

Crates are named `paper-*` — `paper-sdk`, `paper-packages`, `paper-chess` —
with `paperctl` keeping the name §4 gives it. Types inside them are *not*
prefixed: `Canvas`, `Manifest`, `ProtocolVersion`. §7's "no `paperclip_`
prefixes on everything" is about identifiers, and a crate name is already the
namespace that would make a prefix redundant.

`tests/system` is a package with an explicitly declared `[[test]]` target and
`autotests = false`.

Shared configuration lives in `[workspace.package]`, `[workspace.dependencies]`
and `[workspace.lints]`. `Cargo.lock` is committed. The toolchain is pinned in
`rust-toolchain.toml`.

## Consequences

- A new crate is a deliberate act with a reason, not a directory that has been
  sitting empty since Stage 1.
- One place to bump a dependency, one place to change a lint.
- `paper-sdk` is declared with `default-features = false` at the workspace
  level, so `features = ["desktop"]` has to be asked for. Only `paperctl` asks.
  An app cannot accidentally pull in a windowing stack.
- `tests/system` compiling is visible in `cargo test` output as
  `Running cases/system.rs`. A suite that silently never ran would be worse
  than no suite.

## What would make this wrong

If the device build turns out to need a fundamentally different crate graph —
for example if `platform/device/native` cannot be a leaf — the layout changes.
Nothing about Stage 1 makes that expensive.
