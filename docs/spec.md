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

## Amendments

The specification is amended by decision, not by edit — `pasted-text.txt` is
dated and stays as it was. Where a section has been amended, the replacement
text lives in the ADR that carries the decision, and a reader of that section
should go there first.

| Section | Amended by | What changed |
|---|---|---|
| §10 | [ADR-0008](adr/0008-runtime-only-units-and-no-root-filesystem-install.md) | Persistence, Independence, Reboot and Deferred are replaced. This firmware has no writable persistent unit directory, so runtime units under `/run/systemd/system` established by `paperctl` are the only mechanism, and auto-start at boot is deferred. Accepted verbatim on 2026-09-17. |
| §9 (display transport) | [ADR-0007](adr/0007-display-transport-via-vendor-waveform-engine.md) | Presentation goes through the vendor waveform engine, not raw DRM. The `405x1084 -> 1620x2160` packing is proprietary and explicitly out of scope. |
| §3, §5 | [ADR-0015](adr/0015-settings-default-app-and-host-boundary.md) | Home, App Store and Settings ship with Paperclip and are always present, forming one tested platform release with the Host (§13). Everything else, including Chess, is installed through the catalog and independently versioned. Decided by Calum, 2026-09-16. |

## Two rules that decide when anything is finished

Quoted here because they govern how every claim in this repository should be
read:

- A running process is not proof the screen is usable.
- Passing mocks or local tests is not device qualification.

Everything Stage 1 produces runs on a Mac. See `assumptions.md` for what that
leaves unestablished.
