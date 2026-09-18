# ADR-0036 — Why takeover, not in-process

**Status:** accepted (WWW-73).

## Context

`Takeover` is load-bearing throughout this codebase — `CONTEXT.md` calls the
order of its acquire/release "the whole of the safety argument" and cites
ADR-0011 — but ADR-0011 only covers *ordering*, once takeover is already the
chosen mechanism, and ADR-0007 only covers how presentation happens once
Paperclip already owns the display. Nothing argues for taking the display over
at all, as opposed to running Paperclip inside or alongside Xochitl. This ADR
is that argument, not a new decision: the two candidates below were never
live options for reasons already established on hardware and already decided
in this project, and this ADR is where those reasons are collected under one
question for the first time.

The candidates actually available, given how this device works:

1. **In-process within Xochitl** — a Paperclip app (or the whole platform)
   runs as a component loaded into the `xochitl` process itself, sharing its
   Qt event loop and its hold on the vendor engine, rather than Paperclip
   stopping it and presenting on its own.
2. **Side-by-side coexistence** — Xochitl and a Paperclip session both keep
   running, each presenting to different regions or at different times,
   without either stopping the other.

## Decision

Paperclip takes the display over, stopping `xochitl.service` for the duration
and restoring it afterward (ADR-0011), rather than running in-process or
alongside stock. Four independent reasons, any one of which would be
sufficient on its own:

### 1. The vendor engine has exactly one owner; coexistence is not a design choice, it is unavailable

ADR-0007 established on hardware that DRM master is exclusive, and that the
vendor waveform engine enforces the same thing one layer up: `libqsgepaper.so`
guards presentation with an advisory lock (`checkLockFile`, and the string
`"Failed to lock epframebuffer. Is there another EPFramebuffer instance?"`
inside the library itself). There is no mode in which two processes hold the
panel at once. Coexistence was never a tradeoff against takeover — it is
foreclosed by the same hardware fact ADR-0007 already used to rule out raw
DRM, before recoverability or isolation enter the argument at all.

### 2. Xochitl is vendor firmware Paperclip cannot modify or extend

"In-process" means loading Paperclip's own code into the `xochitl` binary —
a Qt/QML plugin it loads, or a recompiled unit. Two standing facts rule this
out independently of whether it would otherwise be a good idea:

- The root filesystem, where `xochitl` and its plugin search paths live, is
  read-only to every process on the device, Paperclip included
  (`docs/recovery.md`'s hard device rules; ADR-0024 confirms no code path in
  this repository patches a vendor binary today). There is nowhere to install
  the component.
- The only way to gain write access to that filesystem is Developer Mode,
  which this project's standing constraints ban outright because it can erase
  the device. Working around the read-only root to make in-process possible
  would mean crossing a rule with no exception clause, for an integration
  shape that is not otherwise required.

In-process is not a rejected alternative so much as one this project already
has no path to, under constraints it decided before this ADR existed.

### 3. Xochitl has no recovery budget of its own; takeover keeps Paperclip's risk out of it

`docs/recovery.md` requires stock to "remain usable and recoverable at all
times", and ADR-0027 is explicit that this means *reachable*, not *default*.
`xochitl.service` has `StartLimitBurst=4` and an `OnFailure=` target
(`remarkable-fail.service`) that does not exist on this image — so a crash
inside it runs `systemd-sulogin-shell` on a serial console (ADR-0008,
ADR-0011). Xochitl was never written to host third-party, independently
versioned, catalog-installed app code, and it has no supervisor of its own to
absorb a crash in that code. Embedding Paperclip apps inside it would put
evolving app code — initially Chess, later anything the catalog carries —
inside the one process whose failure mode is already the worst case this
project guards against.

Takeover instead keeps Xochitl in exactly two states, stopped or running
cleanly, and moves all of Paperclip's own crash risk into `paper-host`, a
process built for it: a supervisor with deadlines, a watchdog, and the
four-layer restore guarantee ADR-0011 proves (`Drop`, an interrupt flag, a
detached `setsid` guardian, and the vendor-lock restoration ADR-0011 added
after the incident that motivated it). None of that exists for `xochitl`, and
none of it could be retrofitted into a closed vendor binary Paperclip cannot
change.

### 4. It is the same bet §11 already made, applied one level up

`docs/isolation.md` and `platform/host/src/report.rs` both record the
project's confirmed priority: "§11 chose crash recovery over hostile-app
isolation." Paperclip apps already run with weak *mutual* isolation (shared
session uid, no MAC, no seccomp on this image) in exchange for a supervisor
that can always recover the display. Takeover is that same tradeoff applied to
the boundary between Paperclip and stock: rather than trying to build an
in-process isolation boundary inside a single Qt process — something this
project has already decided is not worth attempting even *between* its own
apps — Paperclip keeps stock and itself as two separate OS processes with one
exclusive, ordered handoff between them, and spends its engineering effort on
that handoff (ADR-0011) rather than on containment inside a shared process.

## Consequences

- Closes the gap the project description names: "why takeover, not in-process"
  had no owner; this is it, alongside WWW-68's HTTPS catalog transport ADR.
- ADR-0007 and ADR-0011 are now downstream of a stated reason to take the
  whole display over in the first place, rather than a premise both quietly
  assumed.
- `CONTEXT.md`'s **Takeover** glossary entry cites this ADR alongside
  ADR-0011.

## What would make this wrong

- If Xochitl ever gained a documented, writable extension point that did not
  require root-filesystem access or Developer Mode. No such thing exists on
  this image today, and reason 1 (the vendor engine's single ownership) would
  still apply even if it did.
- If the vendor engine ever supported more than one concurrent owner. ADR-0007
  established the opposite on hardware, on the current firmware.
- If §11's crash-recovery-over-isolation priority is itself revisited. That
  would reopen the isolation question generally, of which this ADR's fourth
  reason is one instance, not the whole argument — reasons 1-3 would still
  stand on their own.

## Provenance

Reasons drawn from facts and decisions already established elsewhere in this
repository, not new research: ADR-0007 (vendor engine exclusivity), ADR-0008
and ADR-0011 (Xochitl's failure mode and the restore guarantee), ADR-0012
(the supervisor's shape), ADR-0024 (no vendor binary is patched; root
filesystem is read-only), ADR-0027 (stock is reachable, not default),
`docs/recovery.md` (the recoverability requirement and the Developer Mode
ban), and `docs/isolation.md` (§11's crash-recovery-over-isolation choice).
