# ADR-0008 — Runtime-only units, and nothing on the root filesystem

**Status:** accepted (Stage 2, WWW-11). Recorded here by WWW-3, which is the
first stage to need the rule.

## Context

Spec §10 requires an "independent systemd recovery path" that restores stock
after a host crash, and says in as many words that device-local supervision
handles loss of connectivity — "the Mac is not the only watchdog." §10 also
allows that "runtime systemd units may be needed."

WWW-1 established that the assumption underneath that wording is false on this
firmware: **there is no writable persistent location for a systemd unit.**

- `/` is mounted read-only, so `/lib/systemd/system` cannot be written.
- `/etc`, `/var/lib`, `/var/cache`, `/srv` and `/var/spool` are overlays whose
  upper layer is tmpfs. Anything written there is gone at the next boot.
- `/var/lib/remarkable` (the `persist` device) is mounted read-only.
- The root filesystem has 41.3 MB free of 521 MB, so installing binaries there
  was never an option regardless.
- `/home` is the only writable persistent storage — 46.3 GB, and it is where
  Paperclip's own files go.

WWW-11 investigated four ways out and established facts about each rather than
weighing them in the abstract.

**Remount `/` read-write and install a unit** is *possible*: root is plain ext4
on `/dev/mmcblk0p3`, there is no dm-verity, `/proc/cmdline` carries only
`ro rootfstype=ext4`, and community projects on this exact device do it. It is
also *certainly* erased by every OS update, not merely at risk of it:
`/usr/lib/swupdate/conf.d/09-swupdate-args` selects the update target by
inverting the running slot, so SWUpdate writes the whole other half of the A/B
pair and switches to it. We are booted from `root_b`.

**An existing vendor hook that runs at boot from a writable location** was
refuted by enumeration: every `ExecStart*`, `EnvironmentFile`,
`ConditionPathExists` and `AssertPathExists` in `/lib/systemd/system` that
reads writable storage runs a fixed vendor binary; `/etc/rc.local` does not
exist and `/etc` is volatile; there are no vendor generators; `/usr/local` is a
dead directory; `/var/lib/systemd/linger` is on tmpfs; the four vendor
`.mount` units bind `/home` onto paths that are not execution paths; and there
is no `fw_printenv`/`fw_setenv` with both eMMC boot partitions at
`force_ro=1`.

**`/data` exists**, is writable, persistent across OS updates, and has 92.3 MB
free — but nothing at boot *executes* from it. The vendor only reads marker
files there. It is storage, not a hook, and it is device identity state.

## Decision

**Runtime-only units under `/run/systemd/system`, established by `paperctl` at
session start. Paperclip installs nothing on the root filesystem.**

The premise that made this look like a loss was wrong. §10's independence
requirement does not need reboot persistence, because **every failure mode §10
names happens within a boot**:

- the host crashes → systemd's `Restart=`/`OnFailure=` restores stock, no
  network in the path;
- the host hangs → `WatchdogSec=` kills it, same path;
- Wi-Fi drops, USB is unplugged, the Mac sleeps or dies → nothing changes, the
  units are already loaded in PID 1;
- the device reboots for any reason → every Paperclip unit evaporates and stock
  comes back by itself.

That last one is not a gap in recovery. It is the most reliable recovery path
available, and it is free.

Persistence would buy exactly one thing on top: Paperclip starting by itself
after a reboot — which §10's own reboot row explicitly does not want. And it
would cost the guarantee that no Paperclip defect can survive a power cycle.
On a personal device where §11 ranks recovery above isolation, that is a bad
trade.

Auto-start at boot is **deferred, not rejected**. If it is ever wanted, the
procedure is `mount -o remount,rw /`, write the unit, remount read-only, plus a
re-install step after every OS update and a `/data` marker file as a kill
switch — following the vendor's own
`ConditionPathExists=/data/internal/rm_enable_ssh_wifi_marker` precedent.

## The §10 amendment

Accepted verbatim by Evee on 2026-09-17 (WWW-11), and binding:

> **Persistence.** This firmware has no writable persistent unit directory. `/`
> is read-only; `/etc`, `/var/lib`, `/var/cache`, `/srv` and `/var/spool` are
> overlays whose upper layer is tmpfs; `/var/lib/remarkable` is read-only.
> §10's "runtime systemd units may be needed" is therefore strengthened:
> **runtime units under `/run/systemd/system` are the only mechanism
> available**, they are established by `paperctl` at session start, and they
> are discarded at the next boot. Paperclip installs nothing on the root
> filesystem; its files live under `/home/root/paperclip`.
>
> **Independence.** Once a session is established, supervision is device-local
> and requires nothing from the Mac: systemd owns the Host, restarts or kills
> it on failure or watchdog timeout, and restores stock through an
> `OnFailure=` recovery unit, with no network in the path. The Mac is required
> only to *enter* a session, which is how §4 already defines v1 entry. Losing
> the Mac, the network or the cable mid-session must not affect recovery.
> Losing it between sessions means Paperclip cannot be started, and the tablet
> remains stock.
>
> **Reboot.** Unchanged in intent, now guaranteed by construction rather than
> by policy: any reboot — planned, kernel panic (`panic=2`), watchdog, or
> battery exhaustion — discards every Paperclip unit and returns the device to
> stock. Stock startup remains the default; there is no automatic custom
> foreground takeover. A reboot is the always-available recovery of last
> resort, and no Paperclip failure can persist across one.
>
> **Deferred.** Auto-starting Paperclip at boot. It requires writing a unit
> into the read-only root filesystem — possible via `mount -o remount,rw /`,
> proven by community projects on this device — but it is erased by every OS
> update and it forfeits the reboot guarantee above. Outside v1.

## Consequences

- Binaries live under `/home/root/paperclip`. Nothing goes on the root
  filesystem, ever.
- A session must hold `/sys/power/wake_lock`. Not an optimisation: without it a
  mid-session suspend resumes into a second Xochitl contending for the panel.
  Implemented as `paper_device::WakeLock`, which releases on every exit path
  including an unwind.
- `LimitAS=` replaces `MemoryMax=`, which is silently ineffective on this
  hybrid cgroup hierarchy.
- WWW-4 designs the supervisor as `paperclip-host.service`,
  `paperclip-restore-stock.service` and `paperclip-session.target` in
  `/run/systemd/system`, with recovery via `OnFailure=` that starts `xochitl`,
  releases the wakelock and clears a stale `/tmp/epframebuffer.lock`.
- The Stage 5 conformance gate checks the amendment landed and that no code
  assumes a persistent unit.

## The by-product that outranks the decision

`xochitl.service`'s effective `OnFailure=` is
`emergency.target remarkable-fail.service`, and **`remarkable-fail.service`
does not exist on this image**. `emergency.target` is `systemd-sulogin-shell`
on a serial console — a tablet that looks dead and needs a power cycle.

Stopping Xochitl cleanly does not trigger this. *Crashing* it does, and so does
exceeding `StartLimitBurst=4` within 600 s.

**Never let Xochitl fail. Only ever stop it cleanly. Never exceed its
start-limit burst.** Every run that touches `xochitl.service` inherits this.

## What would make this wrong

- If a future firmware gained a writable persistent unit path, auto-start would
  become cheap and the deferral worth revisiting.
- If §10's independence requirement were ever read to mean "recovers across a
  reboot without the Mac", this answer would not meet it. WWW-11's reading is
  that it is not, and §10's own reboot row is the evidence.
