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

Two apps ship through the catalog rather than with the platform, and both
follow this identically — Chess and Sudoku. Publishing Sudoku is the same five
commands with a different source directory, which is the point of there being
two of them (WWW-39): "install an app that is not Chess" is now a path with
something in it.

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

`paperctl install` installs under `InstallPolicy::deny_all`, so an app it
installs holds no capabilities — including `storage`. An app that saves (Chess,
Sudoku) therefore resumes nothing when it is launched from a store installed
this way, and both handle that by playing unsaved rather than failing. The
grants an app actually needs are host policy, not a CLI flag, and there is no
flag here to widen them on purpose.

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

It also clears install locks. `Drop` releases a lock on every ordinary exit
including an unwind, but not on a `SIGKILL`, an OOM or a flat battery — and a
lock nobody can clear would refuse every future install of that app forever,
which is one way an App Store crash *would* invalidate a host-owned
transaction. A lock still on disk when recovery runs cannot be held by a live
operation, because recovery runs before anything has been launched. That
precondition is the whole licence for breaking them, and it is why nothing
else does.

## The App Store

`apps/app-store`, in four pieces, split so the interesting one needs no
device:

- `screen` is the state, and it is **pure**. A press returns a `Request`; the
  app hands that to `Context::spawn` and folds the answer back in as an
  `Outcome`. So which button appears, what it does, and what happens when a
  download fails are all testable with no catalog, no store and no panel.
- `render` draws that state and returns the rectangles it used, so a press is
  hit-tested against what is actually on the glass.
- `source` is the seam to the platform. The activation guard — the answer to
  "is this app running right now?" — is a constructor argument, never assumed.
  §6 requires that an update never replaces the running version of an active
  app; that is a fact only the host knows, and an App Store answering it for
  itself would answer it wrong. Today nothing can say yes, so nothing does.
- `source` is the seam to the platform. The App Store holds a `StoreSource`;
  the only way to build the real one is `PackagesSource::for_app`, which takes
  an `InstalledApp` and goes through `PackageManager::on_behalf_of`. An App
  Store that host policy did not grant `packages` gets an error at startup, not
  a degraded installer. That is §5's "client of platform facilities, not the
  owner of them" as a property of the code rather than a claim about it.
- `app` is the wire adapter, `AppStoreApp`. It owns nothing `screen` and
  `source` do not already own: a tap becomes a `Request`, `Request`s go to a
  worker thread (never `event` or `draw` — §8), and what comes back — a
  `Step` along the way, or the final `Outcome` — is folded back into `screen`.
  It is what `paperctl run --app app-store` and the Home shelf's App Store
  tile actually launch.

The app is built **without** the `publishing` feature. An app must not contain
a code path that can sign a release (§12), and `cargo build -p paper-app-store`
is what proves it does not — only the tests turn the feature on, to build a
catalog to install from.

Every `SourceError` carries `advice()`, because §6 asks for *actionable*
errors and "the catalog could not be read" is a fact rather than an action:

| What happened | What it tells you to do |
|---|---|
| Not signed by a trusted key | Do not install it. |
| Catalog unreachable | Check the home network; installed apps keep working. |
| App is running | Close it first, then install the update. |
| Digest or size mismatch | Try again; nothing was changed. |
| Offered version is older | Use roll back instead. |
| No room left | Free some space and try again. |

`paperctl screenshot --screen app-store` renders it; `paperctl run --app
app-store` runs it interactively, against `PAPERCLIP_ROOT`'s own store and a
`catalog/` directory beside it.

### What it shows

`Inventory::survey` merges three sources — what is installed, what a catalog
offers, and what has actually managed to start — into one row per app: name,
installed version, available version, fallback, every release on disk, launch
health, and a one-word state. Both `paperctl list` and the App Store render exactly
that model, so they cannot drift apart about what an update is.

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
