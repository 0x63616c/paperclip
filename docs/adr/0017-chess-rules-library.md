# ADR-0017 — Chess rules library, and where it lives

**Status:** accepted (Stage 7, WWW-6)

## Context

§16 is explicit: use an established rules library after review, rather than
hand-writing a legality engine, because an incomplete one is the specific
failure mode the section names. `apps/chess` (Stage 6, `paper-chess`) already
draws a board and the starting position and says so directly in its own doc
comment — it does not know a legal move from an illegal one, and must not
learn one before this decision is made.

WWW-9 (the project-coordination issue) lists "which established chess rules
library to adopt (§16)" under decisions that genuinely require Calum. That
list predates Calum's 2026-09-16 delegation of technical design decisions to
Evee; the WWW-6 issue comment that scoped this pass explicitly assigns the
choice, the review, and the ADR to this pass, superseding the older list for
this one item. Recorded here so the provenance is not lost: the decision was
made by Evee's instruction on WWW-6, not unilaterally by whoever wrote the
code.

## Options considered

| Crate | License | Notes |
|---|---|---|
| [`shakmaty`](https://github.com/niklasf/shakmaty) | **GPL-3.0-or-later** | The library lichess.org's engine tooling is built on; very well exercised, supports variants, PGN/FEN/UCI, `Chess` position with full rule coverage. Ruled out on licence alone: the workspace commits every crate here to MIT (`docs/adr/0001`), and a GPL dependency would put a copyleft obligation on anything Stage 9's packaging distributes, for a personal app that does not need it. |
| [`cozy-chess`](https://github.com/analog-hors/cozy-chess) | MIT | Fast bitboard move generation, used inside several engines. No `Board`-level convenience beyond generation; would need the same history/outcome layer this pass builds regardless. Lower download/reverse-dependency count than `chess`, i.e. fewer independent eyes on it. |
| [`chess`](https://github.com/jordanbray/chess) (jordanbray) | MIT | Mature (v3.2.0, 36 published versions, ~150k downloads), FEN/SAN/UCI parsing, magic-bitboard move generation, and `Board::status()` already distinguishes ongoing/stalemate/checkmate. Slower than `cozy-chess` and `shakmaty` in engine-scale search benchmarks, which does not matter here: this is a two-player local UI making one legality check per tap, not a search loop. |

## Decision

Adopt `chess` 3.2.0 (MIT). It is the only MIT option with a `Board` type that
already does check/checkmate/stalemate classification, which is the part of
§16 most dangerous to get wrong by hand, and its relative move-generation
speed is irrelevant at this scale.

It sits behind a small crate of our own, `paper-chess-rules`
(`apps/chess-rules`), rather than being used directly from application code:

- Our own `Square`, `Color`, `PieceKind` and `Move` types are what
  `paper-chess-rules::Game` accepts and returns. Nothing outside this crate
  names a `chess::` type. Swapping the library later — including to
  `shakmaty` if a licence exception is ever agreed, or to `cozy-chess` for
  performance no v1 device is expected to need — costs one crate, not every
  caller.
- `chess::Board` and `MoveGen` cover legality, check, checkmate and stalemate.
  Everything §16 calls out as a *draw-classification* question rather than a
  *legality* question — insufficient material, threefold repetition, the
  fifty-move rule, and their automatic fivefold/seventy-five-move cousins —
  is not in the library (it merges repetition and the fifty-move rule into
  one undifferentiated `Game::can_declare_draw`, and has no material check at
  all) and is built in `paper-chess-rules` on top of `Board`, using our own
  repetition key rather than trusting `Board::get_hash()` alone: the key is
  `(get_hash(), side_to_move, castle_rights(White), castle_rights(Black),
  en_passant)`, which is provably FIDE-correct regardless of what the
  library's internal hash happens to cover, and does not require trusting
  undocumented behaviour.
- Insufficient material is scoped to the unambiguous cases only — bare king,
  and king plus one minor piece, on both sides combined, no pawns, no
  rooks or queens on the board. Two-minor-piece positions (K+2N v K, bishop
  pairs) are left to the existing fifty-move/repetition path rather than
  guessed at: FIDE's own "dead position" rule for those is a judgment call
  the arbiter makes, not a mechanical one, and hand-writing that judgment
  would be exactly the kind of incomplete engine §16 warns against.
- The crate has no dependency on `paper-sdk` and is placed as its own
  workspace member (`apps/chess-rules`, crate name `paper-chess-rules`)
  alongside `apps/chess` rather than nested inside it, so it stays runnable
  with nothing but `cargo test` and is unaffected by whatever WWW-6's SDK pass
  does to rendering. `apps/chess` does not depend on it yet; wiring the
  screen to real game state is later work in this issue, explicitly out of
  scope for this pass per the WWW-6 comment that started it.

Persistence stores our own event log (`Move` and `ClaimDraw` events), not the
library's types, replayed through `Game` to reconstruct state on load. This
keeps the save format stable across a future library swap and gives the
"history needed for repetition and draw rules" §16 asks for directly, rather
than as a derived cache that could drift from it.

## Consequences

- One crate boundary is where a licence problem, a correctness bug, or a
  performance ceiling in the underlying library would be fixed.
- The draw-classification layer is untested by the library's own test suite;
  it is ours, and it is the thing this pass's tests are mostly about.
- `Cargo.lock` picks up `chess` and its transitive dependencies (no new
  license concerns beyond `chess` itself — verified via `cargo tree`).

## What would make this wrong

If a future stage needs chess variants, PGN import/export, or engine-strength
move generation — none of which are in v1 scope per §16 — `chess` stops being
enough and the crate boundary drawn here is exactly what makes revisiting this
cheap.
