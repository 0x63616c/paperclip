# paperclip

[![CI](https://github.com/0x63616c/paperclip/actions/workflows/ci.yml/badge.svg)](https://github.com/0x63616c/paperclip/actions/workflows/ci.yml)

Personal application environment for the reMarkable Paper Pro: home screen,
independent apps, private App Store, and a Rust dev workflow driven from a Mac.

**Status: Stage 4.** A Cargo workspace, the `paper.toml` package manifest, a
desktop preview at the target panel geometry, and the supervisor that makes a
custom foreground session recoverable — a state machine with enforced
deadlines, generated runtime-only systemd units, and an independent path back
to stock. Every §10 failure is demonstrated against real systemd in an aarch64
Linux VM ([`docs/device/www-4-failure-harness.md`](docs/device/www-4-failure-harness.md)).

**Nothing here has run on the tablet.** The VM proves Linux enforcement; it is
not device qualification. See [`docs/assumptions.md`](docs/assumptions.md) for
what that leaves unestablished, and [`docs/isolation.md`](docs/isolation.md)
for what the isolation does and does not actually enforce.

## Quick start

```sh
just ci                                            # everything CI runs, reproduced locally
cargo run -p paperctl -- preview --screen home     # h · c · f · n · esc
cargo run -p paperctl -- screenshot --screen all --out-dir artifacts
cargo run -p paperctl -- open --dry-run                # the frame a device run would present
cargo run -p paperctl -- manifest validate apps/chess
cargo run -p paperctl -- isolation --target paper-pro   # what §11 really enforces
cargo run -p paperctl -- key generate --out-dir ~/.paperclip
```

The failure harness needs a Linux VM, and says so rather than skipping:

```sh
tools/vm-harness/create-vm.sh ~/paperclip-vm
tools/vm-harness/run-harness.sh ~/paperclip-vm
```

## Layout

| Path | What |
|---|---|
| `platform/protocol` | Protocol version identifiers |
| `platform/packages` | `paper.toml`, `.paperpkg` archives, signing, catalogs, the installer |
| `platform/sdk` | Canvas, palette, text, display mapping, input, desktop backend |
| `platform/host` | The supervisor: state machine, deadlines, units, recovery |
| `platform/updater` | The platform update transaction: stage, activate, verify, commit, roll back |
| `apps/home` | The home screen |
| `apps/app-store` | The App Store: installed, offered, and installing it |
| `apps/chess` | The Chess screen |
| `tools/paperctl` | The command line: `stock`, `setup`, `upgrade`, `remove`, packaging |
| `tools/fault-app` | A session that misbehaves to order |
| `tools/vm-harness` | Scripts that build the VM the harness runs in |
| `tests/system` | Cross-crate tests |
| `tests/failure-harness` | The §10 failure harness — a binary, run in the VM |

Crates appear as their functionality lands; the destination layout is in
[`docs/development.md`](docs/development.md).

## Documentation

- [Development](docs/development.md) — build, preview, screenshot, style
- [App contract](docs/app-contract.md) — `paper.toml` and the capability rule
- [Packaging](docs/packaging.md) — building, signing, publishing, installing, rolling back
- [Updating](docs/updating.md) — upgrading Paperclip itself, rolling it back, removing it
- [Assumptions](docs/assumptions.md) — what Stage 1 bet on, and where each bet lives
- [Recovery](docs/recovery.md) — stock Xochitl policy, the states, and what to do with a tablet that is misbehaving
- [Isolation](docs/isolation.md) — what §11 enforces, what it does not, and what is merely accepted
- [Specification](docs/spec.md) — where the authoritative spec lives
- [ADRs](docs/adr/) — decisions and what would make them wrong
- [Domain glossary](CONTEXT.md) — terms this codebase uses with a specific meaning
- [Contributing](CONTRIBUTING.md) — the loop, style, and what `just ci` runs
- [Security policy](SECURITY.md) — what is signature-verified, and how to report a vulnerability
- [Changelog](CHANGELOG.md)

## Two rules

From the specification, quoted because they decide when anything here is
finished:

<!-- ANCHOR: two-rules -->
- A running process is not proof the screen is usable.
- Passing mocks or local tests is not device qualification.
<!-- ANCHOR_END: two-rules -->
