# Architecture reviews

One file per review of the workspace's shape: where a seam belongs, which
crates should exist that do not, which existing ones are shallow enough to
merge, and how the structure holds at the next order of magnitude. Each
candidate carries real `file.rs:line` citations, the code as it exists, the
proposed code beside it, the options that were weighed, and a recommendation
with its reasoning. Nothing in a review changes the Rust tree; an accepted
candidate becomes its own ticket, and any decision it produces is recorded as
an ADR in `docs/adr/`.

Reviews are dated rather than numbered because they are superseded, not
referenced: a later review replaces an earlier one's picture of the tree. ADRs
record what was decided; a review records what should be decided next. Reviews
are not registered in `book/src/SUMMARY.md` — that chapter is ADRs only.

| Date | Review | Top recommendation |
|---|---|---|
| [2026-09-18](2026-09-18-workspace-seams-and-scaling.md) | Where the seams belong, which core libs should exist, and how the workspace scales (WWW-91) | `paper-session`: a sans-I/O `Conversation` driving one app's launch-time exchange, run by the compositor on the device and by `paperctl dev` on the Mac |
