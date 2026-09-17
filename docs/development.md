# Development

Most of this runs on the Mac, and the tablet's code cross-compiles here — see
"Building for the tablet". One thing does not: the supervisor's *enforcement*
is only testable on Linux, so `tests/failure-harness` runs in a VM and refuses
to run anywhere else. Its *decisions* are tested on the Mac like everything
else, and the two are kept apart on purpose.

The display takeover, rendering and input handling have run on the tablet
(WWW-20). See `docs/device/www-20-notes.md` for observations from that session.

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
cargo run -p paperctl -- preview --screen sudoku

# Keys: h home · c chess · s settings · u sudoku · f flip the board
#       · n clear selection · tab next screen · esc quit
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

`paperctl stock` — the independent recovery path — has its real implementation
only in a Linux build. Typed on a Mac it is a forward to the tablet's own copy
over SSH (WWW-33, below); a Linux build with no tablet to forward to (the VM
harness) says so rather than pretending.

## Reaching the tablet from the Mac (WWW-33)

`open`, `stock`, `setup`, `install`, `upgrade` and `remove` are the six
subcommands that touch the tablet. Typed on a Mac, each one resolves which
tablet to talk to and runs the same command against the copy of `paperctl`
already installed there over SSH — the same thing as SSHing in and typing it
directly, done for you. Typed on the tablet itself, each one runs locally, as
it always has; nothing about that path changed.

Resolution order, highest precedence first:

1. `--device <host>` on the command itself.
2. `PAPERCTL_DEVICE` in the environment.
3. A pinned device (`paperctl devices pin <host>`), which skips every source
   below it — a pin is a promise to stop guessing, not a promise the tablet
   is awake right now.
4. USB ethernet, `10.11.99.1` — Wi-Fi is still what survives autosleep
   (WWW-20); USB is checked first only because it needs no discovery.
5. mDNS on the LAN, `_paperctl._tcp.local.` — proposed, not yet validated on
   hardware: nothing on the device side advertises this service yet, so
   expect this source to find nothing until a later stage adds it.
6. The cached last-good host — whichever of the above last resolved
   successfully, remembered for next time.

`open`, `stock` and `setup` auto-discover with no flag needed; a `paperctl
open` typed on a Mac with no tablet reachable fails within a few seconds,
naming every source it tried. `install`, `upgrade run`/`rollback` and `remove`
only reach the device when `--device` is given explicitly — bare, they still
mean "here," exactly as before, so a scratch `--root` on the Mac for local
package testing keeps working unchanged.

```sh
paperctl devices                     # what auto-discovery found
paperctl devices --output json
paperctl devices pin remarkable-wifi # skip discovery until unpinned
paperctl devices unpin
```

The pin and the cached last-good host live in one file, `paperctl devices
--help` prints exactly where (`~/.config/paperctl/config.toml`, or
`$PAPERCTL_CONFIG_DIR`/`$XDG_CONFIG_HOME` if set).

## Logs, doctor and deploy (WWW-34)

Three commands that used to be shell aliases on Calum's Mac, kept out of this
repository and out of the resolution order above — none of them run on the
device.

```sh
paperctl logs                # the newest recorded run (open, deploy)
paperctl logs --list         # every retained run
paperctl logs --last 2       # the one before the newest
paperctl logs --output json

paperctl doctor              # can the tablet be reached, and is it healthy
paperctl doctor --output json

paperctl deploy               # cross-compile paperctl and install it on the tablet
paperctl deploy --dry-run     # the build and install commands, no device touched
```

`logs` reads the run history `open` and `deploy` write; run records live
under the same directory as the device config (`runs/`, next to
`config.toml`). It fails, naming the path it looked in, when nothing is
retained yet — never a silent empty list.

`doctor` resolves a device through the same order as `paperctl devices`, but
with its own short reachability budget rather than the 30s one `open` and
`stock` need to wake a sleeping tablet (WWW-35) — a diagnostic has to answer
inside its own 15s failure bound, so a sleeping tablet reads as unreachable
here rather than waiting to find out. It exits 0 when healthy, a distinct
nonzero when the tablet cannot be reached at all, and another distinct
nonzero when it is reachable but degraded (`NRestarts > 0`, the vendor
display lock held, or the on-device `paperctl` missing or a different
version).

