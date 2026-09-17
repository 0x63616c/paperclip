# ADR-0004 — A built-in stroke font and vector chess pieces

**Status:** accepted for Stage 1 only. Text rendering is re-decided in Stage 6.

## Context

Stage 1 needs legible capitals and digits on a 1620 × 2160 canvas, and six
recognisable chess pieces. The obvious options both cost more than the stage is
worth:

- A text stack (`cosmic-text`, `fontdue` plus shaping) means choosing a font,
  vendoring a binary, and taking on its licence — before anyone has seen type
  on the actual panel and can say what weight survives an e-ink refresh.
- Unicode chess glyphs (`U+2654`–`U+265F`) need a font that has them, and hand
  back an outline set whose stroke weight cannot be adjusted for a reflective
  display.

## Decision

**Text:** a stroke font built into `paper_sdk::text` — around fifty glyphs
(A–Z, 0–9, a little punctuation) as polylines in a 6 × 10 design box, stroked
with round caps and joins. Text is upper-cased before rendering. No shaping, no
hinting, no kerning.

**Pieces:** vector silhouettes in `paper_chess::pieces`, each a small set of
polygons in a unit square, filled in the side's colour and stroked in the
opposite one.

## Consequences

- No binary blob in the repository, no font licence to track, and stroke weight
  is a parameter — which matters on e-ink, where a hairline may not survive a
  refresh.
- Text scales cleanly and stays crisp when the desktop preview is scaled down.
- Every piece carries a contrasting outline, which is what makes a dark piece
  readable on a dark square. No single mid grey can be 4.5:1 from both `INK`
  and `PAPER` at once, so square contrast alone cannot do it — this is why real
  piece sets outline their pieces, and why these do.
- Labels shrink to fit rather than clip (`chrome::fit_text`), because the font
  has no ellipsis and a half-word is worse than a small word.
- **Costs:** capitals only, no non-Latin text, no wrapping, and glyph shapes
  are hand-tuned coordinates. Adding a character means editing a table.

## What would make this wrong

Nothing, for Stage 1 — this is scoped to it. Stage 6 defines the SDK's
rendering contract (§18 item 5), and real text belongs there, decided once the
device has told us what weight and size actually read on the panel. This ADR
should be superseded then, not extended.
