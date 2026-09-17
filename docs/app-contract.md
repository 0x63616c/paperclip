# The app contract

What an app must provide to be installable and runnable, and what the platform
promises in return (§8).

> **The SDK is a statically linked convenience layer, not a security boundary.**
> The process protocol is the runtime contract.
>
> `paper_sdk` is compiled into the app's own process. Every check in it is a
> check the app could delete by not linking it. What actually holds an app to
> this document is on the far side of the socket: the framing limits, the
> lifecycle rules, and the OS restrictions the supervisor applies to the
> process. Do not rely on anything in the SDK to stop anything.

## The four layers

Each layer assumes the ones before it were skipped, because each one can be.

| Layer | Enforced by | Where it runs | Catches |
|---|---|---|---|
| Compile-time | `paper_sdk`'s types | The app's build | An app that does not implement the interface |
| Package-time | `paperctl check` | A developer's Mac | A package that could not run if it were installed |
| Launch-time | `paper_protocol::Session` and `codec` | The host process | A binary that starts and then misbehaves |
| Runtime | OS restrictions and supervisor deadlines | The kernel and `paper_host` | An app that ignores all of the above |

The conformance suite is `tests/conformance`, one test target per layer.

## The app interface

```rust
pub trait App {
    type Completion: Send + 'static;

    fn event(&mut self, event: &Event<Self::Completion>, cx: &mut Context<'_, Self::Completion>) -> Action;
    fn draw(&mut self, canvas: &mut Canvas, cx: &mut Context<'_, Self::Completion>);
    fn save(&mut self, cx: &mut Context<'_, Self::Completion>) -> Result<(), SaveError>;
    fn damage(&self) -> Damage { Damage::Full }   // defaulted
}
```

The split between the three is the platform's one hard rule about time:

- `event` **decides**. It runs on the UI loop and must not block.
- `draw` **renders state that already exists**. No network, no filesystem, no
  installation work, no waiting. By the time it is called, everything it needs
  is in `self`.
- `save` **persists**, once, against a deadline the app was given.

State drives rendering. An app never presents a frame; it changes its own state
and returns `Action::Redraw`, and the host decides when to ask.

`Action` is `None` / `Redraw` / `Home` / `ReturnToStock`. `None` has no wire
form — it is the *absence* of a request — so the message set carries `Request`,
which cannot spell "nothing".

## Lifecycle

```
launch ──▶ Hello ──▶ (app maps its surface) ──▶ Ready ──▶ Running ⇄ Suspended
                                                              │
                                                   PrepareToExit(reason, deadline)
                                                              │
                                                     Saved ──▶ closed
```

| Event | Guarantee |
|---|---|
| `Hello` | Always first, always exactly once. Carries everything an app knows about itself. |
| `Ready` | The app's promise that its surface is mapped. Due within `READY_DEADLINE` (2 s). |
| `Suspended` | **Best effort.** The device can suspend between any two instructions. Never save on this. |
| `Resumed` | Foreground again. A draw request follows when the panel is presentable — do not draw on `Resumed`. |
| `PrepareToExit` | Bounded. Carries a reason and a deadline in milliseconds, with a floor of `MIN_EXIT_DEADLINE`. |
| `Saved` | Reports success or failure. Sending it early is the difference between a clean close and waiting out every deadline. |

`LaunchReason` tells an app at its first instruction whether this is `Fresh`,
`Restarted` (the previous process crashed or missed its deadline — the save on
disk may be older than what the user last saw) or `Restored`.

## Input

One event type covers finger, pen, eraser and the desktop preview's mouse.

```rust
pub struct PointerEvent {
    pub at: Point,                    // viewport pixels
    pub phase: PointerPhase,          // Hover | Down | Moved | Up | Cancelled
    pub pointer: Pointer,             // Touch | Pen | Eraser | Mouse
    pub contact: ContactId,
    pub pressure: Option<Pressure>,   // 0.0..=1.0
    pub tilt: Option<Tilt>,           // degrees
}
```

**Missing hardware data is represented as absent, never invented.** A finger
reports no pressure, so touch events carry `pressure: None` — not `Some(1.0)`,
which would be a fact nobody measured. There is no other reason for a `None`.

