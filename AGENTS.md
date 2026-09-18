# Paperclip

## Agent skills

### Issue tracker

Issues live in Multica — project `10dfb8b0`, keys `WWW-<n>`, driven by the
`multica` CLI bundled inside Multica.app. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical roles, each label string equal to its name. See
`docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` and `docs/adr/` at the repo root. See
`docs/agents/domain.md`.

## Git workflow

Land work by committing to `main` and pushing it. **Do not open pull
requests** — this repository does not review through them, and a PR left open
is work that looks delivered and is not.

Branch only when something genuinely needs isolating, and fast-forward it back
into `main` yourself rather than leaving it for someone to merge.
