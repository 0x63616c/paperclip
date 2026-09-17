# Specification

The authoritative product and engineering specification for Paperclip is
`pasted-text.txt`, dated 2026-09-16, attached to every issue in the Paperclip
project in Multica. It is not vendored here: there is one copy, it lives where
the project does, and a second copy in a repository would start drifting from
it the first time it changed.

Section numbers quoted in source comments, ADRs and commit messages refer to
that document.

## Decision vocabulary

Used throughout the specification and binding in this repository:

| Term | Meaning |
|---|---|
| **Requirement** / **must** | A release condition. |
| **Proposed** | An implementation choice to validate before adopting. Not evidence of hardware support. |
| **Hardware gate** | A fact or behaviour that must be established on the actual tablet before downstream work is trustworthy. |
| **Deferred** | Deliberately outside v1. |

## Sections Stage 1 works against

| Section | Subject |
|---|---|
| §4 | v1 requirements, including SemVer and `paperctl` as the entry point |
| §7 | Workspace layout and code style |
| §8 | Package format |
| §12 | Manifest contents and typed errors |
| §15 | Desktop rendering experiment |
| §16 | Chess rules library — a WWW-6 decision, explicitly not taken here |
| §18 | Delivery order |

## Two rules that decide when anything is finished

Quoted here because they govern how every claim in this repository should be
read:

- A running process is not proof the screen is usable.
- Passing mocks or local tests is not device qualification.

Everything Stage 1 produces runs on a Mac. See `assumptions.md` for what that
leaves unestablished.