`deploy` is the dev-loop replacement for building `paperctl` by hand and
copying it into place — see the next section, which now runs it instead of
the manual `scp`.

## First light: a screen on the actual panel

`paperctl open` renders the screen through the same code `screenshot` uses,
stops Xochitl, presents through the vendor waveform engine, holds the image,
clears the panel, gives the display back and re-reads stock's health — all on
the tablet, reached from the Mac through the transport above. A long hold
runs detached on the device side rather than tied to the SSH session that
started it, because a live session does not reliably survive one (WWW-23).

```sh
# On the Mac: build it for the tablet, with the vendor engine linked, and
# install it — the binary has to exist on the tablet before anything can
# forward to it. The very first time, `mkdir -p /home/root/paperclip/bin`
# over SSH first; `deploy` installs into that directory, it does not create it.
paperctl deploy

# From the Mac, from here on.
paperctl open              # Home, held 60s, on whichever tablet resolves
paperctl open --hold 300   # long enough to photograph
paperctl open --device remarkable-wifi --hold 300   # a specific tablet
```

Nothing installs on the root filesystem: the binary lives under
`/home/root/paperclip`, and `open` writes no units at all (§13, ADR-0008).

On a Mac, `--dry-run` does the render, the digest and the swap it would ask for,
and touches nothing:

```sh
cargo run -p paperctl -- open --dry-run
```

The digest is the point of comparing the two. It is a SHA-256 of the exact
ARGB8888 the engine is handed, so "tonight's run and this morning's presented
the same bytes" is checkable in a way "it looked the same" is not.

**What a successful run does and does not claim.** It claims the engine
accepted the buffer, what the connector and rails said while it was up, and
that stock came back with `NRestarts` unmoved. It does not claim the image was
right — nothing in the process can see the glass, and `open` says
`presented without error, appearance unverified` rather than something warmer
(§17). The one thing it does check about the pixels is that they are not blank:
a frame that rasterised to bare background refuses the takeover, because an
empty panel and a perfect one produce identical logs from this side.

**Exit codes, since WWW-31.** `open` exits non-zero when the readback cannot
account for the frame — when no engine buffer comes back holding the pixels
that were sent, or when the presenting half reported no verdict at all. That is
reported separately from a stock regression, because the tablet is healthy and
what failed is the evidence. Before WWW-31 the verdict was printed and then
dropped, which is how every present orphaned by the WWW-29 detach exited 0. A
zero exit still means only what the paragraph above says: the engine holds the
bytes, not that the panel changed.

**Golden frames.** `cargo test -p paperctl` freezes the digest of all four
screens, so a takeover starts from a render that is known to be the reviewed
one rather than a fallback or a half-built screen. When a deliberate UI change
moves one, look at `paperctl screenshot`'s PNG, then update the table in
`tools/paperctl/src/screens.rs` in the same commit as the change — a digest
re-blessed in a commit of its own is the assertion switched off.

It also refuses when `xochitl.service` is within two restarts of
`StartLimitBurst=4`, before it takes the wakelock or stops anything — so a
refused run costs the tablet nothing. `NRestarts` is cumulative and does not
decay with the rate-limiter window; `systemctl reset-failed xochitl.service` or
a reboot clears it, and `paperctl` deliberately does neither on its own. See
`docs/device/www-23-first-light.md`.

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

`apps/chess` and `apps/sudoku` are library crates plus a `[[bin]]` entrypoint
(ADR-0022); `package` needs the entrypoint built and staged into `bin/<app>`
before it has a payload to read:

```sh
./tools/package-app.sh chess      # cross-compiles bin/chess, stages it in place
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
platform/updater    the platform update transaction and the removal path
apps/home           the home screen
apps/chess          the Chess screen
apps/chess-rules    chess legality, game state and the save format
apps/sudoku         the Sudoku screen, and the worked example of per-cell damage
apps/sudoku-rules   sudoku generation, validation, solving and the save format
apps/settings       the Settings screen: apps, storage, grants, catalog, platform, diagnostics
tools/paperctl      the command line: stock, setup, upgrade, remove, packaging, logs, doctor, deploy
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