Hover *distance* is the same rule applied to a field that is therefore missing
entirely: the pen reports `ABS_DISTANCE`, but WWW-1 recorded the axis without
its range, so normalizing it would mean inventing a scale.
`PointerPhase::Hover` carries what is known.

`ContactId` is allocated by the platform and never reused within a session.
Kernel tracking ids are reused the moment a finger lifts, which makes "same id,
same finger" wrong exactly when two fingers swap in one frame. Use it as a map
key and delete the entry on `phase.ends_contact()` — matching only `Up` is the
bug that leaves a rejected palm holding a contact forever.

The eraser is a `Pointer` variant because that is the question apps ask, but it
is not a separate device: the tablet switches `BTN_TOOL_PEN` for
`BTN_TOOL_RUBBER` on the one pen node. `Pointer::is_stylus()` is true for both.

`PointerPhase` is deliberately **not** `#[non_exhaustive]`, while everything
else in the module is. Ignoring an unknown device or an unknown field is safe;
ignoring a state transition leaves a contact that never ends. A new phase is a
compile error at every app, and the protocol minor bump that carries it is what
says those apps need rebuilding.

## Drawing

The app's surface is **viewport-relative**: it draws from `(0, 0)` to
`surface.extent` and never learns where that sits on the panel or what else is
on screen. Pointer coordinates arrive in the same space.

The host owns the memory. `Hello` describes it — extent, stride, ARGB8888 — and
the app writes into it and answers with a `FrameDone` naming the damage. A full
1620×2160 frame is 14 MiB; sending that down a socket per refresh would be the
platform's largest single cost for no gain.

Damage is **advisory**. Claiming `Damage::Full` is always correct and is the
initial path; the host keeps the previous frame and may work the changed
regions out itself, because centralised damage tracking is the only version of
this that stays right when an app gets it wrong. Claiming *less* than changed
is the one harmful answer — on e-ink a stale rectangle stays on the glass.

Frames are issued one at a time. `Request::Redraw` messages that arrive while
one is outstanding are coalesced into the next.

## Background work

`Context::spawn` runs a closure on a thread; its result arrives as
`Event::Completed(C)` on the same queue as input, so an app's state is only
ever touched from one thread and there is no lock anywhere in an app. `C` is
the app's own `App::Completion` type.

That is the whole mechanism — no executor, no task registry, no cancellation.
The App Store's downloads need a thread and a channel; an app with no
background work sets `type Completion = Infallible` and the event arm is
provably dead.

## Storage

Four areas, all arriving as absolute paths in `Hello`. An app that builds a
path out of its own id breaks the first time the host moves anything.

| Area | Capability | Notes |
|---|---|---|
| `assets` | none | The package's own files, read-only. An app that cannot read these cannot draw itself. |
| `private` | `storage` | Survives a restart. `write_private` is write-then-rename, because this is what an app calls when it has been told it is about to be killed. |
| `temp` | `storage` | May be empty on every launch. |
| shared | `sharing` | Only exists because a person initiated an exchange (§4). Names the other app; does not let you talk to it. |

`Storage` returning `NotGranted` is convenience, not enforcement: an app that
skips it calls `std::fs` instead. What stops it is the supervisor —
`paper_host::units::SessionGrants::derive` turns the same
`GrantedCapabilities` into the paths a session may write and whether it may
reach the network, and systemd enforces that against the process (WWW-4).

## Diagnostics

One level and one line. There is no structured payload, no key-value map and no
attachment, and that is a secrets decision rather than a minimalism one: every
field a diagnostic can carry is a field something eventually dumps a token
into, and a platform that offers only a short line makes "log the whole config"
awkward enough that nobody does it by accident. Bounded at
`MAX_DIAGNOSTIC_BYTES` (1 KiB); the SDK truncates, the host refuses.

**The app's id, version and session are attached by the host**, from its own
record of the connection. There is no field on `Diagnostic` to put them in.

## Identity comes from the connection

There is no field anywhere in `AppMessage` carrying an app id, a version or a
session. An app does not announce who it is — it is *told*, in `Hello`, by the
process that launched it. An app that wants to be a different app has to become
a different process.

