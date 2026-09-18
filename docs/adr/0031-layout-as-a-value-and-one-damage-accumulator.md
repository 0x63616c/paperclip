# ADR-0031 — Layout as a value, and one damage accumulator

**Status:** accepted (WWW-51)

## Context

Five apps — Chess, Home, Sudoku, Settings, App Store — and, it turned out once
this stage went looking, a sixth (`paper-render-test-card`) each defined their
own `layout: Option<Layout>` field on the `App` struct, set only inside
`draw`. Every one of them therefore guarded `event` with `let Some(layout) =
self.layout else { return Action::None }`, and every one of them was
therefore silently dead to input until the first frame had actually been
drawn — not a crash, a tap that did nothing. `apps/chess/src/app.rs:96`,
`apps/home/src/app.rs:49`, `apps/settings/src/app.rs:162`,
`apps/app-store/src/app.rs:82`, `apps/sudoku/src/app.rs:155` and
`apps/render-test-card/src/app.rs:78` all had this shape before this stage.

The same duplication showed up in tests: `apps/chess/src/app.rs`'s
`square_center` and `apps/app-store/src/app.rs`'s `install_button` each built
a full `Canvas::new(SCREEN)` purely to call `render` and read the layout back
out of it, because layout had no existence apart from a side effect of
drawing.

Independently, `paper_sudoku::SudokuApp`, `paper_app_store::AppStoreApp` and
`paper_render_test_card::RenderTestCardApp` each carried their own
`pending`/`frame` field pair and their own copy of the same three rules for
folding a press's damage claim into what `App::damage` should answer:
`Damage::Full` absorbs, more rectangles than `MAX_DAMAGE_RECTS` collapses to
it, and a draw with nothing claimed behind it also claims everything.
`paper_settings::SettingsApp` computes its damage a different way — diffing
the state it painted last time against the state it is about to paint, in
`SettingsApp::damage_between` — which has no claims to accumulate in the
first place.

## Decision

### `layout(bounds) -> Layout` is a pure function, in every app

Each app's screen module now exposes three functions instead of one:

- `layout(bounds: Size, ..) -> Layout` — pure. No `Canvas`, no side effects.
  Computed from the viewport size and whatever screen/model state actually
  changes geometry (`ChessScreen::flipped`, the promotion picker's presence;
  `SudokuScreen`'s layout needs neither, since its three footer actions never
  change count).
- `draw(canvas: &mut Canvas, .., layout: &Layout)` — draws against a layout
  already computed. Invents no rectangle `layout` did not already decide.
- `render(canvas, ..) -> Layout` — `layout` then `draw`, for the one caller
  that wants both (`paperctl`'s screens, and every test that predates the
  split). This is the function every external caller already used, so its
  signature did not change.

`App::event`'s `Event::Pointer` arm now calls `crate::screen::layout(context
.viewport(), ..)` directly instead of reading a cached field, so the `Option`
— and the field — disappear. A tap is hit-testable the instant the app
exists, before the first `draw` ever runs.

This needed two small additions to `paper_sdk::chrome`, because the existing
`draw_status_bar`, `draw_footer_actions` and `draw_section_heading` compute
their geometry *and* draw in the same function, with no way to get the first
half without a `Canvas`:

- `chrome::status_bar_content_area(bounds: Rect) -> Rect`
- `chrome::footer_action_rects(bounds: Rect, count: usize) -> Vec<Rect>`
- `chrome::section_heading_content_top(at: Point) -> f32`

The three existing `draw_*` functions now call these internally rather than
duplicating the arithmetic, so there is exactly one copy of each formula.

Where a screen composes sub-layouts from other modules (Settings' `nav`,
`pages`, `confirm`; the render test card's `nav`, `ghosting`, `damage`
sections), each of those gained the same `layout`/`draw` split, for the same
reason: a top-level `layout()` cannot be pure if anything it calls still
needs a canvas.

### One `DamageAccumulator`, for the apps whose damage model is claims

`paper_sdk::DamageAccumulator` (`platform/sdk/src/damage.rs`) is the
`pending`/`frame` field pair and the three absorption rules, once:
`claim(Damage)` folds a claim in, `take_frame()` resolves what was claimed
into this frame's answer and starts a fresh, empty claim for the next one.
Sudoku and the render test card now hold one of these instead of their own
copy of the same logic; their own claim-shaped code
(`SudokuScreen::press` returning a `Press{ damage }`, and the equivalent in
the render test card) is unchanged, only what receives the claim is shared.

App Store keeps its own `pending_damage: Damage` field, and Settings keeps
`damage_between`. Neither was moved onto `DamageAccumulator`, and that is a
decision, not an oversight:

- Settings' damage is derived by comparing two states
  (`Painted { page, confirming }`) at draw time, not by accumulating claims
  between draws. It has nothing to accumulate, and it deliberately answers
  "no regions" for "nothing changed" — a case `DamageAccumulator::take_frame`
  turns into `Damage::Full` on purpose (a draw nobody claimed anything for
  cannot safely claim less than everything). Forcing Settings onto the
  accumulator would have made `a_frame_that_changed_nothing_claims_nothing`
  (`apps/settings/src/app.rs`) false.
- App Store's `pending_damage` is overwritten at each state-changing call
  site, not accumulated — each site's assignment already describes
  everything true as of that call. Moving it onto the accumulator's
  claim/union semantics is very likely a small correctness improvement
  (two claims between two draws currently make the second overwrite the
  first, which `DamageAccumulator::claim`'s union would not), but that is a
  behaviour change this restructure does not make: the task was to remove the
  `layout: Option<Layout>` bug and consolidate what could be consolidated
  without changing what an existing app does, not to also fix an
  unrelated, unobserved, theoretical under-claim. It is recorded here as the
  concrete next step if App Store's damage is revisited.

So "one damage implementation" is exactly true for the two apps that share a
model (Sudoku, the render test card), and deliberately not extended to the
two whose damage answers a different question.

## What it costs

- Every screen module with sub-layouts (Settings, the render test card) now
  has more functions, not fewer — a `layout_x`/`draw_x` pair per sub-region
  instead of one `draw_x`. The reduction is in what can go wrong (a stale
  `Option`, a canvas built only to throw its pixels away), not in line count.
- `paper_sdk::chrome`'s three pure geometry functions are a second way to get
  the same numbers `draw_status_bar` et al. produce; nothing enforces that a
  future change to one updates the other, beyond the tests added alongside
  this stage that assert they agree (`platform/sdk/src/chrome.rs`'s
  `the_pure_*_matches_what_drawing_returns` tests, and each app's own
  `layout_needs_no_canvas_and_matches_what_render_draws`).
- App Store's damage model is now the only one of the three claim-shaped
  apps *not* sharing `DamageAccumulator`, which is exactly the kind of
  divergence this stage was closing elsewhere. It is bounded and named above
  rather than silently left inconsistent.

## What would make this wrong

- **If a future screen's layout cannot be computed without a canvas** —
  something that genuinely needs measured text metrics unavailable outside
  drawing, say — the `layout`/`draw` split stops being free and the honest
  answer is to say so in that screen's own module doc, not to force a
  canvas-free signature that lies about what it needs.
- **If App Store's overwrite-not-accumulate damage is ever shown to
  under-claim in practice** (two state changes between two real draws, on
  the device or in a system test), that is the measurement that turns
  "recorded as a next step" into a ticket: move `AppStoreApp` onto
  `DamageAccumulator` the same way Sudoku and the render test card were.
