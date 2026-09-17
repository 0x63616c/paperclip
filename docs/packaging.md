# Packaging, publishing and installing

How an app gets from a directory on the Mac to a selected release in a
Paperclip store. The decisions behind all of this are
[ADR-0013](adr/0013-package-archive-and-the-signed-release-envelope.md) and
[ADR-0014](adr/0014-install-transaction-durability-and-rollback.md).

Everything below runs on the Mac today. Nothing in this document has run on the
tablet.

## The two halves

`paperctl` is split, and the split is the point:

| Publishing — holds a secret key | Installing — holds only public keys |
|---|---|
| `paperctl key generate` | `paperctl install` |
| `paperctl package` | `paperctl list` |
| `paperctl publish` | `paperctl rollback` |
| `paperctl check` | `paperctl recover` |

The signing key never leaves the Mac. The device build of `paper-packages` is
compiled with `default-features = false`, which removes the `publishing`
feature and with it every code path that can hold one.

## One-time setup

```sh
paperctl key generate --out-dir ~/.paperclip
# paperclip.key — mode 0600, stays here
# paperclip.pub — copy this to the tablet
```

The key id printed here is what appears in every signature and in
`paperctl install` output. Regenerating the key makes every release already
published unverifiable on any device that trusts the old one, which is why
`--force` exists and is not the default.

## Publishing a release

```sh
paperctl package apps/chess --out build/chess-0.2.0.paperpkg
paperctl check build/chess-0.2.0.paperpkg          # opens it the way a device would
paperctl publish build/chess-0.2.0.paperpkg \
    --catalog ~/catalogs/home \
    --key ~/.paperclip/paperclip.key \
    --name calum-home
paperctl check ~/catalogs/home --trust ~/.paperclip/paperclip.pub
```

`publish` refuses to replace a version with different bytes. Republishing the
*same* bytes is a no-op that still advances the serial, because a device that
has already seen serial 1 must never be handed another serial 1.

Development builds use prerelease versions — `0.3.0-dev.4` — which are as
immutable as anything else. That is what makes them safe to iterate with.

## Serving a catalog

A catalog is static files:

```text
index.toml              the list, and the serial
index.toml.sig          detached signature
apps/<id>/<version>/release.toml
apps/<id>/<version>/release.toml.sig
apps/<id>/<version>/<leaf>-<version>.paperpkg
```

Any static file server over that directory works. There is no database, no
device-control server, and nothing in the tree that executes.

Signatures are required regardless of transport. Over plain HTTP on the home
LAN the signature is the only thing between the tablet and whoever else is on
the network; over HTTPS it is still the only thing that says *the publisher*
produced this rather than the server. A transport that uses HTTPS must verify
certificates — one that does not is worse than plain HTTP, because it looks
safe.

Only `FileTransport` ships today: a catalog directory, local or mounted. An
HTTP transport arrives with the App Store, which is the first thing that needs
one.

## Installing

```sh
paperctl install dev.calum.chess \
    --catalog ~/catalogs/home \
    --trust ~/.paperclip/paperclip.pub \
    --root /tmp/paperclip-store
```

Without `@<version>` this installs the newest **stable** release. A prerelease
is never chosen for you — name it (`dev.calum.chess@0.3.0-dev.4`) if you want
it. SemVer orders `0.3.0-dev.1` above `0.2.0`, so an "install the newest" path
that did not filter would move a device onto a development build the moment one
was published.

`--root` is the store. Without it, `PAPERCLIP_ROOT`, then the device path
`/home/root/.local/share/paperclip`. There is no current-directory fallback.

```sh
paperctl list --root /tmp/paperclip-store --catalog ~/catalogs/home \
    --trust ~/.paperclip/paperclip.pub
paperctl rollback dev.calum.chess --root /tmp/paperclip-store
paperctl recover --root /tmp/paperclip-store
```

`rollback` selects the previous release and makes the one it rolled back *from*
the new fallback, so it can be undone without going back to the catalog.

`recover` finishes or undoes whatever an interrupted install left behind. It is
safe when nothing is wrong, and the host will run the same code at start.

## What the App Store will show

The App Store *app* is not built yet — it needs the app lifecycle and rendering
contract from WWW-5, which does not exist. What it will show does exist, as
`paper_packages::inventory`:

