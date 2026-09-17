# ADR-0018 — A fourth request: `Launch`

**Status:** accepted (Stage 7, WWW-6)

## Context

ADR-0016 built `Request`/`Action` as a closed set of three — `Redraw`,
`Home`, `ReturnToStock` — quoting §8's own sketch and stating plainly: *"There
is no 'launch that app'... an app influences the platform only through these,
and everything else is the host's decision."* That was correct for every app
built against WWW-5's contract so far, because none of them needed to name
another app.

Home does. Its whole job, per §5, is a shelf a person taps to open something
else. Nothing in the existing message set lets it ask for that: `Request::Home`
means "take me to the home screen", not "I am the home screen, run this
instead of me". Building Home as a real, wire-protocol app — this issue's own
ask — is not possible without closing that gap.

## Decision

Add one variant to both enums:

```rust
pub enum Request {
    Redraw,
    Home,
    ReturnToStock,
    Launch(AppId),
}
```

and the matching `Action::Launch(AppId)`, with `Action::request()` and
`From<Request> for Action` extended to carry it.

This is exactly the kind of change both enums were already built to take:
both are `#[non_exhaustive]` (ADR-0016 predates this decision but the
attribute was already there), so no external match on either one could have
compiled without a wildcard arm in the first place. `CURRENT` moves to
protocol `1.1` — additive, per the rule `version.rs` already states: "every
additive change... a new message, a new field an old app can ignore... is a
minor bump."

`Action` and `Request` lose `Copy` (an `AppId` wraps a `String`), keeping
`Clone`. The one caller that mattered, `runtime::Loop::drive`, already used
`action.request()` and did not need `action` again afterward, so this cost
one call site, not a redesign.

What did **not** change: `Launch` still only *asks*. The host decides whether
the named app exists, is installed, and is runnable — the same authority it
already holds over `Home` and `ReturnToStock`. An app cannot make itself run
by naming itself, because nothing reads `Request::Launch` as anything but a
request.

## Consequences

- Home can be a real app: select a tile, return `Action::Launch(id)`, and the
  host — not Home — decides what happens next.
- Every other app's `Action::request()` call site is unaffected; the match
  arms it already had still compile, because none of them exhaustively
  matched a `#[non_exhaustive]` enum without a wildcard.
- The four-variant shape is still closed in the sense ADR-0016 cared about:
  an app cannot install, cannot go full-screen, cannot address another app
  directly. It can only ask to be replaced by one the host already knows
  about.

## What would make this wrong

If a second app ever needs to ask for something this shape cannot express —
launching with arguments, for instance — that is a new decision, not a
reason to make `Launch` do double duty.
