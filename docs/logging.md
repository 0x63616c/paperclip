# Logging and tracing standard

WWW-55 spent most of a device session paying for the absence of this file.
Every defect it found was diagnosed by reading systemd's exit codes and
guessing, because the code that failed said nothing useful — or nothing at
all. The only instrument a person watching the tablet has is the journal, so
the journal has to answer "what happened, in what order, and how long did it
take" without a rebuild. This is the standard that makes that true, and it
binds every crate in this workspace.

`tracing` is the API everywhere: a crate that wants to log calls
`tracing::info!`/`warn!`/`error!`/`debug!`/`trace!` directly and depends on
nothing else. `paper_telemetry` is the sinks, not the calls — see its module
doc and ADR-0025 for `init_pretty` (Mac) versus `init_journald` (device) and
why they differ.

## Levels

| Level | Means | Example |
|---|---|---|
| `error` | This process could not recover locally; a person needs to know now. | A restore attempt failed outright. |
| `warn` | Something failed or was refused, but the system continued — including every silent failure this standard makes loud (see below). | `systemctl` refused a start; a progress witness could not be written. |
| `info` | A lifecycle fact a person reads first, start to finish, to answer "what happened, in what order, how long did it take." | App connected; first frame; foreground switch and its reason; session end and its reason; restore outcome and duration. |
| `debug` | Internal detail useful while troubleshooting, too noisy for normal operation. | State-machine internals not already covered by an `info` transition line. |
| `trace` | Full-rate, per-event detail. Never the default. | Every decoded pointer event; every gesture arbitration verdict. |

`EnvFilter` (both `init_pretty` and `init_journald`) defaults to `info` when
`RUST_LOG` is unset. That default *is* "input tracing off" — there is no
separate flag. To turn it on for one session:

```sh
RUST_LOG=info,paper_compositor::gesture=trace,paper_device::input=trace journalctl ...
```

**Nothing that touches the panel may log per frame at `info`.** e-ink sessions
are mostly idle and the journal is on flash — full-rate output belongs at
`trace`, gated off by default, exactly like input tracing above.

## Required fields

A log line about one session, one owner, or one app carries that as a
structured field, not prose folded into the message — `session = %id`,
`owner = ?foreground`, `app = %app_id`, the same convention
`paper_telemetry::diagnostic::record` already uses for every `DiagnosticRecord`
it re-emits. A field can be filtered and grouped by `journalctl -o json` and
grepped for; a sentence describing the same fact cannot.

## Never swallow a cause

`paper_sys::Systemctl` was this ticket's worst offender and is the fix to
copy: it read a failed `systemctl`'s reason from `stdout`, which a refusal
never writes to — `systemctl start`/`stop` write there — so
`UnitError::reason` was silently empty on every real failure, and the
supervisor logged `systemd refused to start paperclip-app@home.service:
failed: ` with nothing after the colon. The real cause, `226/NAMESPACE`, was
only ever in `journalctl -u` for the unit itself (see
`platform/sys/src/unit.rs`'s `failure_reason`, fixed in this ticket, and its
test `a_failed_start_carries_systemds_stderr_as_a_non_empty_reason`).

The rule this generalises to: a `map_err` that discards the source error's
text, or an `Err(_) => …` arm that pattern-matches away the reason, produces
exactly this failure mode elsewhere. Before writing either, ask whether the
next person reading the journal after a failure can tell what happened from
what you kept. If not, keep more of it — the underlying error's `Display`, at
minimum — even when the caller only needs to know that it failed.

## Durations are fields, not prose — and not implicit span timing

Every operation that can be slow gets a duration as a structured field on the
`info`/`error` event that reports its outcome — `duration_ms = started.elapsed
().as_millis() as u64` — not folded into a message string. `platform/host/src/
linux/runtime.rs`'s `restore` is the worked example: `duration_ms` on both the
success and failure arm, because a restore is two guarded starts with a
25 s wait each and "restore attempt 1: ok" told a reader nothing about which
half of that budget was spent.

