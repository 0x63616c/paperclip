# ADR-0019 — The platform update transaction

**Status:** accepted, proven in the VM harness. **Exercised on hardware once**
(WWW-55, 2026-09-18, the first run of `tools/device-acceptance/run.sh` against
a real tablet) and it did not reach COMMIT: it staged, ran `paperctl setup`,
then failed inside VERIFY at `systemctl start paperclip-host.service` — `Unit
not found` (WWW-74). `xochitl.service` was untouched throughout; the failure
landed after ACTIVATE confirmed stock was up and before any session took the
display. WWW-74 fixed the cause (below); a hardware run confirming the fixed
transaction reaches COMMIT on a clean tablet is still outstanding, and nothing
here should be cited as evidence that it does.

Implements spec §13 and the setup/removal half of §14.

## Context

§13 asks for something narrower than "an update mechanism":

- an updater **independent of the host being replaced**;
- stage and verify a complete release before anything running is touched;
- return the device to stock, then activate;
- a journal, so a power cut is reconcilable rather than guessed at;
- **bounded** health checks, with *process startup alone not counting as health
  evidence*;
- rollback on failure, with **no unbounded retry loop**;
- routine app installation unable to replace the host, the trust keys or the
  recovery bootstrap;
- persistent state migrations backward compatible or recoverable — rollback
  cannot be a swap of executables over state the older release cannot read;
- a reboot during any stage leaving stock startup available.

Calum brought an independently written upgrade design (WWW-8, 2026-09-16) that
agreed with §13 and sharpened three points: an explicit `previous` symlink, a
concrete readiness ladder, and `PREPARE → ACTIVATE → VERIFY → COMMIT` as the
journal's state names. Those are adopted verbatim.

## Decision

### The layering, and the rule it encodes

```
apps                   installed from the catalog, independently versioned
──────────────────────────────────────────────────────────────────────────
Paperclip              host + Home + App Store + Settings: ONE release
──────────────────────────────────────────────────────────────────────────
paperctl               performs the swap; is not one of the files swapped
──────────────────────────────────────────────────────────────────────────
reMarkable OS          A/B rootfs, SWUpdate, not ours
```

Each layer may replace the one above it and never the one below.

### On disk

```
/home/root/paperclip/
  .paperclip-platform       the marker that says this is ours to delete from
  bin/paperctl              the recovery bootstrap, and the updater
  keys/                     trusted public keys
  releases/<version>/       platform.toml, platform.toml.sig, bin/*
  current   -> releases/0.4.0
  previous  -> releases/0.3.1
  staging/<transaction>/
  state/update.json         the journal
  state/platform/           the platform's own persistent state
  state/snapshots/<version>/
```

`previous` is written down rather than derived. Deriving it — "the highest
version that is not `current`" — is right until there are three releases on
disk, and then it silently picks one that was never running.

`paperclip-host.service`'s `ExecStart=` is
`/home/root/paperclip/current/bin/paperclip-host`. Activation is therefore a
symlink swap and a restart, not a copy over a file something may be executing.

### One binary, not two

§13 wants the updater outside the host session. `paperctl` already is: it lives
at `bin/paperctl`, outside `releases/`, because it is what
`paperclip-restore-stock.service` runs when the supervisor has died. A platform
update replaces everything under `releases/` and cannot reach it.

So the requirement is met by *placement*, and a second binary in the same
directory would meet it no better while being a second thing to build and keep
in step. This departs from the file list in the WWW-8 design comment, which
named a separate `updater` binary; it does not depart from what that binary was
for.

Replacing `paperctl` itself is the one operation an ordinary upgrade cannot do,
and it has its own subcommand — `paperctl upgrade bootstrap`. Its recovery plan
is that the outgoing binary is *renamed* to `bin/paperctl.previous` rather than
overwritten, so it survives on disk for someone to move back by hand over SSH.

The word "updater" is internal. The verb is `paperctl upgrade`, and later a
button that says "Update Paperclip".

### The readiness ladder

§13 says "bounded health checks" without saying what they check. This is the
definition, published by the supervisor in its status file as it climbs:

```
process           the unit has a main pid and it is alive
control           the command and status files exist; it is addressable
protocol          the version this build speaks is recorded
device-adapter    the facilities probe succeeded and the stock adapter is up
home              the selected release's Home binary is an ELF this machine runs
ready             READY=1 sent
```

Two consequences worth naming.

**`active` and healthy are different claims, and both are made.** The
supervisor sends `READY=1` whatever the ladder reached, because a supervisor
that withheld it would be killed by the unit's start timeout — taking away the
one process able to say which rung failed. `systemctl is-active` answers "can
it be asked"; `ready=` in the status file answers "is it healthy". An upgrade
commits on the second.

**The report names the rung.** "The update failed" sends someone to the tablet.
"The update stalled at `device-adapter`" sends them to the display stack.

The climb deadline is 30 s, and the *upper* bound on it is not a matter of
taste: it must stay under `HOST_WATCHDOG`. A deadline at or above the watchdog
means systemd kills a stalled candidate — for not petting a watchdog it never
reached the main loop to pet — at the same moment the updater is grading it.
The rollback happens either way; the reported reason degrades to "the
supervisor exited" and the useful half is lost to a race. The VM harness showed
it: both stall cases took 80.3 s and 80.5 s against an 80 s watchdog. The
updater decides, not systemd.

### The transaction

```
PREPARE   stage and verify a complete release. Nothing selected changes.
ACTIVATE  return the display to stock, then swap `previous`, then `current`.
VERIFY    bring the candidate up and grade it against the ladder.
COMMIT    only if it reached `ready`.
```

