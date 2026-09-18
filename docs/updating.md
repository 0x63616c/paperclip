# Updating and removing Paperclip

Two operations, both driven from the Mac over SSH with `paperctl`. Apps update
through the App Store and are not this document; this is Paperclip itself — the
Host, the compositor, Home, the App Store and Settings, which ship as **one
release** and move together (§13).

> **Not yet run on hardware.** Everything here is implemented and exercised in
> the Linux VM harness (`docs/device/www-8-upgrade-harness.md`; WWW-41 added
> the app-install and active-session cases). No platform upgrade has been
> performed on the tablet. `tools/device-acceptance/run.sh` is the §17
> acceptance run this note refers to — written, not yet executed against the
> real device. Treat the procedure as untested on the device until it has.

## Setting a tablet up

Reach the tablet through the `remarkable-wifi` SSH alias, not an IP — the
tablet is on DHCP and its address moves; the alias doesn't.

```sh
ssh remarkable-wifi /home/root/paperclip/bin/paperctl setup
```

Staged, and it stops at the first thing that is not there. It checks the
prerequisites rather than assuming that because SSH worked the tablet can draw:
systemd and its version, that `xochitl.service` is loaded under the name the
generated units reference, that `/run` is a tmpfs, what the sandbox actually
enforces, whether the vendor display library is where WWW-1 recorded it, and
whether the wakelock exists.

Running it twice changes nothing the second time. `--check` reports without
creating anything. It does not enable Developer Mode and does not downgrade
firmware; neither is implemented, and there is no code path in it that writes
outside `/home/root/paperclip`.

Copy the public key into `/home/root/paperclip/keys/` before the first upgrade
— setup says so when it is missing.

## The first upgrade on a tablet

Nothing to diff against yet: `paperctl upgrade status` reports `current
none`, because there is no fallback release. `tools/device-acceptance/run.sh`
asserts `--from` against that same output, so a first-ever install passes
`--from none` — the literal string, matching the literal `none` the status
line prints. No bootstrap step beyond `setup` above is needed; `upgrade run`
does not care whether the release it is replacing is a real version or
nothing.

Before spending the tablet's start budget on a real attempt, run the same
script with `--check-only --from none` (plus `--device`, if not
auto-discovered). It evaluates the four preconditions — reachable, start
budget, display free, `--from` matches — and prints a verdict for each
without touching the tablet: no build, no upgrade, no `paperctl stock`. See
`tools/device-acceptance/run.sh`'s own usage comment for the full flag.

What has to be built and signed on the Mac before a first install, and what
still has to reach the tablet by hand: `docs/device/www-69-preflight.md`.

## What a release is

One version, one signed manifest, five binaries:

```
platform.toml        version, protocol, state versions, a digest per component
platform.toml.sig    signed with the publishing key, under its own domain
bin/paperclip-host
bin/paperclip-compositor
bin/home
bin/app-store
bin/settings
```

They are one tested release on purpose. A Host from 0.4.0 running a Home from
0.3.1 is a combination nobody tried.

## Building a release

`release.toml`, at the repository root, declares the version this build is —
version, protocol, and the two state numbers below (WWW-61, ADR-0026).
Bumping it is the only thing needed to ask for a new platform release; it is
not the crate version, which stays `0.1.0` from `[workspace.package]`.

