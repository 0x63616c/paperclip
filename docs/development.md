# Development

Most of this runs on the Mac, and the tablet's code cross-compiles here — see
"Building for the tablet". One thing does not: the supervisor's *enforcement*
is only testable on Linux, so `tests/failure-harness` runs in a VM and refuses
to run anywhere else. Its *decisions* are tested on the Mac like everything
else, and the two are kept apart on purpose.

Nothing in this repository has run on the tablet. See `recovery.md` for what
exists on that side and what does not, and `isolation.md` for what the
isolation really enforces.

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
cargo run -p paperctl -- preview --screen settings

# Keys: h home · c chess · s settings · f flip the board · n clear selection
#       · tab next screen · esc quit
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

`--screen settings` writes one file per Settings page plus one confirmation
dialog (`settings-confirm-uninstall.png`), reached through the same
`SettingsScreen` methods a real tap would use rather than a hand-built dialog.

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

## The supervisor

```sh
# What a platform actually enforces (§11). Works on a Mac for the recorded
# device profile; `--target here` needs Linux and refuses elsewhere.
cargo run -p paperctl -- isolation --target paper-pro
cargo run -p paperctl -- isolation --target paper-pro --markdown

# The runtime units a session would be given.
cargo run -p paperctl -- units --target paper-pro
```

The device build of `paperctl` has no windowing stack, because the tablet has
none — no libEGL, no Mesa, no GPU (WWW-1 §3):

```sh
cargo build -p paperctl --no-default-features   # what ships to /home/root/paperclip/bin
```

`paperctl stock` — the independent recovery path — exists only in a Linux
build. On a Mac it says so rather than pretending.

## The failure harness

Every §10 recovery row, caused for real against systemd, in an aarch64 Linux
VM. It is a binary rather than a `cargo test` target so that it cannot quietly
skip on a Mac and leave a green line behind.

```sh
brew install qemu
tools/vm-harness/create-vm.sh ~/paperclip-vm
tools/vm-harness/run-harness.sh ~/paperclip-vm
```

About three minutes for the VM, about a minute for the run. It writes
`docs/device/www-4-failure-harness.md`, which names the kernel and systemd
build it came from.

```sh
~/paperclip-vm/vmsh 'sudo ~/bin/paperclip-failure-harness --bin-dir ~/bin --list'
~/paperclip-vm/vmsh 'sudo ~/bin/paperclip-failure-harness --bin-dir ~/bin --case app-hang'
```

**A container will not do**, however convenient. LXC — which is what OrbStack
and Docker run systemd inside — installs a generator that disables every
sandboxing directive for every service on the machine, so the harness reports
that nothing is enforced and cannot tell that from a broken unit file.

## Manifests

```sh
cargo run -p paperctl -- manifest validate apps/chess
cargo run -p paperctl -- manifest validate apps/chess --payload
```

`--payload` additionally checks that the entrypoint and every declared asset
exists. It will fail in a source tree, because `bin/chess` is a build output
and not a file anybody commits. That is the correct answer: `--payload` is for
a staged package, not a source directory.

## Packaging and catalogs

```sh
cargo run -p paperctl -- key generate --out-dir ~/.paperclip
cargo run -p paperctl -- package apps/chess --out build/chess.paperpkg
cargo run -p paperctl -- publish build/chess.paperpkg \
    --catalog ~/catalogs/home --key ~/.paperclip/paperclip.key
cargo run -p paperctl -- install dev.calum.chess \
    --catalog ~/catalogs/home --trust ~/.paperclip/paperclip.pub --root /tmp/store
```

The whole workflow, the catalog layout, and how to reproduce each §15 scenario
by hand are in [`packaging.md`](packaging.md).

## Layout

```
platform/protocol   protocol version identifiers
platform/packages   paper.toml, .paperpkg archives, signing, catalogs, the installer
platform/sdk        canvas, palette, text, display mapping, input, desktop backend
platform/device     the tablet adapter: waveform presentation, evdev, session locks
platform/device/native  the only C++: a C ABI over the vendor waveform engine
platform/host       the supervisor: state machine, deadlines, units, recovery
apps/home           the home screen
apps/chess          the Chess screen
apps/settings       the Settings screen: apps, storage, grants, catalog, platform, diagnostics
tools/paperctl      the command line, including `paperctl stock`
tools/fault-app     a session that misbehaves to order, for the harness
tools/vm-harness    scripts that build the VM the harness runs in
tools/cross         the zig cc linker wrapper for the device triple
tools/device-probe  one-off device probes and the vendor-library pull script
tests/system        cross-crate tests
tests/failure-harness  the §10 failure harness (a binary, run in the VM)
docs/               this, plus ADRs
```

Crates appear as their functionality lands. The rest of the §7 layout —
`platform/updater`, `apps/app-store` — is the destination, and creating those
directories empty now would just be scaffolding to delete later.
`packaging/systemd` will stay empty permanently: units are generated at session
start into `/run/systemd/system` and nothing is packaged for the root
filesystem (ADR-0008).

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
