# paperclip

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
cargo test --workspace
cargo run -p paperctl -- preview --screen home     # h · c · f · n · esc
cargo run -p paperctl -- screenshot --screen all --out-dir artifacts
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
| `apps/home` | The home screen |
| `apps/app-store` | The App Store: installed, offered, and installing it |
| `apps/chess` | The Chess screen |
| `tools/paperctl` | The command line, including `paperctl stock` |
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
- [Assumptions](docs/assumptions.md) — what Stage 1 bet on, and where each bet lives
- [Recovery](docs/recovery.md) — stock Xochitl policy, the states, and what to do with a tablet that is misbehaving
- [Isolation](docs/isolation.md) — what §11 enforces, what it does not, and what is merely accepted
- [Specification](docs/spec.md) — where the authoritative spec lives
- [ADRs](docs/adr/) — decisions and what would make them wrong

## Two rules

From the specification, quoted because they decide when anything here is
finished:

- A running process is not proof the screen is usable.
- Passing mocks or local tests is not device qualification.
