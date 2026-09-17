# ADR-0006 — Recording device-facing assumptions instead of guessing

**Status:** accepted (Stage 1, WWW-2)

## Context

WWW-2 ran in parallel with WWW-1, the read-only device survey, by Calum's
explicit decision. Nothing in Stage 1 needs the tablet. But the issue is clear
about the trade: any device-facing assumption made here — toolchain target,
rendering approach, input model — is provisional until WWW-1's gate report
lands, and those assumptions must be recorded explicitly so the review gate can
check them against the report rather than hunt for them.

The failure mode this guards against is not making a wrong assumption. It is
making one invisibly, so that when the report contradicts it, nobody connects
the two.

## Decision

Every device-facing assumption is recorded in `docs/assumptions.md` as a
numbered row naming the assumption, the single place it is encoded, and what
changes if WWW-1 says otherwise.

Where an assumption can be avoided entirely, it is avoided rather than
recorded. Specifically: `rust-toolchain.toml` installs **no** cross-compilation
target. The Paper Pro's target triple is not established, and adding one now
would be a guess dressed as configuration — harder to notice, and harder to
revisit, than a row in a table.

Assumptions are encoded as named constants with one definition
(`paper_sdk::SCREEN`, `chrome::MIN_TOUCH_TARGET`), never as repeated literals.
Layout is derived from them, and tests assert relationships — "the board clears
the action row", "every square is at least one touch target" — rather than
pixel positions. A wrong constant then changes one line and the tests still
mean something.

## Consequences

- The Stage 2 review gate has a checklist, not an archaeology exercise.
- Seven assumptions are recorded (A1–A7). Six are encoded in exactly one place
  each; the seventh is deliberately not encoded at all.
- `docs/assumptions.md` also lists what Stage 1 does **not** establish, so no
  test result in this repository can be mistaken for device qualification.

## What would make this wrong

Nothing. If WWW-1 closes every gate favourably, the table becomes a record of
what was checked, which is worth keeping.
