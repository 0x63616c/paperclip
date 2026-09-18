# ADR-0032 — Refreshing the recovery bootstrap

**Status:** accepted.

Implements WWW-76. Every claim about `bootstrap`'s and `doctor`'s behaviour
below is exercised by unit tests in `tools/paperctl/src/upgrade.rs` and
`doctor.rs`; nothing here has run against a real tablet, and none of it needs
one to be correct — see "What still needs hardware" below.

## Context

`paperctl` at `/home/root/paperclip/bin/paperctl` is the recovery bootstrap
`paperclip-restore-stock.service` runs when the supervisor has died
(`tools/paperctl/src/upgrade.rs`'s module doc; ADR-0019 §13). It sits outside
`releases/` on purpose, so an ordinary platform update — which replaces
everything under `releases/` — cannot reach it. Nothing else refreshes it
either. WWW-55 hit this directly on 2026-09-18: a device run reproduced a
failure `703c4d4` had already fixed, hours after the fix landed, because the
`paperctl` on the device predated it. The session had to cross-compile and
`scp` a new one by hand before the fix could even be observed.

Three things needed deciding:

1. Does an ordinary platform upgrade refresh `paperctl`, and if not, what
   does?
2. How does anyone — a person or an agent — learn the device's `paperctl` is
   stale, short of a failure that predates the fix they are trying to
   verify?
3. Can replacing the live bootstrap ever leave the device with no working
   `paperctl` at all, if the replacement fails part-way through?

## Decision

### Refreshing stays a deliberate, separate step — not part of `upgrade run`

`paperctl upgrade bootstrap <replacement>` already existed (this module,
predating WWW-76) for exactly this: replacing `paperctl` itself, with its own
recovery plan, because "replacing the recovery bootstrap" cannot be an
ordinary update.

WWW-76 does **not** wire that call into `paperctl upgrade run`. The platform
upgrade transaction (`platform/updater::upgrade`, ADR-0019) is its own
reviewed surface — §13's stage/activate/verify/commit/rollback state machine,
gated in this project's review model because a wrong call there can leave the
environment unbootable. Folding "also replace the recovery bootstrap" into
that transaction would mean a platform rollback and a bootstrap replacement
could interleave in ways nothing has designed for — for example, a platform
release that gets rolled back after its `paperctl` was already swapped in
would leave a `paperctl` that does not match either the restored release or
the one that failed. `bootstrap`'s own safety net (a copy of the outgoing
binary, described below) is deliberately independent of the platform
transaction's snapshot/rollback machinery, and coupling the two would blur
that.

Instead: a platform bundle *may* carry `bin/paperctl` as an extra component
— `paperctl upgrade package --include bin/paperctl` already supports shipping
arbitrary extra files, covered by the same manifest digests as everything
else (`platform/updater/src/bundle.rs`). No new packaging mechanism was
needed. After a successful `upgrade run`, the CLI now prints a pointer to
`doctor` and to `bootstrap`:

```
Paperclip 0.4.0 -> 0.5.0: healthy
if this release changed paperctl, `paperctl doctor` will say so;
`paperctl upgrade bootstrap <path>` refreshes it.
```

This is the answer to "when does it run": at the operator's (or an agent's)
discretion, after an upgrade, once `doctor` has confirmed there is something
to refresh — never automatically, and never inside the platform transaction.
The dev-loop path is unchanged: `paperctl deploy` (ADR-0023) remains how a
development build reaches the tablet.

### `paperctl doctor` says "older", not just "different"

Before WWW-76, `doctor`'s `BinaryStatus.matches_local` was a plain string
equality against `CARGO_PKG_VERSION` — true, false, or absent. It could not
say which side was stale, and a `paperctl` on the Mac that happened to be
*older* than the device's would have been reported as a mismatch requiring
no action, worded identically to the dangerous case.

`BinaryStatus` now carries `comparison: Option<VersionComparison>` —
`Same`, `DeviceOlder`, `DeviceNewer`, or `Unparseable` — computed by parsing
both sides as semver and ordering them
(`tools/paperctl/src/doctor.rs::VersionComparison::of`). Table output says so
in words: `DeviceOlder` prints the exact remediation (`run
`paperctl upgrade bootstrap` or `paperctl deploy` to refresh it`);
`DeviceNewer` is reported but does not degrade the verdict, since a Mac
running an older `paperctl` than an already-upgraded device is not the
problem WWW-76 is about; `Unparseable` degrades the verdict alongside
`DeviceOlder`, on the same "cannot confirm it is fine" reasoning already
applied to a missing binary.

This is what would have shortened WWW-55: one `paperctl doctor` naming the
stale binary before the device session started, rather than discovering it
mid-session.

### `bootstrap` no longer has a window with no live `paperctl`

Reviewing `bootstrap` for WWW-76 found a real ordering bug, not just a
documentation gap. The previous implementation:

```
if live.exists() { rename(live, kept) }   // live now GONE
atomic_write(live, replacement)           // if this fails, nothing is at `live`
```

`atomic_write` (`platform/packages/src/store.rs`) already stages its content
beside the target, `fsync`s it, and `rename`s the stage over the destination
in one step — it does not need the destination moved out of the way first.
Pre-emptively renaming `live` to `kept` bought nothing and opened exactly the
window §13 and this issue's acceptance criteria rule out: if `atomic_write`
failed for any reason — a full disk, the filesystem gone read-only under the
write — the device was left with `paperctl.previous` but nothing at all at
the live path.

The fix: copy `live` to `kept` (a plain copy — `live` stays in place) before
calling `atomic_write(live, replacement)`. Renaming a new file over one that
is currently executing works on Linux — the running process keeps its own
inode open; that is a plain `rename`, not the in-place truncate that fails
with `ETXTBSY` (the reasoning the original code's own comment already
depended on, just applied to the wrong step). At every instant, `live` is
either the old binary or the new one, never absent.

Proven, not asserted, by
`upgrade::bootstrap_tests::a_failed_replacement_leaves_the_previous_paperctl_working`:
it pre-creates `paperctl.previous` (so the copy step only needs write access
to an existing file, not a new directory entry), makes the bin directory
refuse new entries, and confirms `atomic_write`'s staging file is what fails
— while `live` still holds the original, executable bytes. That is the same
failure shape as a full disk or a filesystem remounted read-only mid-write.

## What still needs hardware

- Whether `atomic_write`'s `fsync`-then-`rename` sequence behaves the same
  under the device's actual filesystem and storage as it does under macOS
  APFS in the test above (unestablished; the mechanism is standard POSIX
  behaviour, not device-specific, so this is low-risk but unconfirmed).
- Whether renaming over a `paperctl` invoked by
  `paperclip-restore-stock.service` while that service is mid-run behaves as
  described (unestablished — no test here runs a real systemd unit).

## Consequences

- `paperctl upgrade package --include bin/paperctl` is the documented way to
  carry a matching `paperctl` inside a platform bundle; no new flag or
  manifest field was added.
- `BinaryStatus.matches_local` is gone; anything reading `paperctl doctor
  --output json` for that field now reads `device_binary.comparison`
  (`"same" | "device-older" | "device-newer" | "unparseable"`).
- `bootstrap`'s previous-binary backup is now a copy, not a rename — the
  outgoing binary and its backup exist simultaneously for the duration of the
  call, which costs one binary's worth of disk space during the swap and
  removes the no-live-binary window in exchange.