`./tools/stage-platform.sh [output-dir]` does the steps below in one call and
is what to run in practice; they are spelled out here for what each one does.
Four of the five components link no vendor code and cross-compile with the
`zig cc` wrapper already configured for the device target. `paperclip-
compositor` is the exception (WWW-86): it is the one process that opens the
panel (ADR-0039), so it must be built with `vendor-engine` or it silently
falls back to `MemoryPanel` — and linking the vendor C++ ABI needs
`tools/cross/build-device.sh`'s Docker/Qt-headers path, not `zig cc`
(ADR-0009; the symbol-mangling reason is in that script's own doc comment).

On the Mac, with the publishing key:

```sh
cargo build --release --target aarch64-unknown-linux-gnu \
    -p paper-host -p paper-home -p paper-app-store -p paper-settings
./tools/cross/build-device.sh --bin paperclip-compositor paper-compositor

mkdir -p build/bin
cp target/aarch64-unknown-linux-gnu/release/paperclip-host  build/bin/
cp target/aarch64-unknown-linux-gnu/release/home            build/bin/
cp target/aarch64-unknown-linux-gnu/release/app-store       build/bin/
cp target/aarch64-unknown-linux-gnu/release/settings        build/bin/
cp target/device-container/release/paperclip-compositor     build/bin/

paperctl upgrade package \
    --source build \
    --key ~/.paperclip/paperclip.key \
    --out paperclip-0.4.0.tar.gz
```

`--version`, `--protocol`, `--state-version` and `--rollback-to-state` all
override the matching field in `release.toml` when given, for a one-off build;
without them the file is what settles what this release is.

If the change to the platform's persistent state is **not** backward
compatible, bump `release.toml`'s `[release.state]` so that `writes` and
`readable_back_to` are equal — this is what makes a rollback safe:

```toml
[release.state]
writes = 2
readable_back_to = 2
```

`readable_back_to` is the lowest state version that can still read what this
release writes. Leaving it equal to `writes` tells the tablet to snapshot its
state before activating, so going back is going back rather than running the
old code over bytes it cannot parse. The two are spelled out rather than
shortened, on purpose (ADR-0026): they are one editing mistake apart, and a
name that says what each one means is harder to swap by accident than a
position in an argument list ever was.

## What needs publishing

```sh
cargo xtask plan-release
```

Reconciles every `apps/<app>/paper.toml` version and `release.toml`'s version
against what GitHub already lists as released, and reports which ones need
building — never a `git diff`, so a force-push, a multi-commit push, a re-run
of a red build, or a revert all reconcile the same way. Publishing nothing is
the normal case and exits `0`; a declared version already published under
different content exits non-zero, because that is a mistake to see rather than
skip. `--format json` gives a workflow step something to parse. See
`xtask/src/plan_release.rs`'s module doc and ADR-0026 for the tag and digest
convention it reads.

## Installing it

```sh
scp paperclip-0.4.0.tar.gz remarkable-wifi:/tmp/
ssh remarkable-wifi /home/root/paperclip/bin/paperctl upgrade run \
    /tmp/paperclip-0.4.0.tar.gz --trust /home/root/paperclip/keys/paperclip.pub
```

What happens, in order:

1. The bundle is unpacked into staging and every component checked against the
   signed manifest — size, digest, and that it is a binary this tablet can run.
   Nothing that is running has been touched yet.
2. If a state snapshot is needed, it is taken.
3. The display goes back to stock and the old Host exits.
4. `previous` and `current` are moved.
5. The new Host is started and watched until it reports itself ready — not
   until its process exists, and not until systemd calls the unit active.
6. It committed, or it rolled back.

A healthy upgrade prints one line:

```
Paperclip 0.3.1 -> 0.4.0: ready in 3.2s
```

A refused one prints two, and names the step the new release never got past:

```
0.4.0 was refused: reached `protocol`, never reached `device-adapter`
0.3.1 is back: ready in 2.9s
```

Nothing is retried. If the previous release does not come back either, the
tablet is left showing stock — reachable over SSH, with `paperctl upgrade
status` saying where things stand.

## If the power goes out mid-update

Turn the tablet back on. It comes up as stock, because nothing Paperclip
installs survives a reboot. Then:

```sh
ssh remarkable-wifi /home/root/paperclip/bin/paperctl upgrade reconcile
```

An update that had not finished is undone, not resumed — the tablet goes back
to the release that was working. Run it before anything else after an
interruption; it is safe when nothing was interrupted.

## Going back deliberately

```sh
ssh remarkable-wifi /home/root/paperclip/bin/paperctl upgrade rollback \
    --trust /home/root/paperclip/keys/paperclip.pub
```

Only the previous release is kept, so this goes back one step. Further back
means installing that bundle again.

## Checking what is on there

```sh
ssh remarkable-wifi /home/root/paperclip/bin/paperctl upgrade status
```

```
current   0.4.0
previous  0.3.1
installed 0.3.1, 0.4.0
last      0.3.1 -> 0.4.0: commit (reached `ready`)
```

## Updating `paperctl` itself

`paperctl` is the command that rescues a stuck tablet, so it is deliberately
outside everything an ordinary update replaces, and replacing it is its own
step:

```sh
scp paperctl remarkable-wifi:/tmp/
ssh remarkable-wifi /home/root/paperclip/bin/paperctl upgrade bootstrap /tmp/paperctl
```

The outgoing one is kept beside it as `bin/paperctl.previous`. If the new one
turns out to be wrong, `mv` it back over SSH.

## Removing Paperclip

Return the display to stock first, then ask what removal would do:

```sh
ssh remarkable-wifi /home/root/paperclip/bin/paperctl stock
ssh remarkable-wifi /home/root/paperclip/bin/paperctl remove
```

Nothing is deleted without `--yes`; the first run prints the list. **Notebooks
are never touched** — they are Xochitl's, in a different directory, and nothing
in the removal path can name them. **App data is kept** unless you ask for it:

```sh
ssh remarkable-wifi /home/root/paperclip/bin/paperctl remove --yes
ssh remarkable-wifi /home/root/paperclip/bin/paperctl remove --yes --remove-app-data
```

After a removal, reboot. The tablet comes up as it did before Paperclip was
ever installed: nothing Paperclip writes ever goes on the root filesystem, and
the runtime units live in `/run`, which a boot clears.

## The two rules this document depends on

- **Stock Xochitl is started and stopped, never killed.** A *failed*
  `xochitl.service` drops the tablet onto a serial console with nothing on the
  screen. No command here can do it.
- **Nothing is installed on the root filesystem.** Binaries live under
  `/home/root/paperclip`, which survives an OS update; units live in `/run`,
  which does not survive a reboot. That is deliberate in both directions.
