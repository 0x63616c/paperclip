# ADR-0037 — The compositor accept loop, its client wire, and crash/hang isolation

**Status:** accepted (WWW-52, WWW-78). Builds on ADR-0033 (WWW-77) — see
"What stays out of this stage" there for exactly which of this ADR's
decisions it left open.

## Context

WWW-77 built the compositor's buffer lifecycle (`pool`, `surface`) as pure
state with no socket, no client connection and no running process, and named
three things explicitly out of scope: "the non-blocking accept loop and
crash/hang teardown are WWW-78." WWW-78's own acceptance criteria:

- Killing an app with `SIGKILL` leaves the panel showing a legible frame and
  returns to Home.
- A client that stops responding does not stall the compositor.

Plus scope named in the ticket body: EOF teardown must drop exactly the dead
client's surfaces/buffers/mappings without touching the compositor's event
loop or the panel fd, and a wedged client must never cost a blocking `read`.

## Decision

### The client wire is new, and lives in `paper-compositor`, not `paper-protocol`

ADR-0033 left open whether a compositor client's protocol merges into
`HostMessage`/`AppMessage` or stays "a distinct wire format." Those two enums
are `#[non_exhaustive]` and closed by `message.rs`'s own doc comment ("a
message that is not in `HostMessage` or `AppMessage` does not exist"), and no
real client speaks this wire yet (WWW-81 moves one onto it) — designing the
merge now would be exactly the speculative abstraction WWW-77's own
"Out of scope" section already ruled out once.