`Inventory::survey` merges three sources — what is installed, what a catalog
offers, and what has actually managed to start — into one row per app: name,
installed version, available version, fallback, every release on disk, launch
health, and a one-word state. `paperctl list` renders exactly that model, so
the CLI and the App Store cannot drift apart about what an update is.

The states, and why each exists:

| State | Meaning |
|---|---|
| `installed` | Installed, with no catalog to compare against. Not "up to date" — with nothing to compare, that is a claim nothing can support. |
| `up to date` | Installed, and the newest the catalog offers. |
| `update available` | The catalog offers something newer. |
| `available` | Offered, not installed here. |
| `installed; not in the catalog` | The catalog dropped it. It is still installed and still launchable — a catalog does not uninstall anything. |
| `not starting` | Selected, and it has never managed to start. Outranks every other state, because it is the only one needing a person. |
| `nothing selected` | Releases on disk, none selected. An install interrupted before activation. |

Release notes are *not* fetched while surveying. Notes live in per-release
descriptors, so fetching one per row would make opening a list N round trips
over a LAN that may not be there. `Inventory::notes` fetches one, when someone
asks to see one.

Install progress is reported through `install::Progress`, as `Downloading`
(with a total taken from the signed descriptor, so the fraction is trustworthy
before a byte arrives), then `Verifying`, `Extracting`, `Committing`,
`Activating`. Reporting only: a `Progress` cannot cancel or fail an install,
and nothing waits on it. `paperctl install` prints these, which is what keeps
the reporting honest — it has a second consumer.

## The store

```text
<root>/
  apps/<id>/releases/<version>/   an extracted package, immutable
  apps/<id>/current               which version is selected
  apps/<id>/previous              which version to fall back to
  data/<id>/                      the app's own writable state
  shared/<grant>/                 explicit cross-app sharing
  staging/<transaction>/          downloads and extraction in progress
  state/                          journal, locks, catalog cache, launch health
```

Writable data is never inside a release directory. That is what makes a release
directory disposable, and disposable is what makes rollback a selection change
rather than a restore.

A release directory is complete only if it contains `.paperclip-release`. That
file is written last, inside staging, before the tree is fsynced and renamed
into place — so a release directory either has it or is not one. Nothing
without it is listed, selected or launched.

## Reproducing the §15 scenarios

Each of these is covered by a test in `platform/packages/tests/install.rs`;
`cargo test -p paper-packages --test install` runs all of them. The manual
reproductions are here because reading an assertion is not the same as watching
the thing happen.

**Offline.** Install once so the index is cached, then make the catalog
unreachable:

```sh
mv ~/catalogs/home/index.toml ~/catalogs/home/index.toml.away
paperctl list --root /tmp/store --catalog ~/catalogs/home --trust ~/.paperclip/paperclip.pub
# catalog    calum-home serial 2 (stale; cached)
```

The installed app is still listed and still selected. Stale is shown, never
hidden.

**Update available.** Publish a higher version and run `paperctl list`; the
catalog line shows both, and `paperctl install` moves to the newer one and
records the older as the fallback.

**Interrupted download.** Truncate an archive in the catalog:

```sh
truncate -s 100 ~/catalogs/home/apps/dev.calum.chess/0.2.0/chess-0.2.0.paperpkg
paperctl install dev.calum.chess@0.2.0 --catalog ~/catalogs/home \
    --trust ~/.paperclip/paperclip.pub --root /tmp/store
# the download is N bytes; the signed release says M
```

The previously installed release is untouched and still selected, and no
staging directory is left behind.

**Bad signature.** Edit a published descriptor — even by one space:

```sh
sed -i '' 's/size = /size  = /' ~/catalogs/home/apps/dev.calum.chess/0.2.0/release.toml
paperctl install dev.calum.chess@0.2.0 ...
# the paperclip.release.v1 signature from key <id> does not verify
```

Editing the *notes*, or reordering nothing at all, gives the same answer. That
is the scheme working: the signature is over the bytes as published.

**Disk full.** Point the store at a small filesystem, or a read-only one, and
install. The install fails, nothing is committed, and the previous release is
still selected. In the test suite this is injected precisely, as a reader that
returns `ErrorKind::StorageFull` partway through the archive.

**Power loss.** Not reproducible by hand without pulling power. The test builds
the exact on-disk state each interruption leaves — a staging tree with its
journal entry, an activate entry whose target is complete, an activate entry
whose target is missing — and asserts what `recover` does with each.
