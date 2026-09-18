# 0027 — Paperclip is the resident; stock is one of the things it may grant the display to

Stage 2 (WWW-49).

## Decision

Paperclip is the resident system. Stock reMarkable functionality (Xochitl) is
an app Paperclip may grant the display to, on the same mechanics as Home or
any other app — not a default the tablet boots into and Paperclip borrows
from.

This is a reinterpretation of the release condition in `docs/recovery.md`:

> Stock reMarkable functionality — Xochitl — must remain usable and
> recoverable at all times.

Read before this ADR as "stock is the default, and Paperclip must always be
able to hand it back." Read after: "stock is *usable and recoverable*" — the
user can always reach it — not that it is what the tablet shows by default.
`docs/recovery.md`'s wording ("usable and recoverable") already survives the
inversion unchanged; what changes is which side of the relationship is
assumed. That distinction is worth writing down rather than letting the two
readings coexist silently, because the difference is exactly what "Paperclip
is the OS" means or does not mean.

## The mechanics were already symmetric; the framing was not

`platform/host::state::Foreground` — `Stock`, `Home`, `App(AppId)` — has
modelled stock as one of three grantable owners since WWW-4, not a privileged
fourth thing outside the enum (`platform/host/src/state.rs:70-92`).
`session_unit_name` (`platform/host/src/linux/runtime.rs`) generates a unit
template per owner the same way for all three. The state machine's *shape*
already had no special case for stock; the module docs, the `Takeover` type's
own framing ("taking the display from stock, and giving it back"), and
`docs/recovery.md`'s phrasing did. This ADR is mostly that: making the
existing shape the *stated* model rather than a README aspiration sitting on
top of code that quietly agreed.

`paper_device::takeover::Takeover` keeps its current framing and is not
renamed. It is not a generic "grant the display to any app" mechanism and
should not become one: it exists because Xochitl specifically needs an
out-of-band safety protocol nothing else does — the vendor waveform engine's
advisory lock (`/tmp/epframebuffer.lock`), a process-lifetime-bound library
handle (ADR-0009), and a start-rate limiter whose `OnFailure=` reaches a
serial console (ADR-0011). An ordinary Paperclip app switch goes through
`paperclip-session.target`'s `Conflicts=`, with no equivalent hazard. Stock is
handled specially in the code because it is mechanically special, not because
it is privileged in the model — and that distinction is the ADR's whole
argument, so `Takeover` staying stock-specific is consistent with it, not an
exception to it.

## What this ADR does not change, and why

`platform/host::state::Machine::new` (`platform/host/src/state.rs:464-467`)
still initialises `state: SessionState::Stock, foreground: Foreground::Stock`
— the supervisor still *boots believing* stock owns the display
(`Supervisor::new`'s own doc comment says so,
`platform/host/src/linux/runtime.rs`).

That is left alone deliberately. On the tablet today, Xochitl's own systemd
units start before Paperclip's supervisor does — there is no A/B boot control,
no safe mode, and no mechanism yet by which Paperclip could be what the panel
shows the instant the system comes up. Changing `Machine::new`'s default to
`Foreground::Home` would make the state machine's belief diverge from what is
actually on the glass at supervisor start, which is a correctness bug, not an
inversion: the ordering guarantee `platform/device` and `platform/host` both
depend on is that the machine's model of the world matches the world. Fixing
that for real is a boot-sequencing change — A/B slots, a boot-failure counter,
safe mode — and it is **WWW-53**'s (`Run at boot, safely`), gated on stage 3,
not this ticket's. Flipping the default here would trade a documented gap for
an undocumented one, and the standing instruction on this project is not to
weaken a guarantee without replacing it in the same change or naming the
child that does. WWW-53 is that child.

Until WWW-53 lands, "Paperclip is the resident" is true of the state machine's
*shape* (§ above) and false of its *boot-time default*, and both halves of
that sentence are written down here rather than one of them being quietly
assumed.

## Consequences

- No behaviour changes on the device from this ADR alone. `Foreground`,
  `session_unit_name`, and the supervisor's boot default are unchanged; what
  changed in the same commit (WWW-49) is `platform/host::linux::runtime`
  sitting on `platform/sys::UnitControl` instead of a concrete, untested
  `Systemd`, and a systemd start refusal being queued as an event immediately
  rather than discovered at the switch deadline — both preconditions for
  stock behaving like any other grantable app rather than a special case the
  supervisor could only find out about by timing out.
- `docs/recovery.md`'s release condition stands, reworded to cite this ADR
  rather than silently carrying two readings.
- Any future code that special-cases stock beyond what §"mechanics" above
  names (the vendor lock, the process-lifetime handle, the start limiter)
  should be read as a regression toward "stock is default" and questioned.

## What would make this wrong

- If Xochitl ever gains its own working `OnFailure=` target (ADR-0011 names
  the same condition), the mechanical case for `Takeover`'s special handling
  weakens, and it may become approachable as an ordinary app switch. Not
  reached yet.
- If WWW-53 lands and finds that Paperclip cannot in fact be first on the
  panel — a vendor boot dependency that cannot be reordered without
  Developer Mode, say — then "Paperclip is the resident" is false at the
  layer that matters most and this ADR's claim should be narrowed to "true in
  the running state machine, false at boot" explicitly, not left to imply
  more than WWW-53 established.
