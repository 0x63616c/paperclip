# Backup and restore

The procedure `docs/recovery.md` listed as owed. Read `docs/recovery.md`
first — this file is the backup half of that document, split out because it
has its own audience: whoever is about to run Stage 3 or later, or whoever is
holding a tablet after something went wrong.

## What is captured, and where

A full copy of `/home` — the only writable persistent partition (WWW-1 §5;
`docs/adr/0024-pinned-image-and-backup-scope.md`) — taken 2026-09-17T00:40Z
over USB SSH while the device was untouched otherwise. It lives on the Mac at
`~/paperclip-backups/2026-09-17T00-40Z-stage0/`, directory mode `0700`:

| File | What it is |
|---|---|
| `home.tar` | `/home`, 571 files / 26.7 MB, `tar -C /home -cf - --numeric-owner`, streamed over SSH |
| `home.tar.sha256` | SHA-256 of the archive |
| `home-manifest-before.sha256` | Per-file SHA-256 taken on the device immediately before the copy |
| `home-manifest-after.sha256` | Same, immediately after — used to detect concurrent writes |
| `home-manifest-restored.sha256` | Per-file SHA-256 recomputed from the extracted archive |
| `verify-diff.txt` | Diff between the device manifest and the extracted archive |
| `system-metadata.txt` | `os-release`, `uname`, `/proc/partitions`, `mount`, `blkid`, `systemctl --failed` |
| `reference/libepaper.so` | Xochitl's own QPA plugin, copied for offline reference (not linked by Paperclip) |
| `RESTORE.md` | The original capture note; this file supersedes it as the canonical procedure |

**This directory contains secrets** — dropbear host keys, `authorized_keys`,
NetworkManager profiles with Wi-Fi PSKs, and Xochitl's `UserToken`/
`devicetoken`. It stays on the Mac at the path above. Never commit it, attach
it to an issue, or copy it anywhere this repository's history could reach.

### What is deliberately not captured, and why

- **A full raw image of the eMMC.** The four read-only partitions (root,
  `persist`, the two boot slots) cannot be written to by anything on the
  device, Paperclip included, so there is nothing here that captures a state
  this project could corrupt. If the rootfs itself is ever damaged, the
  recovery path is reMarkable's own out-of-band recovery mode, independent of
  this backup. See ADR-0024 for the full reasoning.
- **The original bytes of a vendor binary this project patches.** There
  isn't one. Paperclip links `libqsgepaper.so` (via `platform/device/native`,
  ADR-0007) but does not modify it, and no code path in `platform/` or
  `tools/` patches any vendor file in place — checked by grepping both trees
  for `patch`; the only hits are prose about SemVer patch releases (ADR-0024
  records the exact grep). If that ever changes, capturing the pre-patch
  bytes becomes a precondition for shipping the patch, not an afterthought.

### The exact vendor library this project binds to

Already pinned, not re-captured here: `libqsgepaper.so`, sha256
`3f76b7db328f7e16cde2d360ebe4a1db043a113cd2386d4c995cc4d25a0eeaec`, pulled
from image `20260827113527` and recorded in
`platform/device/native/vendor-abi.txt` and ADR-0009. A changed digest on a
future pull is the signal to re-run `platform/device/native/check-abi.sh`
before trusting anything that links it (`tools/device-probe/README.md`).

### The pinned OS image and boot identity

- Image `20260827113527` (`IMG_VERSION=3.28.0.172`, Codex Linux 5.8.203).
  Confirmed current as of this write-up — see ADR-0024, which also records
  what actually prevents it from changing (OS auto-update, below).
- `machine-id` `080f8209dabc79ef080f8209dabc79ef` — identical between WWW-1's
  capture and this one, confirming it is the same device across sessions.
- `boot_id` at WWW-1's capture: `db478d64-1465-459d-9e75-6018653af388`. At
  this write-up: `3da09914-0f42-4445-bc92-0ede9ced4172`. A boot id is not
  something to pin — it changes every boot by design — it is recorded here
  only as an evidence anchor for exactly when each capture was taken.

### OS auto-update: disabled, mechanism named

