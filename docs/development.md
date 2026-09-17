# Development

Everything here runs on the Mac. There is no device path yet; see
`recovery.md` for what exists on that side and what does not.

## Prerequisites

`rustup`. The toolchain is pinned in `rust-toolchain.toml` and installs itself
on first build. Nothing else — no system libraries, no GPU, no package
manager.

## The loop

```sh
cargo test --workspace                       # everything
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

All three must pass before anything is pushed. Clippy is run with
`-D warnings` deliberately: the workspace lints in `Cargo.toml` are the style
rules from §7, and a warning nobody has to fix is a rule nobody follows.

## Seeing a screen

```sh
# Open the preview window
cargo run -p paperctl -- preview --screen home
cargo run -p paperctl -- preview --screen chess

# Keys: h home · c chess · f flip the board · n clear selection · esc quit
```

The window opens at a fraction of the real panel size — 1620 × 2160 portrait
does not fit on a laptop screen — and the canvas is scaled and centred inside
it. Presses are mapped back through the same fit, so a click lands where it
looks like it lands. Resize the window and it stays correct; a press on a
letterbox bar reaches nothing rather than being clamped to the nearest edge.

## Screenshots

Offscreen, no window, full target resolution:

```sh
cargo run -p paperctl -- screenshot --screen all --out-dir artifacts
```

From the live window, which also captures the letterbox and so is the evidence
that the fit is right:

```sh
cargo run -p paperctl -- preview --screen home \
  --window-size 1000x760 \
  --capture-canvas artifacts/home.png \
  --capture-window artifacts/home-window.png \
  --exit-after-capture
```

`--window-size` takes logical points and overrides the default sizing; give it
a shape that does not match the canvas to see the bars.

`artifacts/` is git-ignored. Screenshots that matter belong attached to an
issue, not committed.

## Manifests

```sh
cargo run -p paperctl -- manifest validate apps/chess
cargo run -p paperctl -- manifest validate apps/chess --payload
```

`--payload` additionally checks that the entrypoint and every declared asset
exists. It will fail in a source tree, because `bin/chess` is a build output
and not a file anybody commits. That is the correct answer: `--payload` is for
a staged package, not a source directory.

## Layout

```
platform/protocol   protocol version identifiers
platform/packages   paper.toml: parsing, validation, capability grants
platform/sdk        canvas, palette, text, display mapping, input, desktop backend
apps/home           the home screen
apps/chess          the Chess screen
tools/paperctl      the command line
tests/system        cross-crate tests
docs/               this, plus ADRs
```

Crates appear as their functionality lands. The full §7 layout —
`platform/host`, `platform/updater`, `platform/device`, `apps/app-store`,
`packaging/systemd` — is the destination, and creating those directories empty
now would just be scaffolding to delete later.

`tests/system` is a **package**, not a bare directory. §7 warns that a `tests/`
directory at a virtual workspace root is not automatically a Cargo
integration-test target; it would silently never compile. The target is
registered explicitly in `tests/system/Cargo.toml`, with `autotests = false` so
nothing is picked up by accident.

## Style

The rules are §7's, and the workspace lint table enforces the ones a linter can
see. The rest, briefly:

- Concrete structs and enums. No trait-per-struct, no generic plugin
  factories, no global context object.
- Private modules by default; a crate's public surface is its `lib.rs`
  re-exports.
- `Result` for failure, with typed errors that name the field and say what was
  wrong with it.
- No `paperclip_` prefix on things that are already inside a Paperclip crate.
