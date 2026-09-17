# paperclip

Personal application environment for the reMarkable Paper Pro: home screen,
independent apps, private App Store, and a Rust dev workflow driven from a Mac.

**Status: Stage 1.** A Cargo workspace, the `paper.toml` package manifest, and
a desktop preview that renders the home screen and a chess board at the target
panel geometry. Nothing here has run on a tablet — see
[`docs/assumptions.md`](docs/assumptions.md) for what that leaves unestablished.

## Quick start

```sh
cargo test --workspace
cargo run -p paperctl -- preview --screen home     # h · c · f · n · esc
cargo run -p paperctl -- screenshot --screen all --out-dir artifacts
cargo run -p paperctl -- manifest validate apps/chess
```

## Layout

| Path | What |
|---|---|
| `platform/protocol` | Protocol version identifiers |
| `platform/packages` | `paper.toml`: parsing, validation, capability grants |
| `platform/sdk` | Canvas, palette, text, display mapping, input, desktop backend |
| `apps/home` | The home screen |
| `apps/chess` | The Chess screen |
| `tools/paperctl` | The command line |
| `tests/system` | Cross-crate tests |

Crates appear as their functionality lands; the destination layout is in
[`docs/development.md`](docs/development.md).

## Documentation

- [Development](docs/development.md) — build, preview, screenshot, style
- [App contract](docs/app-contract.md) — `paper.toml` and the capability rule
- [Assumptions](docs/assumptions.md) — what Stage 1 bet on, and where each bet lives
- [Recovery](docs/recovery.md) — stock Xochitl policy; **not yet exercised on hardware**
- [Specification](docs/spec.md) — where the authoritative spec lives
- [ADRs](docs/adr/) — decisions and what would make them wrong

## Two rules

From the specification, quoted because they decide when anything here is
finished:

- A running process is not proof the screen is usable.
- Passing mocks or local tests is not device qualification.