The conformance suite asserts this over the encoded form, so growing such a
field fails a test.

## Versioning

`ProtocolVersion` is `major.minor`. Patch numbers are absent: a protocol either
changed shape or it did not.

```
host.can_run(app)  ⟺  host.major == app.major && host.minor >= app.minor
```

A host may be newer than the app it runs, never older. An unsupported version
is refused explicitly rather than negotiated down — at package time from the
manifest, and again at launch time from the running binary, because the
manifest is a text file next to the binary and nothing makes them agree.

**App SemVer and protocol version are independent.** Chess 3.0.0 and Chess
0.1.0 may both speak protocol `1.0`, and a protocol bump is not an app release.

Additive changes — a new message, a new optional field, a new `Pointer` variant
— are minor bumps. A new `PointerPhase` is also a minor bump *and* a compile
error at every app, which is the point.

## The wire

Four bytes of little-endian length, then that many bytes of JSON. The length is
checked against `MAX_MESSAGE_BYTES` (64 KiB) **before anything is allocated**,
so a hostile prefix of `0xFFFFFFFF` costs four bytes and a rejection.

The encoder refuses to produce a message the reader would refuse to accept, so
there is no message that is legal to send and fatal to receive.

| Host → app | App → host |
|---|---|
| `Hello` | `Ready` |
| `Lifecycle` (`Suspended` / `Resumed` / `PrepareToExit`) | `Frame` |
| `Pointer` | `Request` (`Redraw` / `Home` / `ReturnToStock`) |
| `Draw` | `Saved` |
| `Goodbye` | `Diagnostic` |

## Limits and deadlines

All in `paper_protocol::limits`, single-sourced because a limit that disagreed
between the SDK and the host would be a protocol where a conforming app gets
killed for a message its own SDK told it was fine to send.

| Name | Value | What it bounds |
|---|---|---|
| `MAX_MESSAGE_BYTES` | 64 KiB | Any encoded message |
| `MAX_DIAGNOSTIC_BYTES` | 1 KiB | A diagnostic body |
| `MAX_DAMAGE_RECTS` | 32 | Rectangles in one `FrameDone` |
| `MAX_SHARED_GRANTS` | 16 | Shares in one `Hello` |
| `READY_DEADLINE` | 2 s | `Hello` → `Ready` |
| `FRAME_DEADLINE` | 500 ms | `Draw` → `FrameDone` (a diagnostic, not a kill) |
| `EXIT_DEADLINE` | 3 s | Default `PrepareToExit` budget |
| `MIN_EXIT_DEADLINE` | 250 ms | The shortest budget an app will ever be given |

## `paper.toml`

Every package has one, at its root.

```toml
[app]
id = "dev.calum.chess"       # stable, reverse-DNS, lowercase
name = "Chess"                # what the shelf shows
version = "0.1.0"             # SemVer, required by §4
protocol = "1.0"              # the platform contract this was built against
entrypoint = "bin/chess"      # relative to the package root
assets = []                   # files the package promises to ship
```

### Field rules

