# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and every workspace crate follows [SemVer](https://semver.org/) once it is
first published — see `Cargo.toml`'s `workspace.package.version`. Nothing in
this repository has been published yet (`publish = false` throughout); until
then, entries below are grouped by project stage rather than by release tag.

## [Unreleased]

### Added

- CI (`.github/workflows/ci.yml`): format, clippy, tests, a rustdoc
  broken-intra-doc-link gate, an `aarch64-unknown-linux-gnu` type-check, a
  `paperctl --no-default-features` build, a `cargo-hack` feature matrix, and
  an assertion that the device build carries no signing symbol (§12).
- `justfile` and `cargo xtask`, replacing the workflows that were previously
  spread across `tools/cross/build-device.sh`, `tools/vm-harness/*.sh`, and
  `docs/development.md` alone.
- `LICENSE` (MIT, matching `Cargo.toml`'s declared license), `SECURITY.md`,
  `CONTRIBUTING.md`, this file, and `CONTEXT.md`.
- Fuzz targets for the two hostile-input surfaces named in `platform/packages`'
  own module docs: `.paperpkg` archive extraction and the `paper.toml`
  parser (`platform/packages/fuzz`).
- `release.toml`, declaring the platform's own version, protocol and state
  numbers — `paperctl upgrade package` now reads it, with its CLI flags as
  per-field overrides — and `cargo xtask plan-release`, which reconciles
  declared app and platform versions against what GitHub already lists as
  published and reports what needs publishing (WWW-61, ADR-0026).

### Fixed

- Several stale rustdoc intra-doc links and one stale doc comment (`takeover.rs`
  said "Three layers" after ADR-0011 was amended to four).

Everything before this point predates a changelog and is described by the
project's own issue history and `docs/adr/`, not reconstructed here.
