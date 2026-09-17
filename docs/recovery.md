# Recovery

**Status: nothing in this document has been exercised on hardware.** Stage 3
has now written device code, but it has never taken the display and has never
run on the tablet. This file records the policy that work must satisfy, so it
starts against a written standard rather than inventing one under pressure.

## Hard device rules

Not advice. Each was established by evidence and each has already cost
something.

- **Never let `xochitl.service` fail.** Only ever stop it cleanly. Its
  effective `OnFailure=` is `emergency.target remarkable-fail.service`, and
  **`remarkable-fail.service` does not exist on this image** — so a crashed
  Xochitl, or four failed starts within 600 s (`StartLimitBurst=4`), drops the
  device into `systemd-sulogin-shell` on a serial console. That is a tablet
  that looks dead and needs a power cycle. See ADR-0008.
- **Any script that stops Xochitl must restore it on every exit path**,
  including errors. Guard against the busybox `date +%s%N` class of failure
  that left stock down unintentionally during WWW-1: no `set -e`, no `set -u`,
  no `%N`.
- **A takeover session must hold `/sys/power/wake_lock`** and release it on
  every exit path. Without it a mid-session suspend resumes into a *second*
  Xochitl contending for the panel. Correctness, not optimisation.
  `paper_device::WakeLock` releases in `Drop`, so an unwinding host still
  lets the tablet sleep again.
- **Clear the panel before releasing the display.** WWW-20 photographed what
  skipping it looks like: bands and hairlines laid over a stock UI that
  Xochitl's own repaint does not remove. `Panel::clear` does this. Note that
  it runs on the ordinary exit path only — **a crashed adapter still leaves
  residue**.
- **Presentation goes through the vendor waveform engine**, never raw DRM
  (ADR-0007).
- **Paperclip installs nothing on the root filesystem.** Binaries under
  `/home/root/paperclip`, units runtime-only in `/run/systemd/system`
  (ADR-0008).
- **Never write to `/data`** — device identity state — or to any read-only
  filesystem.

## Standing constraints

These apply to every run in the Paperclip project, not only to recovery:

- **Never enable Developer Mode.** It can erase data. If integration appears to
  require it, stop and raise it as a separate decision.
- **Never downgrade firmware.** Same rule, same escalation.
- The tablet is production hardware. Inspection is read-only until a verified
  backup exists.
- Any destructive or state-changing device operation needs Calum's explicit
  approval first.
- No operation may delete notebooks, credentials, or user data.

## Requirement

Stock reMarkable functionality — Xochitl — must remain usable and recoverable
at all times. This is a release condition, not an aspiration. A Paperclip
session that cannot hand the display back is a failed session regardless of
what it rendered.

## Stop conditions

Stop, mark the affected issue `blocked`, and escalate rather than proceeding,
when any of these is true:

- Stock Xochitl cannot be reliably restored after a custom session takes the
  display.
- A hardware gate a later stage depends on came back **could-not-determine**.
  An unestablished gate is not a passed gate.
- Progress appears to require Developer Mode or a firmware downgrade.
- A verified backup does not exist and Stage 3 or later is about to take the
  display.
- Sleep/lock handoff cannot be made reliable on the target firmware. Per §5
  that means v1 is not ready for unattended normal use — report it rather than
  shipping around it.

## What exists today

Two things, and neither is a working handoff.

The home screen draws a **Return to stock** tile. It is inert. It is there
because the shelf is the Stage 1 deliverable and the handoff is the shape the
shelf has to accommodate, not because any handoff works. See `assumptions.md`,
A7.

`platform/device` supplies the *primitives* a handoff needs, all tested on the
Mac and none exercised on the tablet:

| Primitive | What it does | Proven |
|---|---|---|
| `WakeLock` | takes and releases the kernel wakelock, on every exit path | on scratch files, not on `/sys/power` |
| `DisplayLocks` | reads the vendor advisory locks and says who holds the panel | against the exact bytes the tablet wrote |
| `Panel::clear` | drives the panel white with a settled waveform | on `MemoryPanel` only |
| `VendorPanel` | the vendor waveform engine behind a C ABI | opened and presented on the tablet; **nothing has seen the glass** |
| `device-report` | a read-only readiness report to run on the tablet | run on the tablet; every prediction held |
| `Stock` | stops and restores Xochitl, never kills it, one guarded retry | against a fake systemd, not against systemd |
| `StartBudget` | refuses a session near `StartLimitBurst` | on scratch files |
| `Takeover` | owns the acquire/release order, restores on every path | ordering, drop, **panic**, each failure branch, **and three round trips on hardware** |
| `DetachedWatchdog` | a `setsid` guardian that outlives `SIGKILL` | the mechanism, on aarch64 Linux; not its script |

## What is still owed here

- The verified backup procedure, and how "verified" is checked. (A backup from
  WWW-1 exists on the Mac; the *procedure* is not written down here.)
- ~~How the display is taken, and how it is given back — the sequencing.~~
  Written: `paper_device::Takeover` owns the order and ADR-0011 explains why it
  is that order. Still never run on the tablet.
- What happens on crash, on sleep, and on lock — the save-and-return-to-stock
  behaviour §5 chose over seamless resumption. WWW-21 owns the sleep and lock
  half.
- The manual recovery path, written for someone holding a tablet that is not
  responding as expected.
- Evidence. Every row in the table above that says "never run".

Until those are written from evidence on the device, treat recovery as
unproven. A passing test in this repository is not device qualification, and a
process that starts is not a screen that works.