Ordering decisions, each paid for by a specific failure:

- *Stage before touching anything.* An unsigned, truncated or wrong-machine
  candidate is refused while the running platform is still running.
- *Stand down before activating.* Swapping the binary under a session that
  still owns the panel is how a device ends up with a blank screen and no
  supervisor. This is also where the wakelock is checked: one still held after
  the old supervisor is gone belongs to nobody, and is released by name —
  refusing to continue if that does not work, while refusing still costs
  nothing.
- *Swap `previous` before `current`.* Interrupted between the two, `current`
  still names the old release. The other order leaves a `current` with no
  recorded fallback.
- *Grade, then commit.* Committing first would mean the journal reached its
  terminal state while the outcome was unknown.

### VERIFY writes the units it starts (WWW-74)

`/run/systemd/system` is a tmpfs (WWW-11), so `paperclip-host.service` and the
units next to it are gone after *every* reboot, not only before a first
install — a platform already running yesterday and rebooted today is the same
starting point, as far as VERIFY is concerned, as a tablet that has never had
Paperclip on it. Before WWW-74, `bring_up()` assumed something upstream had
already written them; nothing in the real product ever did; `paperctl units`
is a manual preview command nothing else invokes, and the only real writer was
the VM harness's own fixture, which is why the VM never caught this.

`bring_up()` now writes [`UnitSet::for_supervisor`] itself — everything except
the per-app unit, which needs a specific app's `SessionGrants` that VERIFY
does not have yet — and calls `daemon-reload` before starting the supervisor.
The alternative of making the install path call whatever wrote the units
before was rejected: there is no such call to make, since nothing upstream of
VERIFY ever wrote them for real. Making the units persistent instead was also
rejected, on purpose: that is WWW-53's decision to make, and it spends this
ADR's own reboot-lands-at-stock guarantee (below), not something to reach for
inside a bug fix here.

### Rollback, and the loop that is not written

One climb for the candidate, one for the fallback. If neither is healthy the
device is left at stock with the journal saying `failed`. A tablet sitting at
stock with a written explanation is reachable over SSH; a tablet in a restart
loop is not. There is no function in `platform/updater` that could contain a
retry loop, which is the way to not write one.

### A reboot mid-update

Reverts, never resumes. A journal at `activate` or `verify` describes an update
that never committed, so `current` goes back to `from` and any state snapshot
is restored. Resuming would mean starting a candidate that has never been shown
healthy, unattended, on a device that just came back from an interruption.

The journal is under `/home/root/paperclip/state`, not `/run`: a record cleared
by the reboot it exists to survive would be no record. The *units* are in
`/run` (WWW-11), so the reboot itself lands at stock by construction — which
makes the power-loss story stronger than the design assumed, not weaker.

### State migration

A platform manifest declares two numbers:

- `state_version` — what this release writes;
- `rollback_to_state` — the lowest state version that can still read what this
  release writes.

Before activating, if the outgoing release's `state_version` is below the
candidate's `rollback_to_state`, the updater snapshots `state/platform` and
records the snapshot in the journal. Rollback restores it. If the snapshot
cannot be taken, the upgrade is refused — while refusing is still cheap.

This is what stops "rollback" from being a swap of executables over bytes the
older release cannot parse, which is not a rollback but a second failure with
the first one's evidence destroyed.

### Apps cannot reach any of it

Two structural facts, neither of them a convention the installer follows:

1. **Different trees.** The app store is `~/.local/share/paperclip`; the
   platform is `/home/root/paperclip`. `PlatformLayout::separate_from` refuses
   to operate when they overlap, and no `AppId` produces a path in the platform
   tree because the tree is not under that root.
2. **Different signing domains.** A platform manifest is signed under
   `paperclip.platform.v1`; an app release under `paperclip.release.v1`. A
   signature over an app release does not verify as a platform manifest, from a
   trusted key, over identical bytes.

### Extraction

One extractor, not two. `paper_packages::archive::extract_tree` is the shared
core under the app installer's `extract` — two size bounds, an entry ceiling,
no symlinks, no device nodes, no absolute paths, no `..` — and the platform
bundle uses it directly. A second copy of those protections is a second copy to
get wrong.

## Consequences

- `SessionPaths::host()` now resolves through `current`. Any deployment that
  put the supervisor at `bin/paperclip-host` must move it into a release.
- The supervisor claims `ready` only if it can validate the selected release's
  Home binary. A deployment with no `current/bin/home` will run and report
  `ready=device-adapter`, which is correct and will refuse an upgrade to it.
- Two releases are kept on disk, not one and not all. One would make rollback a
  download; all of them would fill `/home`.

## What is not settled

- **No platform upgrade has reached COMMIT on hardware.** One run (WWW-55) got
  as far as VERIFY and failed there for the reason WWW-74 fixed; the §17
  acceptance sequence with the fix applied is still outstanding, and nothing
  here should be cited as device evidence that the fixed transaction works.
- Nothing here writes `paperclip-app@.service` before a session actually
  launches one; that unit is still written nowhere in the real product. It did
  not block WWW-74 because VERIFY only starts the supervisor, never a session,
  but it is the same shape of gap and belongs to whatever ticket makes a
  production launch request go through these units rather than the in-process
  bridge `paperctl run`/`open` use today.
- Tablet-initiated updates stay deferred, per §13, until this is proven on the
  device.
- A catalog-driven platform update is not implemented; `paperctl upgrade run`
  takes a bundle path. The App Store as a frontend onto the same machinery is
  §13's later step, not this one.
