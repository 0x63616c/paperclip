# Development

Everything here runs on the Mac. Code for the tablet *cross-compiles* here too
— see "Building for the tablet" — but nothing in this repository has run on the
tablet. See `recovery.md` for what exists on that side and what does not.

## Prerequisites

`rustup`. The toolchain is pinned in `rust-toolchain.toml` and installs itself
on first build, including the `aarch64-unknown-linux-gnu` target. Nothing else
— no system libraries, no GPU, no package manager.

For cross-*linking* (not just checking), one more: `brew install zig`. For
anything that links Qt, the reMarkable SDK as well; see
`platform/device/native/README.md`.

## The loop

```sh
cargo test --workspace                       # everything
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

All three must pass before anything is pushed. Clippy is run with
`-D warnings` deliberately: the workspace lints in `Cargo.toml` are the style
rules from §7, and a warning nobody has to fix is a rule nobody follows.

## Building for the tablet

The device triple is `aarch64-unknown-linux-gnu` — glibc, not musl (WWW-1).

```sh
# Does the device half compile? Needs nothing but rustup.
cargo check --workspace --target aarch64-unknown-linux-gnu

# A real aarch64 binary. Needs zig.
cargo build --release -p paper-device --example device-report \
    --target aarch64-unknown-linux-gnu
```

`.cargo/config.toml` points the linker at `tools/cross/aarch64-linux-gnu-cc`, a
two-line wrapper around `zig cc`. That is deliberately *not* the reMarkable
SDK: zig ships its own glibc stubs and cross-links aarch64 from a bare Mac, so
"does this build for the tablet" is answerable in seconds rather than after a
multi-gigabyte SDK install. It targets glibc 2.36 against the device's 2.39,
because linking against an older glibc runs on a newer one and the reverse is
the `GLIBC_2.39 not found` failure that only appears on the tablet.

Pass `-p paper-device` rather than building the whole workspace: without it,
Cargo unifies features across the workspace and drags the desktop preview's
windowing stack into a device build, which is not something the tablet wants.

The one thing zig cannot do is link Qt. `platform/device`'s `vendor-engine`
feature needs the SDK's Qt 6.10.3 headers and a copy of `libqsgepaper.so` from
the tablet — proprietary, never committed. `platform/device/native/README.md`
has the procedure, and `tools/device-probe/pull-vendor-lib.sh` is the
read-only copy.

### Running the readiness report on the tablet

```sh
cargo build --release -p paper-device --example device-report \
    --target aarch64-unknown-linux-gnu
scp target/aarch64-unknown-linux-gnu/release/examples/device-report \
    remarkable-wifi:/home/root/paperclip/
ssh remarkable-wifi /home/root/paperclip/device-report
```

It resolves the pen and touch nodes by advertised capability, checks the
wakelock is writable, and reports the vendor display locks. It writes nothing
and stops nothing. Redact the machine id and boot id before posting output.

Use **Wi-Fi**, not USB: the USB CDC gadget does not survive the tablet's
autosleep, and Wi-Fi does (WWW-20).

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
platform/device     the tablet adapter: waveform presentation, evdev, session locks
platform/device/native  the only C++: a C ABI over the vendor waveform engine
apps/home           the home screen
apps/chess          the Chess screen
tools/paperctl      the command line
tools/cross         the zig cc linker wrapper for the device triple
tools/device-probe  one-off device probes and the vendor-library pull script
tests/system        cross-crate tests
docs/               this, plus ADRs
```

Crates appear as their functionality lands. The full §7 layout —
`platform/host`, `platform/updater`, `apps/app-store`, `packaging/systemd` —
is the destination, and creating those directories empty now would just be
scaffolding to delete later.

Nothing above `platform/device` may import Qt types, Linux device paths, SSH,
systemd or Xochitl controls. That surface belongs to the adapter and the host.

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
