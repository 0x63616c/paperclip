# ADR-0024 — The pinned OS image is load-bearing, and what the backup covers

**Status:** accepted (WWW-44). Every device fact below was re-checked over SSH
on 2026-09-18 against Calum's tablet at `192.168.0.180`, host key
`SHA256:AquctDxJpCGIg0YqGMhj/2tmSFDaAOKKXKCYdT/f0Ic` — identical to WWW-1's
recorded fingerprint, so this is the same device, not a re-trust. All checks
were reads (`cat`, `systemctl cat/is-enabled/is-active/list-unit-files`,
`ssh-keyscan`); nothing was written, restarted, or stopped.

## Context

The rest of this programme treats the vendor ABI (`libqsgepaper.so`,
ADR-0007, ADR-0009) as a fixed target: fixed offsets, fixed exported symbols,
fixed behaviour. That is only true for as long as the OS image does not
change under it — a `SWUpdate` replaces the whole rootfs slot, and
`platform/device/native/README.md` already says to re-run `check-abi.sh` if
the recorded digest ever changes. Nothing before this ADR wrote down that the
image is *expected* to stay put, or what actually enforces that.

Separately, WWW-44 asks for a capture of "the full image / partition layout"
before Stage 2 or later writes to the root filesystem. The device has one
writable persistent partition (`/home`, WWW-1 §5) and four read-only ones
(root, `persist`, and the two boot slots). A byte-for-byte image of the
read-only partitions and a full backup of the one writable partition are not
the same kind of insurance, and conflating them would either waste a great
deal of time imaging bytes that cannot change, or create false confidence
that an untested restore of them was rehearsed.

## Decision

### The image version is pinned, and here is what pins it

**Image `20260827113527`** (`IMG_VERSION=3.28.0.172`, Codex Linux 5.8.203
"scarthgap", kernel `6.12.49+git-imx8mm-ferrari-g2418a6dae57b`) is the
version every downstream ABI assumption in this repository is made against.
Treat a changed `/etc/version` as the signal that every hardware-facing
assumption needs re-verification, not just the vendor library digest.

What actually prevents this image from changing without Calum's action:

| Mechanism | State | Evidence |
|---|---|---|
| `update-engine.service` (`no.remarkable.update1`, `ExecStart=/usr/bin/fakeupdateengine_service` — literally titled `Fake Update Engine` in its own unit file) | **disabled, inactive** | `systemctl cat update-engine.service`; `systemctl is-enabled` → `disabled`; `systemctl is-active` → `inactive` |
| Xochitl's own auto-update setting | **off** | `/home/root/.config/remarkable/xochitl.conf` → `AutoUpdate=false` |
| Scheduled or triggered path to either | **none found** | no `crontab`, no `/etc/cron*`, no `/var/spool/cron`; `/etc/NetworkManager/dispatcher.d/{no-wait,pre-down,pre-up}.d` all empty; `systemctl list-timers --all` shows no update-related timer |

`swupdate.service`, `swupdate.socket` and `rm-apply-ota.service` are
**enabled** — but that is the *apply* machinery (a socket-activated listener
that would act on an update someone pushes to it), not the *check-and-fetch*
machinery. Nothing on the device currently calls it: `update-engine`, the one
component that would decide to, is the disabled stub above. `rm-sync.service`
being enabled/active is reMarkable's document cloud sync, unrelated to OS
update — recorded here only so it is not mistaken for one.

**What this evidences and what it does not.** It evidences that nothing on
the device today initiates or would accept an unattended OS update. It does
not evidence that the mechanism can never be re-enabled — `AutoUpdate=false`
is a setting, not a removed capability, and the vendor could ship a build
that resets it. Re-run the checks in this table (not just the digest check)
after any factory reset, developer-mode excursion (which this project never
performs) or vendor support interaction.

### The backup covers what Paperclip can actually change, not the whole disk

Paperclip installs nothing on the root filesystem (ADR-0008); it does not
patch any vendor binary today (`grep -rln patch platform/ tools/` turns up
only prose about SemVer patch releases and a comment about patch-release
serializer drift — no code path modifies a vendor file in place); and the
only vendor library it links against is `libqsgepaper.so`, whose exact bytes
are already pinned by its own digest (`3f76b7db328f7e16cde2d360ebe4a1db043
a113cd2386d4c995cc4d25a0eeaec`, recorded in
`platform/device/native/vendor-abi.txt` and ADR-0009, re-verified as current
in this session's `/etc/version` read).

So the only partition Paperclip's own actions can corrupt is `/home` — the
one already captured whole by WWW-1's backup (`docs/backup.md`). The root
filesystem, `/data`, and `/var/lib/remarkable` (`persist`) are read-only to
every process on the device, Paperclip included (`docs/recovery.md`'s hard
device rules), and are not this project's to restore if they are damaged —
that is reMarkable's own out-of-band recovery mode, which exists
independently of anything captured here.

**Decision: no full raw image of the eMMC is captured.** It would mean
reading `/dev/mmcblk0` (~58 GB across all partitions per WWW-1's
`/proc/partitions`) over SSH for a filesystem Paperclip cannot write to and
does not need bit-identical to recover from anything *this project* might
do. What is captured instead, and is sufficient for that scope: the full
partition table and mount map (`system-metadata.txt`, gathered read-only via
`cat /proc/partitions`, `blkid`, `mount`), and the complete contents of the
one writable partition (`home.tar`).

## Consequences

- A hardware gate closes here that Stage 2 or later would otherwise have had
  to re-derive under pressure: `docs/recovery.md`'s stop condition ("a
  verified backup does not exist and Stage 3 or later is about to take the
  display") is now backed by a written procedure, not just an archive
  sitting on the Mac.
- If the auto-update evidence above ever comes back different — `disabled`
  flips to `enabled`, or a timer appears — that is a stop condition in its
  own right (`docs/recovery.md`), because the rest of this ADR's promise
  that the ABI stays fixed no longer holds.
- The backup's restore procedure is documented and not rehearsed
  (`docs/backup.md`), same caveat WWW-1 already recorded: a restore test on
  the only device is itself a risk, so the confidence claimed stops at
  "verified consistent", not "verified restorable".

## What would make this wrong

- A vendor support flow, factory reset, or firmware recovery that silently
  re-enables `update-engine.service` or flips `AutoUpdate=true`. Re-check the
  table above rather than trusting this ADR's date.
- A future need to patch a vendor binary in place. If that ever becomes
  real, the missing "original bytes" capture this ADR declines to do today
  becomes required before the patch ships, not after.
