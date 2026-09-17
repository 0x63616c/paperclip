# ADR-0016 — The app protocol, and what the SDK is not

**Status:** accepted, Stage 6 (WWW-5). **Not exercised on hardware.** Every
test behind this ADR runs on the Mac. Nothing in this repository has yet run
two Paperclip processes on the tablet, so the transport for the drawing
surface — the one part of this design that needs the device to be real — is
specified here and implemented by WWW-4.

## Context

§8 asks for an app contract in four layers: compile-time, package-time,
launch-time and runtime. It also sketches the app interface — `App::{event,
draw, save}` with an `Action` of `None` / `Redraw` / `Home` / `ReturnToStock`
— and calls that a starting point rather than final code.

Three facts from earlier stages shaped what was actually built.

- **The input model had to change.** WWW-10 left `PointerEvent` marked as a
  placeholder and `#[non_exhaustive]` specifically so this stage could design
  it. WWW-1 established what the hardware reports: multitouch protocol B with
  ten slots and a tracking id per contact, pen pressure over 0–4096, tilt in
  hundredths of a degree over ±9000, hover distance, and the eraser as a
  tool-type switch on the one pen node rather than a second device.
- **The panel is 1620×2160 ARGB8888** with no hardware alpha (WWW-20,
  ADR-0007). A full frame is 14 MiB.
- **Suspend can happen between any two instructions**, and the panel is not
  presentable for roughly 77 ms after a resume (WWW-3).

## Decisions

### 1. The protocol crate owns the vocabulary, not just the version

`paper_protocol` was a version number. It is now the whole contract: geometry,
app id, capability names, the input model, the message set, the framing, the
lifecycle rules and every limit.

`AppId`, `Capability`, `RelativePath` and the geometry types moved down into it
from `paper_packages` and `paper_sdk`, and are re-exported from where they
used to live so nothing outside changed. The reason is that each of them is on
*both* sides of the contract — a `paper.toml` declares an app id and a `Hello`
tells an app what its own id is — and two definitions of the same concept drift.

The cost is that `paper_protocol` is a bigger crate than its name suggests, and
that `paper_packages` now re-exports types it does not define.

### 2. The SDK is a convenience layer, and the crate says so

The rule, stated in `docs/app-contract.md`, in `paper_sdk`'s crate docs and in
`paper_protocol`'s: **the SDK is statically linked into the app's own process,
so it is not a security boundary and cannot be one.**

Everything that binds an app is enforced on the far side of the socket. The
practical consequence is where the code lives: the framing limits and the
lifecycle state machine are in `paper_protocol`, which the *host* links, not in
`paper_sdk`, which the app links. The conformance suite's runtime layer builds
bytes by hand and never calls an SDK function, because that is what an app is
free to do.

What the SDK's own checks are for is that an honest app fails at the point of
the mistake with a typed error, instead of being killed later by a supervisor.

### 3. Pixels do not travel on the wire

The host owns the drawing surface, describes it in `Hello`, and the app writes
into it and answers with a `FrameDone` naming the damage. 14 MiB per frame
through a socket would be the platform's largest single cost and would buy
nothing: the host has to end up with those bytes in a mapping the waveform
engine can read either way.

How the memory reaches an app process on the device — a file descriptor sent
with the launch connection, mapped before `Ready` — is WWW-4's. This stage
specifies the shape (`SurfaceDescriptor`) and ships one implementation,
`LocalSurface`, which allocates its own buffer and is what the desktop preview
and every test use. It is a real implementation rather than a mock: the drawing
path above it is the same code the device runs.

**This is the open risk in this ADR.** If passing a mapping to a launched
process turns out not to work on this firmware, the fallback is inline frames
on the wire, and the measurement that decides it has not been taken.

### 4. Length-prefixed JSON

Four bytes of little-endian length, then that many bytes of JSON. The length is
checked against `MAX_MESSAGE_BYTES` *before* anything is allocated.

