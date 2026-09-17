# Recovery

**Status: nothing in this document has been exercised on hardware.** Stage 1
produced no device code, took no display, and touched no tablet. This file
records the policy Stage 3 onwards must satisfy, so that work starts against a
written standard rather than inventing one under pressure.

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

One thing, and it is a placeholder: the home screen draws a **Return to stock**
tile. It is inert. It is there because the shelf is the Stage 1 deliverable and
the handoff is the shape the shelf has to accommodate, not because any handoff
works. See `assumptions.md`, A7.

## What Stage 3 owes this document

- The verified backup procedure, and how "verified" is checked.
- How the display is taken, and how it is given back.
- What happens on crash, on sleep, and on lock — the save-and-return-to-stock
  behaviour §5 chose over seamless resumption.
- The manual recovery path, written for someone holding a tablet that is not
  responding as expected.

Until those are written from evidence on the device, treat recovery as
unproven.
