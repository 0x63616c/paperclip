# Updating and removing Paperclip

Two operations, both driven from the Mac over SSH with `paperctl`. Apps update
through the App Store and are not this document; this is Paperclip itself — the
Host, Home, the App Store and Settings, which ship as **one release** and move
together (§13).

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

## What a release is

One version, one signed manifest, four binaries:

```
platform.toml        version, protocol, state versions, a digest per component
platform.toml.sig    signed with the publishing key, under its own domain
bin/paperclip-host
bin/home
bin/app-store
bin/settings
```

They are one tested release on purpose. A Host from 0.4.0 running a Home from
0.3.1 is a combination nobody tried.

## Building a release

On the Mac, with the publishing key:

```sh
cargo build --release --target aarch64-unknown-linux-gnu \
    -p paper-host -p paper-home -p paper-app-store -p paper-settings

mkdir -p build/bin
cp target/aarch64-unknown-linux-gnu/release/paperclip-host build/bin/
cp target/aarch64-unknown-linux-gnu/release/home           build/bin/
cp target/aarch64-unknown-linux-gnu/release/app-store      build/bin/
cp target/aarch64-unknown-linux-gnu/release/settings       build/bin/

paperctl upgrade package \
    --source build --version 0.4.0 \
    --key ~/paperclip-keys/paperclip.key \
    --out paperclip-0.4.0.tar.gz
```

If the change to the platform's persistent state is **not** backward
compatible, say so — this is what makes a rollback safe:

```sh
    --state-version 2 --rollback-to-state 2
```

`--rollback-to-state` is the lowest state version that can still read what this
release writes. Leaving it equal to `--state-version` tells the tablet to
snapshot its state before activating, so going back is going back rather than
running the old code over bytes it cannot parse.

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
