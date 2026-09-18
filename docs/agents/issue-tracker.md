# Issue tracker: Multica

Issues and specs for this repo live in Multica, not GitHub. GitHub holds the
code only; every landed commit names a `WWW-<n>` ticket.

## Reaching the CLI

The `multica` binary ships inside the desktop app and is not on `PATH`:

    /Applications/Multica.app/Contents/Resources/app.asar.unpacked/resources/bin/multica

Calum aliases it as `mc` in `~/.aliases`, but that alias does not survive into
an agent shell — use the full path.

Paperclip project id: `10dfb8b0`. Issue keys are `WWW-<n>`.

## Conventions

- **Create**: `multica issue create --project 10dfb8b0 --title "..." --description-stdin`
  (pipe the body in; `--description` decodes escapes, `--description-stdin`
  preserves multi-line text verbatim). `--parent <id>` makes it a sub-issue.
- **Read**: `multica issue get WWW-34 --output json` — the verb is `get`, not
  `view`. Comments are separate: `multica issue comment list WWW-34`.
- **List**: `multica issue list --project 10dfb8b0 --limit 100 --output json`.
  Add `--fields id,identifier,title,status,assignee_id,labels` to keep agent
  context small; `--status` and `--assignee` filter server-side. The server caps
  a page at 100 — page with `--offset`.
- **Search**: `multica issue search "<text>"` across title, description and comments.
- **Comment**: `multica issue comment add WWW-34 --content-stdin`. A run triggered
  by a comment must reply under it with `--parent <comment-id>`.
- **Labels**: `multica issue label add <issue-id> <label-id>` / `label remove`.
  Note it takes a **label id**, not a name — resolve with `multica label list`.
- **Status**: `multica issue status WWW-34 <key>`. Keys are `backlog`, `todo`,
  `in_progress`, `in_review`, `done`, `blocked`, `cancelled`.
- **Assign**: `multica issue assign WWW-34 --to "Software Engineer"`.

## Status moves start agent runs

`multica issue status` and `multica issue assign` **start an agent run on the
assignee** unless you pass `--no-start`. A status move is therefore a hand-off,
not bookkeeping. Pass `--no-start` whenever you only mean to record state.

Check whether a run actually started with `multica issue runs WWW-34`;
`multica issue run-messages <execution-id>` shows what it did.

## Implementation goes to an agent, not to this session

Work on Paperclip is executed by Multica agents (Evee, Software Engineer,
Reviewer), and Calum validates on `main` and records the result as a ticket
comment. When a task turns into implementation, find or file the ticket and
hand it over rather than editing the tree here. Diagnosis, verification and
cross-checking the tracker against the tree are in scope.

## Ticket status lags the tree — in both directions

Do not trust a status without checking `git log`. Tickets have sat in `backlog`
with their code already on `main`, and in `blocked` when only device validation
was outstanding.

## When a skill says "publish to the issue tracker"

`multica issue create --project 10dfb8b0 …`.

## When a skill says "fetch the relevant ticket"

`multica issue get <KEY> --output json`, plus `multica issue comment list <KEY>`
— validation lives in the comments, not the description.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a parent issue; **children** are sub-issues.

- **Map**: an issue holding the Notes / Decisions-so-far / Fog body, labelled
  `wayfinder:map`.
- **Child ticket**: `multica issue create --parent <map-id>`, labelled
  `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). List them with
  `multica issue children <map-id>`.
- **Blocking**: `--stage <n>` groups children into ordered barrier groups under
  the parent — the parent is woken only when every sub-issue in a stage
  finishes. Use stage ordinals for dependency order; where that is too coarse,
  put a `Blocked by: WWW-<n>` line at the top of the child body.
- **Frontier**: the map's open children in the lowest unfinished stage, dropping
  any that are assigned or have an unclosed `Blocked by`.
- **Claim**: `multica issue assign <n> --to "<agent>"` (add `--no-start` to
  claim without launching a run).
- **Resolve**: `multica issue comment add <n> --content-stdin` with the answer,
  `multica issue status <n> done --no-start`, then append a context pointer to
  the map's Decisions-so-far.
