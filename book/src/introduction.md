# Paperclip OS

Paperclip is a personal application environment for the reMarkable Paper Pro:
a home screen, independently running apps, a private catalog, and a Rust
development workflow driven from a Mac. Stock reMarkable software (Xochitl)
must stay usable and recoverable at all times — that requirement is not a
feature of Paperclip, it is a constraint on everything Paperclip is allowed to
do.

This site is built so it cannot describe more than the code actually does.
Every decision chapter is `{{#include}}`d directly from `docs/adr/` in the
repository, and the API reference is published alongside it, so a claim here
is either checkable against real source or it does not appear.

## Two rules

From the product and engineering specification, quoted because they decide
when anything here is finished:

{{#include ../../README.md:two-rules}}

## Decisions

[The ADR chapter](decisions/README.md) is the most complete part of this site
so far: one page per decision, `{{#include}}`d whole, "what would make this
wrong" sections intact. For example, the type that owns the entire
display-takeover-and-restore ordering, argued in
[ADR-0011](decisions/0011-takeover-ordering-and-the-restore-guarantee.md), is
[`paper_device::takeover::Takeover`](api/paper_device/takeover/struct.Takeover.html)
in the generated API reference under `/api`.

## What is not here yet

This is a scaffold, not the finished site (WWW-18). Core-concepts,
architecture, "writing an app", and device-operations chapters are
deliberately not written yet:

- The core-concepts chapter (stock, takeover, session, supervisor, and the
  rest of `CONTEXT.md`'s glossary) and the "writing an app" chapter need the
  paperctl, takeover and app-screen restructuring that WWW-43 stage 2 tracks.
- The architecture chapter's compositor section needs WWW-52, the compositor
  programme, to reach a stable shape — it is still `in_progress` as this
  scaffold lands.
- The "running it on a device" chapter is a planned rehome of
  `docs/development.md`, `docs/packaging.md` and `docs/recovery.md`, not yet
  done.

Writing any of these now would document a shape that is still changing, which
is the mistake this project's stage-11 documentation policy exists to avoid.