JSON because the wire carries control messages at a few hundred per second at
worst and never carries pixels, so a compact encoding saves nothing measurable
and costs the thing that is actually scarce: a frame you can read in a log
while a tablet misbehaves in a room with no debugger.

What would make this wrong is a message rate nobody has anticipated — if pen
input at full rate ever turns out to matter more than legibility, this is a
minor protocol bump and a different codec behind the same `codec` module.

### 5. The input model carries what the hardware reports, and nothing else

- Position in viewport pixels, pressure as a fraction of full scale, tilt in
  degrees. Normalized, because `4096` is a fact about this digitizer and an app
  that hard-codes it breaks on the next one.
- **Missing data is absent.** A finger reports no pressure, so touch events
  carry `None` — not `Some(1.0)`.
- **Hover distance is omitted entirely.** The axis exists and the pen reports
  it, but WWW-1 recorded the axis without its range, so there is no honest way
  to normalize it. `PointerPhase::Hover` carries the part that is known; a
  distance arrives, as a new optional field, when something measures the axis.
- **Contact ids are allocated by the platform, never echoed from the kernel.**
  Kernel tracking ids are reused the moment a finger lifts, which makes "same
  id, same finger" wrong exactly when two fingers swap in one frame.
- `PointerPhase` is deliberately **not** `#[non_exhaustive]`, while everything
  else in the module is. A phase is a state transition, and an app that
  silently drops one it does not recognise is an app holding a contact that
  never ends. A new phase should be a compile error at every app, and the minor
  protocol bump that carries it is what tells the App Store those apps need
  rebuilding.

### 6. An app never says who it is

There is no field in any `AppMessage` carrying an app id, a version or a
session. All three are the host's knowledge about the connection it launched,
and `Diagnostic::tagged` is the only place they are attached. A log line saying
`dev.calum.chess` is therefore evidence that the process the host launched as
Chess wrote it, rather than evidence that something typed that string into a
message.

The conformance suite asserts this over the *encoded* form, so growing such a
field fails a test rather than passing review.

### 7. `paperctl check` is the package-time layer; authenticity is not in it

One command runs everything decidable about a package before anything runs it,
including reading the entrypoint's ELF header — 20 bytes — to answer "could
this run on that tablet".

It says nothing about who built a package, and has no accessor that could be
mistaken for saying so. The bytes a signature covers are the archive's, which
WWW-7 settled in [ADR-0013](0013-package-archive-and-the-signed-release-envelope.md):
`archive::build` is deterministic and `signing` states exactly which bytes are
signed. An integrity digest computed over a *directory* here would be a second,
weaker answer to a question that already has a real one.

An earlier draft of this stage did invent one — a `paper.sum` beside the
package — before WWW-7's work landed. It is recorded here because the deletion
is the decision: two mechanisms for "are these the right bytes" is worse than
one, even when the weaker one is honest about its limits.

## Consequences

- Apps depend on `paper_sdk` alone. `paper_protocol` is re-exported through it.
- `paper_device`'s `ContactEvent` and `Tool` are gone; the decoders emit
  `PointerEvent` directly, which is what the type comment in WWW-3 predicted
  would happen once the SDK event was widened.
- The pen decoder now reports hover, which it previously dropped for want of a
  phase.
- Adding a message, a field or a pointer kind is a minor protocol bump. Adding
  a `PointerPhase` breaks every app on purpose.

## What would make this wrong

- **Surface passing does not work on this firmware.** Then decision 3 is wrong
  and frames go inline. Everything else survives.
- **The frame rate makes JSON measurable.** Then decision 4 is wrong and the
  codec changes behind the same module boundary.
- **The hover distance axis turns out to have a documented range.** Then the
  field that was deliberately left out gets added, additively.
- **Two apps need to talk to each other rather than through a shared
  directory.** Nothing in this message set allows that, deliberately, and it
  would be a new contract rather than an extension of this one.