The list from the WWW-55 postmortem that still needs this treatment as each
touches it: unit start, socket connect, handshake, first frame, takeover,
upgrade, materialisation. Add `duration_ms` at the point each reports success
or failure; do not wait for a dedicated ticket to do the one your change
already touches.

**Do not rely on `tracing`'s span-close timing to carry this to the device.**
`init_pretty` now requests `FmtSpan::CLOSE`, so a span's own duration prints on
a Mac. `tracing-journald`'s `Layer` implements no `on_close` at all (checked
against its source, not assumed) — a span closing produces nothing in the
journal, which is the one instrument this whole standard exists for. A span is
still worth opening, for `journalctl`'s nesting (ADR-0025), but the duration
itself has to be an explicit field on an event, or it never reaches the
device.

## Lifecycle at `info`

These are the lines a person reads first, and today most of them do not
exist: app connected, first frame, foreground switch with its reason, session
end with its reason. `platform/host/src/linux/runtime.rs`'s transition log
(one `info!` per state-machine step that changed something) is the existing
example to extend, not replace, as each of these lands.

## Make silent failures loud

Anything currently swallowed with `let _ = ` or `.ok()` that affects behaviour
logs at `warn` with the path and the underlying error, once fixed — a `let _
=` is only still correct where the operation's failure genuinely changes
nothing a reader would want to know about (a best-effort cleanup of a file
that may never have existed is fine to leave silent; a witness a session could
not publish, or a directory a supervisor could not create, is not).

Two fixed in this ticket, as the pattern to copy:

- `tools/fault-app/src/main.rs`'s `spin`/`loop_forever`/the memory-pressure
  loop used to discard `MainLoopProgress::tick`'s error outright — the exact
  WWW-55 symptom, "the progress witness could not be written and said
  nothing." Now `eprintln!`s the error, matching this tool's own existing
  convention for reporting an outcome to the harness.
- `platform/host/src/linux/runtime.rs`'s progress-poll `Err(_) => {}` arm
  correctly treats an unreadable witness as no progress (§10 asks for exactly
  that), but reaching that arm at all means the file exists and is corrupt —
  worth a `warn!`, which it did not get before.

## Input tracing belongs in the imperative shell, not the pure core

`paper_compositor::gesture::GestureDetector` and `paper_device::input`'s
decoders are deliberately pure — no clock, no I/O, tested against captured
byte traces — and no other pure module in this workspace (`paper_host`'s
`state`, `policy`, `facilities`, `report`) calls `tracing` either. Keep that
split: add the "log every decoded pointer event and every arbitration
verdict" tracing this ticket asks for at the call site that drives them, not
inside them. That call site — the compositor event loop wiring evdev decode
into `GestureDetector::arbitrate` — is WWW-78's, and does not exist yet
(`arbitrate` currently has no non-test caller in this workspace). This part of
the standard is written for whoever lands WWW-78 to follow immediately, not
deferred as a "someday."

## What must never be logged

- Notebook content or file contents, ever.
- Any path under `/home/root/.local` beyond the logging app's own storage
  root.
- Secrets: dropbear host keys, `authorized_keys`, NetworkManager Wi-Fi PSKs,
  Xochitl's `UserToken`/`devicetoken` — the same list the project description
  already names for the WWW-1 backup archive. If a value came off the device
  and is not something you typed, assume it needs scrubbing before it reaches
  a log line, a comment, or an issue.

## Constraints this standard does not change

- `paper-sdk` must not gain a dependency on `paper_telemetry` or any
  supervisor crate — it is what third-party apps link. An app logs by calling
  `tracing::*!` directly, same as everything else; if a future need means
  `paper-sdk` itself must share a logging constant (a field name, an env var)
  with the supervisor, it goes in `paper-protocol`, which `paper-sdk` already
  depends on and the supervisor crates do not need to be pulled in for.