So `platform/compositor/src/wire.rs` defines its own small set: `ClientHello`
(role, label, the pool descriptor whose fd accompanies it), `ClientRequest`
(`Attach`/`Commit`, reusing WWW-77's `BufferSlot`/`Damage`), and `HostEvent`
(`Released`). Framed with `paper_protocol::codec`'s existing length-prefixed
JSON — reusing the framing, not the message set, keeps this wire consistent
with the rest of the platform's "read a frame in a log" reasoning
(`codec.rs`'s own doc) without touching the closed enums.

### `ClientHello` carries its fd via `SCM_RIGHTS`, in one `sendmsg`/`recvmsg`

ADR-0033 named this directly: "`Pool::from_file` takes an already-open,
already-sized `File` — production will hand it one built from a received fd
(WWW-78)." `platform/compositor/src/fdpass.rs` implements `send_with_fd`/
`recv_with_fd` over raw `libc::sendmsg`/`recvmsg`, following `pool.rs`'s
pattern (`#![allow(unsafe_code)]` at the module, a written reason on every
block). One call each side, not a `write` followed by a separate fd send:
`SCM_RIGHTS` on a stream socket is associated with whichever regular bytes
travel in the *same* call, so splitting it across two would only reliably
attach the fd to whichever chunk happened to arrive first.

The control buffer is a `union` of `[u8; CONTROL_LEN]` and `libc::cmsghdr`
rather than a plain byte array, so the buffer's alignment is `cmsghdr`'s own
rather than 1 — the alignment `CMSG_DATA`'s pointer arithmetic assumes when
it hands back where to read or write the fd.

### Non-blocking is the whole answer to "a wedged client stalls nobody"

Every fd — the listener, a pending connection, a registered client — is set
non-blocking the instant it exists. `Compositor::run_once` calls `libc::poll`
exactly once per tick across every fd at once with one bounded timeout; no
`unsafe` outside that single `poll` call, everything else in `server.rs` is
plain non-blocking reads. This workspace has no async runtime (no `mio`,
`tokio` or `polling` anywhere in the tree — checked before writing this), and
the closest existing precedent, `tools/paperctl/src/session.rs`'s
`set_read_timeout` on a *blocking* socket, does not generalise to servicing
several clients from one loop: a blocking read with a timeout still blocks
every other client for up to that timeout. `poll` plus non-blocking fds is
what makes "costs one entry in an array" rather than "costs a blocked
syscall" literally true, which is what the ticket asks to be demonstrated
rather than asserted.

`HELLO_DEADLINE` (2s, sized like `paper_protocol::limits::READY_DEADLINE`)
exists only to stop a connection that never completes its handshake from
being a permanent entry — a slow resource leak, not a stall, and a different
problem from the one non-blocking I/O already solves structurally.

### Framing over a non-blocking fd needs its own accumulator

`paper_protocol::codec::read_message` assumes a `Read` that blocks until a
whole frame arrives. Calling it against a non-blocking socket that stops
mid-frame would lose the 4-byte prefix it already consumed the moment the
next call starts by reading a fresh prefix from what is actually the middle
of the previous frame's body. `server.rs`'s private `FrameReader` accumulates
bytes across reads and only ever hands back a frame once one has fully
arrived, leaving any remainder buffered for next time.

The same accumulator has to seed itself from whatever a single `recvmsg`
handshake read already picked up past the hello's own frame: on a fast local
socket, a client that draws its first frame immediately rather than waiting
for anything back can have its `Attach`/`Commit` sitting in the same kernel
buffer as its `ClientHello` before the compositor ever calls `poll` again.
`Compositor::finish_handshake` reports how many bytes `decode_hello` consumed
and feeds the rest into the new client's `FrameReader` before returning —
found by a failing integration test (every "commit reaches the panel"
scenario failed until this was added), not anticipated up front.

### Foreground, Home, and the stopped frame

`Compositor` tracks one `Foreground` (`None` / a client / `Stopped{label}`)
and at most one registered Home client. Policy, deliberately minimal because
no real client exists yet to design fuller arbitration against (that is
WWW-81's and the app-switching ticket's job, not this one's):

- The first client to register becomes foreground if nothing is yet.
- Any `App`-role client that registers becomes foreground unconditionally
  (single-foreground-app, per the project's v1 scope).
- `Home`-role registration only takes foreground if nothing else has it.

On EOF, a protocol violation, or (for a still-pending connection) the
`HELLO_DEADLINE` expiring, `Compositor::disconnect` removes exactly that
client's `Surface`/`Pool`/socket. If it was foreground, `stopped::
render_stopped_frame` draws "`<label> stopped`" — synthesised by the
compositor itself, since the dead client's own last frame is exactly the
content a crash cannot be trusted to have left legible — and presents it
with a full refresh (no trusted prior frame to diff a partial update
against). If a Home client is still connected, foreground switches to it and
its last committed frame (if any) is presented; if Home itself was what
died, foreground becomes `None` and stays on the stopped frame — there is
nowhere else to fall back to, and that is a state this ticket leaves rather
than invents a second fallback for.

### Presenting a client's pixels bypasses `paper_device::panel::present`

`present` takes a `paper_sdk::Canvas`, and a client's pool slot is a raw
`&[u8]` this crate never rasterised. `present::present_pool_slot` blits
directly into the panel's own buffer and calls `Panel::swap` itself. This
means the per-present EINK telemetry `present` records is not recorded on
this path — a gap, left open below rather than closed by adding a
`Canvas`-from-raw-bytes constructor to `paper_sdk`, which is a bigger surface
change than this ticket's crash/hang scope needs. `stopped::
render_stopped_frame` does go through `present` (it has a real `Canvas`), so
only the ordinary client-frame path has this gap.

Presentation always covers the whole extent regardless of a commit's
`Damage` claim — `Damage::Full` is always a correct superset per its own
doc, and partial-rectangle blitting straight into the panel buffer is an
optimisation this ticket's acceptance criteria do not need.

## What stays out of this stage

- **No `[[bin]]`, no well-known socket path, no wiring to the vendor panel or
  to a real client.** `Compositor` is a library any caller can drive against
  a real `UnixListener` and any `paper_device::Panel`. Turning this into
  something the supervisor starts at boot, and moving Home/the test card
  onto this wire for real, are WWW-81 and beyond.
- **No buffer-release backpressure handling.** `notify_release` is
  best-effort: a non-blocking write that cannot complete right now is
  dropped rather than queued or retried, since no real client reads
  `HostEvent::Released` yet.
- **No idle-kill for an already-registered client that goes silent.**
  Non-blocking I/O already stops it from stalling anyone else; killing it
  for inactivity is a policy question out of this ticket's scope.
- **No multi-app arbitration beyond "the newest App wins."** A real launch
  sequence, focus history, and anything about *which* app should become
  foreground are later work.

## Consequences

- New modules in `platform/compositor`: `wire`, `fdpass`, `present`,
  `stopped`, `server`. `paper-compositor` gains dependencies on `paper-device`
  and `paper-sdk` (for `Panel`/`MemoryPanel` and `Canvas`/text respectively)
  and on `serde` directly (for the new wire types).
  `platform/compositor/examples/test_client.rs` is a small real client used
  only by the `SIGKILL` integration test.
- 9 new unit tests (`wire`, `fdpass`, `present`, `stopped`) and 7 integration
  tests in `platform/compositor/tests/crash_and_hang_isolation.rs`, one of
  which (`sigkilling_the_foreground_app_shows_a_stopped_frame_and_returns_to_home`)
  spawns a real process and sends it a real `SIGKILL`; the rest drop an
  in-process `UnixStream` end to simulate the same kernel-visible EOF, since
  `Compositor::disconnect` cannot and does not distinguish the two.

## What would make this wrong

- **If a real client's handshake plus first frame does not reliably land in
  one `recvmsg`** — the `FrameReader`-seeding fix above assumes a local
  socket coalesces a fast sender's successive writes often enough to matter,
  but does not assume they always do; a client whose hello and first commit
  straddle two separate reads is still handled correctly (the second arrives
  through the ordinary `service_client` path), just not exercised by this
  ADR's own reasoning for *why* the fix was needed.
- **If the "newest App wins" foreground policy is wrong once a real launch
  sequence exists** — it was chosen only to make this ticket's acceptance
  criteria testable, not as a considered design for app switching.
- **If skipping EINK telemetry on the client-frame present path matters
  before WWW-81** — it currently doesn't, because nothing drives that path
  outside this ticket's own tests.