| Field | Rule |
|---|---|
| `id` | Two or more dot-separated segments. Each starts with a lowercase letter and contains only `a–z`, `0–9` and `-`, and does not end with `-`. At most 128 bytes. Safe unescaped as a directory name, a unit name and a URL path segment. |
| `name` | Trimmed, non-empty, at most 48 characters, no control characters. |
| `version` | Valid SemVer, including pre-release and build metadata. |
| `protocol` | `major.minor`. Not SemVer: a protocol either changed shape or it did not. |
| `entrypoint` | Package-relative. No leading `/`, no `..`, no `.` component, no `\`, no `~`, no NUL, at most 512 bytes. |
| `assets` | Same path rules as `entrypoint`. No duplicates. Optional; defaults to empty. |

Anything else in the file is an error. There is no "ignored for forward
compatibility" tier, because a key silently ignored is a key the author
believes is working.

The file itself is bounded: at most 64 KiB (`MAX_MANIFEST_BYTES`) and at most
512 declared assets (`MAX_ASSETS`). The manifest is the first thing the
platform reads from a package nothing has vouched for yet, so its cost is
capped before it is parsed rather than after.

### Validation happens in two steps

`Manifest::parse` decides everything readable from the text. It deliberately
does **not** check protocol compatibility, because the App Store has to be able
to display an app this device cannot run.

- `Manifest::ensure_runnable()` — asks whether this platform build can run it.
- `Manifest::validate_payload(root)` — asks whether the entrypoint and every
  declared asset is actually present, and that none of them is reached through
  a symlink.

The path rules in the table above are *lexical*: they constrain the text in
`paper.toml` and nothing else. `bin/run` is a safe string whether or not it is
a symlink to `/bin/sh` on disk. So `validate_payload` walks every declared path
component by component and refuses any link along the way, including a link at
a directory component — `assets -> /etc` escapes just as completely as
`assets/board.toml -> /etc/passwd` does, and a check that only looked at the
last component would miss it.

## `paperctl check`

The package-time layer at a terminal. `paperctl check` takes a catalog
directory, a `.paperpkg`, or — this stage's addition — a **package source
directory**, and picks the right question from what it was pointed at. A
directory with a `paper.toml` in it is a source tree, and a source tree has
nobody vouching for it yet, so it is checked before it is packaged rather than
after.

```
paperctl check <package-dir>     # requires the `publishing` feature
```

In the order a failure is most useful:

1. The manifest parses — ids, SemVer, protocol shape, paths, bounded sizes.
2. This platform can speak the protocol it asks for.
3. The payload is there and nothing on the way to it is a symlink.
4. Nothing is over its size limit (64 MiB entrypoint, 16 MiB asset, 128 MiB
   package).
5. The entrypoint is an aarch64 ELF executable — read from its 20-byte header.
   This is the only check that distinguishes a program from a shell script.

### On a source tree it says nothing about authenticity

Pointed at a source directory, `check` answers "could this run", never "who
made it", and `PackageCheck` has no accessor that could be mistaken for the
second question.

A package *directory* is not a published thing. The bytes a signature covers
are the archive's, produced deterministically by `paper_packages::archive` and
signed through `paper_packages::signing` and `paper_packages::release` (WWW-7,
ADR-0013). Recomputing an integrity digest over a directory here would be a
second, weaker answer to a question that already has a real one.

## Capabilities are not part of the manifest

An app cannot grant itself anything. This is enforced by types, not by review:

- `Manifest` has no capability field.
- A `capabilities`, `permissions` or `grants` key in a `paper.toml` is a
  dedicated parse error — `ManifestError::SelfGrantedCapabilities` — whose
  message says the decision is not the app's to make.
- `GrantedCapabilities` implements neither `Deserialize` nor `Default` and has
  no public constructor, so no file on disk can produce one.
- The only source of a `GrantedCapabilities` is `InstallPolicy::grant`, and the
  only way to pair one with a manifest is `InstalledApp::install`, which
  requires a policy.

`InstallPolicy::deny_all()` is the starting point. Capabilities are added to it
by the host, per app id.

Current capabilities: `storage`, `network`, `sharing`, `packages`. The list
grows when the host gains the enforcement point that goes with a new entry, not
before — a capability nothing checks is a comment pretending to be a type.

`packages` is the one with a visible enforcement point today:
`PackageManager::on_behalf_of` refuses to hand an installer to an app that was
not granted it. The App Store manages packages because policy granted it that,
not because it is the App Store; it is a client of a platform facility, not the
owner of one.

The `capabilities` list in `Hello` is **informational**: it lets an App Store
without `network` grey out its catalog rather than discover the answer by
failing to connect. It is not what stops the app connecting.

## What is not established

Everything above runs on a Mac. Two things this contract asserts have not been
seen on the tablet:

- **Passing a drawing surface to a launched app process.** The shape is
  specified here; the file-descriptor transport is WWW-4's, and if it turns out
  not to work on this firmware the fallback is inline frames and a protocol
  minor bump. See ADR-0016.
- **That the frame budget is achievable.** `FRAME_DEADLINE` is a stated budget,
  not a measurement.

Passing tests here is not device qualification.