Full evidence and reasoning is in ADR-0024. Summary: the device's real update
engine (`update-engine.service`, D-Bus name `no.remarkable.update1`) is a
disabled, inactive stub (`/usr/bin/fakeupdateengine_service`, literally
titled `Fake Update Engine` in its own unit file); Xochitl's own setting
(`/home/root/.config/remarkable/xochitl.conf` → `AutoUpdate=false`) agrees;
and no cron, timer, or NetworkManager dispatcher script exists that would
trigger either. `swupdate.service`/`.socket` being enabled is the separate
*apply* listener, not something currently calling itself.

## What "verified" means here, and what it does not

**Verified** means: a per-file SHA-256 manifest taken on the device before
the copy, the same manifest taken again after, and a third manifest
recomputed from the extracted archive — all three compared. The result: 570
of 571 files bit-identical; the one difference is the live systemd journal,
which both device manifests independently show changed during the capture
window and nothing else did. All 77 files under
`.local/share/remarkable` (notebooks, metadata, templates, `user-auth`) are
included in that identical set.

**What this does not claim:**

- **Not a quiesced backup.** Xochitl ran throughout the capture. It is
  consistent because verification showed nothing relevant was being written,
  not because the writer was stopped. A guaranteed-quiescent backup would
  require stopping `xochitl.service` for the capture — a state change WWW-1
  was not authorised to make, and this issue did not need it to close the
  gate below.
- **Not a rehearsed restore.** The restore procedure below has not been run
  against the tablet. Rehearsing it means wiping or overwriting `/home` on
  the only device this project has, which is a real risk for a
  correctness check — the standing constraint against testing recovery by
  wiping the device stands. The confidence claimed is "the archive matches
  what was on the device to the byte, twice-checked," not "restoring from it
  is proven to work."
- **Not an image of anything Paperclip cannot write to.** See "What is
  deliberately not captured" above.

## Restore procedure

Restores *user data only* — notebooks, settings, sync state, the encrypted
`/home` partition's contents. It does not reinstall firmware; the root
filesystem is outside this backup's scope by design (see above), and a
corrupted rootfs goes through reMarkable's own recovery mode, not this
procedure.

1. Establish SSH to the tablet (USB `10.11.99.1` or the current Wi-Fi
   address) using the existing key. **Confirm the host key fingerprint
   matches `SHA256:AquctDxJpCGIg0YqGMhj/2tmSFDaAOKKXKCYdT/f0Ic`.** If it has
   changed, stop and investigate — do not accept a new key silently.
2. Stop the writer so the restore is not raced:
   `ssh <tablet> 'systemctl stop xochitl'`. Per `docs/recovery.md`'s hard
   device rules, restore it on every exit path, including failure.
3. Restore into `/home`:
   `ssh <tablet> 'tar -C /home -xf - --numeric-owner' < home.tar`
   (run from `~/paperclip-backups/2026-09-17T00-40Z-stage0/` on the Mac). To
   restore only notebook data, append
   `./root/.local/share/remarkable` as a path argument to the remote `tar`.
4. Verify: take a fresh per-file manifest on the device and diff it against
   `home-manifest-before.sha256`. Expect differences only under `.journal/`
   and `.cache/` — anything else means the restore did not reproduce the
   captured state.
5. Restart the stock application: `ssh <tablet> 'systemctl start xochitl'`.
   Confirm the shelf renders and the notebooks in
   `.local/share/remarkable/xochitl/.tree` are present.

Steps 2, 3 and 5 change device state and need Calum's explicit approval
before they are run, per the project's standing constraints — this procedure
being written down is not standing authorisation to execute it.

## When to refresh this backup

The capture above is from 2026-09-17, one day before Stage 1 lands its first
root-filesystem-adjacent unit (WWW-8 territory) and before any binary
patching exists to worry about. Refresh it — repeat the capture, not just
re-read this file — before:

- Stage 3 or later takes the display for the first time on real hardware
  (`docs/recovery.md`'s stop condition), if meaningful time has passed since
  the last capture.
- Any operation this project has not yet performed on the device: the first
  binary patch (capture its original bytes first, per "What is deliberately
  not captured" above), the first platform self-update (WWW-8), the first
  factory-reset-adjacent test.
- The vendor library digest or `/etc/version` changes, since that means the
  device itself changed and everything pinned here should be re-verified,
  not assumed.
