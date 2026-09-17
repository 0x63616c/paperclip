# ADR-0014 — The install transaction: durability, activation and rollback

**Status:** accepted (Stage 7, WWW-7)

## Context

§12 asks for an install that is a transaction: validate, stage, verify,
extract, commit durably, activate only when the app is stopped, keep the
previous usable release. It says explicitly that **rename alone is not a
power-loss guarantee**, that an interrupted install must leave the prior
release usable and incomplete staging non-launchable, and that rollback is
explicit.

This is the code in the repository most likely to look correct and be wrong. A
naive implementation — extract into place, rename, done — passes every test you
would write for it and loses the device on one bad power cut.

## Decision

### The invariant

There is exactly one, and everything below exists to hold it:

> **The selected version is a complete release directory, or there is no
> selected version.**

"Complete" is a file: `.paperclip-release` inside the release directory,
holding the archive digest, the signing key id, and when the install
committed. It uses the prefix `archive::extract` refuses, so no package can
forge one. Nothing without it is ever listed as installed, ever selected, or
ever offered to anything.

### Durability is four steps, not one

`rename(2)` is atomic with respect to other readers. It says nothing about
power: after it returns, a crash can leave the new name pointing at content
that never reached the disk, or leave neither name, because the directory entry
is itself in a write-back cache.

So `store::atomic_write` and `store::commit_directory` are the only places a
commit is written, and both do the same four things:

1. write the new content under a temporary name in the **same** directory
   (a rename across filesystems is not a rename);
2. `fsync` the content — every file, for a tree;
3. `rename` over the real name;
4. `fsync` the directory that now holds the name.

Step 2 makes the file real. Step 4 makes the *name* real.

### The order of an install

1. Validate the signed descriptor — it is a `VerifiedRelease`, which cannot
   exist without a trusted signature — and compute the capability grant from
   host policy.
2. Download into `staging/<transaction>/`, bounded by the size the descriptor
   declares, hashing in the same pass that writes.
3. Check size, then digest.
4. Extract into `staging/<transaction>/payload`, executing nothing.
5. Check that the package's own `paper.toml` agrees with the signed descriptor
   on app id, version and protocol. Without this the signature covers a wrapper
   rather than its contents, and anyone able to swap archives inside a catalog
   could ship a different app under a name the descriptor made trustworthy.
6. Write the completion marker into the staged payload — **last**.
7. `fsync` the whole staged tree, rename it to `apps/<id>/releases/<version>`,
   `fsync` that directory. A release directory therefore appears complete or
   does not appear.
8. If activating: refuse if the app is running, journal the intent, write
   `previous`, then `current`, then delete the journal entry.

### The journal, and what each interruption leaves

`state/journal/<app>-<version>-<phase>.toml` records an intent *before* it is
acted on, durably, including an fsync of the journal directory. An intent
written after the fact is an intent recovery never sees.

| Killed at | State on disk | What `recover` does |
|---|---|---|
| Download or extraction | staging tree, `stage` entry | delete both; the prior release was never touched |
| Between commit and activation | complete new release, unselected, `activate` entry | finish the activation — the bytes are all there and the intent was recorded |
| During activation, after `previous`, before `current` | `activate` entry | finish it; rerunning the two selection writes is idempotent |
| With a half-written release directory | — | cannot happen |

`previous` is written before `current`, always. A crash between them leaves
`previous` pointing somewhere true and `current` still on the old release,
which is exactly the state recovery knows how to finish. The other order leaves
`current` on the new release with no recorded way back.

`recover` also sweeps staging directories no journal claims, and release
directories with no marker — the debris a naive installer would leave. It is
safe to run when nothing is wrong, and is meant to run at host start before
anything is launched.

### Serialising conflicting operations

An `AppLock` is held for the whole of install, rollback and per-app recovery.
It is a directory created with `create_dir`, because that is the exclusive
create the standard library offers without `unsafe`, which §7 bans outside
`platform/device/native`. Per app, so an install of Chess does not block one of
the App Store.

### Activation is not launching, and rollback is not automatic

Activating selects a version. Nothing in this crate starts a process; §12
forbids a launch triggered by an install, and there is no code here that could.

Rollback is a selection change back to `previous`, and the version rolled back
*from* becomes the new fallback, so a rollback can itself be undone without
going back to the catalog. It is never automatic: §12 wants a failed release to
be recoverable *from*, not silently replaced. A platform that reverts on its
own hides the failure, and the next install walks into it again.

Whether a release ever actually worked is a different question, asked after
something has tried to run it, and lives in `paper_packages::launch`. A release
that has been attempted three times without reporting a start stops being
offered for launch, so a crash loop is a thing the operator is told about
rather than a thing the device does forever.

### What is not persisted

`GrantedCapabilities`, deliberately. ADR-0003's third property is that nothing
on disk can produce one; writing grants into the store and reading them back
would be exactly that. They are recomputed from host policy at install and at
launch.

### Downgrades

An install refuses a version older than the one selected. A stale catalog
served to a device should not walk it backwards, and expressing a downgrade is
what `rollback` is for. `allow_downgrade` exists for the caller that means it.

## Consequences

- `paperctl recover` exists and is cheap. The host will call the same
  `PackageManager::recover` at start.
- Pruning keeps the selected release, the fallback, and the release just
  installed. The last of those is not decoration: an install that deliberately
  did not activate — a download held for a platform update — has just written a
  release that is neither current nor previous, and pruning by selection alone
  deletes the thing the install was for. That bug existed and a fault test
  caught it.
- `tests/install.rs` covers every fault §12 names, plus a simulated power loss
  between staging and commit, and each asserts the same thing: the previously
  installed release is still there and still selected.
- The store's root is `PAPERCLIP_ROOT`, then `/home/root/.local/share/paperclip`.
  There is no current-directory fallback; a store that appears wherever a
  command ran from is a store that gets two copies.

## An unresolved conflict, recorded rather than decided

§11 puts Paperclip's data under `/home/root/.local/share/paperclip`.
[ADR-0008](0008-runtime-only-units-and-no-root-filesystem-install.md) says its
files live under `/home/root/paperclip`. Both are outside the root filesystem,
so neither breaks the rule that matters, and this ADR does not pick a winner:
`Layout` takes a root, the §11 path is the default, and `PAPERCLIP_ROOT`
overrides it. WWW-8 installs the platform itself and is the run that has to
reconcile the two.

## What would make this wrong

- If the device filesystem turns out not to be ext4 with ordered-data
  journalling, the fsync-plus-rename sequence needs re-checking against what it
  actually is. Nothing here has run on the tablet.
- If two processes ever need to install *different* apps while sharing one
  staging area, the per-app lock is no longer sufficient and staging needs its
  own.
- If an app ever needs to survive a rollback with migrated data, the note in
  §11 — an executable rollback does not roll back data migrations — becomes a
  design problem rather than a documented one.
