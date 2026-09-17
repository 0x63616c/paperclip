# ADR-0021 — How an app claims per-cell damage, and Sudoku as the worked example

**Status:** accepted (WWW-39)

## Context

WWW-6 made the device present what changed rather than the whole panel:
`paperctl run` takes the [`Damage`](../app-contract.md) an app publishes and
turns it into one partial waveform. Nothing then exercised it. Home and Chess
both take [`App::damage`](../app-contract.md)'s default, `Damage::Full`, and
both are right to — a chess move changes two squares, a status line, a footer
label and possibly a captured piece, and the bounding box of that is most of
the screen anyway.

So the damage path had a consumer and no producer, and "an app claims one
small region" was code nobody had written. Sudoku is the case where it pays:
entering a digit changes one cell out of eighty-one.

Second, and independently: Chess was both the only installable app and the
only app the install path was tested with. A bug in "installing an app that is
not Chess" had nowhere to show up.

## Decision

### Sudoku, as a catalog app with its own rules crate

`apps/sudoku-rules` (`paper-sudoku-rules`) holds the grid, generation,
constraint validation, solving and the save format. `apps/sudoku`
(`paper-sudoku`) draws and interprets taps. The split, and the reasons for it,
are ADR-0017's for `paper-chess-rules` — with one difference: there is no
established crate to adopt. Sudoku's rules are one function ("no digit twice in
a row, a column or a block"); what needs care is generation, because a puzzle
with two solutions is broken rather than hard. `solutions(grid, limit)` is
therefore load-bearing, and generation never removes a clue without counting
what is left.

Generation is deterministic in a seed, with SplitMix64 pinned in the crate
rather than a `rand` dependency. That is what lets the golden-frame table
freeze a Sudoku render, and what lets a saved game resume the *sequence* of
puzzles rather than restart it.

Difficulty is clue count (42 / 34 / 28 givens), not solving technique. Grading
by the hardest technique a puzzle needs means implementing those techniques;
clue count is an approximation that claims nothing it cannot back up.

### The damage claim is built by the press, not by `damage()`

`App::damage` is asked what changed *after* the frame is drawn, by which point
the previous state is gone. The only moment both states exist is the press, so
`SudokuScreen::press` returns a `Press { action, damage }` and `SudokuApp`
accumulates those claims until the host asks for a frame. The rules are:

- `Damage::Full` is absorbing. Anything unclear, anything structural (a new
  puzzle, SOLVED appearing), and anything after `Suspended`/`Resumed` claims
  everything.
- A draw with no accumulated claim behind it — a first frame, a remapped
  surface, a host that simply wants one — is `Damage::Full`. Claiming nothing
  would present nothing.
- More than `MAX_DAMAGE_RECTS` rectangles collapses to `Damage::Full` rather
  than dropping the overflow.
- Everything drawn *for* a cell stays inside that cell's rectangle, because
  that rectangle is what a one-cell claim promises to cover. The selection
  outline is inset; the digit is centred and fits.

Over-claiming costs one larger panel update. Under-claiming leaves a stale
rectangle on the glass until something else happens to repaint it, which is why
every ambiguous case above resolves to `Full`.

### A claim only pays if the presenter honours it

An app that claims one cell and a presenter that swaps the panel anyway look
identical in the app's own tests, and cost a full-screen waveform per
keystroke on the glass. So the translation from a claim to a panel rectangle
is one function, `PixelRect::covering`, next to the rounding and union rules
it is built from:

- `Damage::Full` is the whole panel.
- Regions collapse to their union, rounded outward — the engine coalesces
  overlapping updates anyway, and one rectangle costs one waveform where
  several cost several. A claim of two distant cells is therefore one swap
  over both, which is the reason selection is two cells and not two claims.
- A claim of nothing is an empty rectangle, and an empty rectangle is a swap
  that never reaches the glass.

`paperctl run` previously computed this inline, in a module that only compiles
for Linux — so the arithmetic deciding the cost of every keystroke was never
type-checked on the machine the tests run on, let alone asserted. It is now
cross-platform, and the system suite asserts the number: a Sudoku digit entry
swaps under 1% of the panel, and it is exactly the cell that changed.

### What the screen therefore does not show

No "cells remaining" counter, no timer, no progress bar. Each would change on
every digit entry and turn the one-cell claim into a two-region one. On a
display where a region refresh costs roughly the same regardless of size, that
is a real cost for a number nobody needs.

## Consequences

- `paper_sdk` re-exports `MAX_DAMAGE_RECTS`, so an app that overrides
  `damage()` can see the cap it is claiming against without depending on
  `paper_protocol` directly.
- `paperctl dev --app sudoku`, `paperctl run --app sudoku`,
  `paperctl screenshot --screen sudoku` and the preview's `u` key all reach it,
  and the golden table freezes its render.
- The claim is asserted four ways: on the `Press` (one rectangle, and it is
  the cell), on the pixels (nothing outside the claim changed between two real
  renders), on the wire (the `FrameDone` a host reads), and on the panel
  rectangle the presenting loop would hand the waveform engine.
- A second app goes through package → publish → check → install as a test
  (`tools/paperctl/tests/catalog.rs`) rather than a transcript, driving the
  real command line against the `paper.toml` Sudoku ships. That is what makes
  "installing an app that is not Chess" a path a regression can fail on.
- **Sudoku is deliberately not on either Home shelf.** Both shelves are
  hand-built lists of what this device has (WWW-37 holds them together), and
  the App Store preview shows Sudoku as `NOT INSTALLED` with an INSTALL
  action — a tile for an app the same build says is not installed would be the
  two fixtures disagreeing. It is reached by `--app sudoku` and
  `--screen sudoku` until WWW-38 makes the store install for real, at which
  point the shelf should come from the inventory rather than from either list.
  `session::launch_target` already resolves `dev.calum.sudoku`, so nothing
  else has to change when it does.

## What would make this wrong

- **If per-region presentation turns out not to help on this panel.** The
  waveform engine coalesces updates and the vendor library's cost model is not
  ours; if a 158x158 px update measures the same as a full-panel one on the
  glass, the exactness here buys nothing and the honest simplification is
  `Damage::Full` everywhere. Nothing in this ADR has been measured on the
  device — WWW-6 established the transport, not a saving. Every assertion
  above is about which pixels will be swapped, not about what the panel does
  with them; no Sudoku frame has reached the glass.
- **If the host starts deriving damage itself** (`App::damage`'s own doc
  comment reserves that right, and says centralised tracking is the version
  that stays correct when an app gets it wrong). Then this becomes an
  optimisation the platform ignores, and the app-side claim should be deleted
  rather than maintained.
- **If generation gets slow enough to block a tap.** It runs on the UI thread
  because a hard puzzle generates in single-digit milliseconds in a debug
  build. If a future difficulty level, or a slower device path, changes that,
  the answer is `Context::spawn` and a real `Completion` type.
