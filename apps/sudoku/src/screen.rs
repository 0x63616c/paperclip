//! Composing the Sudoku screen, the interaction on top of it, and — the
//! reason this app exists — what each press actually changes on the glass.
//!
//! The rules are [`paper_sudoku_rules`]'s. What is here is presentation, the
//! translation from a tap to an intent, and a damage claim per press.
//!
//! ## Why the damage claim lives in [`SudokuScreen::press`]
//!
//! [`App::damage`](paper_sdk::App::damage) is asked what changed *after* a
//! frame is drawn, which is too late to work it out: by then the previous
//! state is gone. The press is the only moment both states exist, so the
//! press is what says what moved. [`Press`] carries that out alongside the
//! action, and [`SudokuApp`](crate::SudokuApp) accumulates it until the host
//! asks for a frame.
//!
//! Claiming *less* than changed leaves a stale rectangle on e-ink until
//! something else repaints it, so the rule here is: when in doubt, claim
//! [`Damage::Full`]. Entering a digit is the case worth being exact about —
//! one cell out of eighty-one, which is why nothing on this screen changes
//! per entry except the cell itself. There is deliberately no "cells
//! remaining" counter, no timer and no progress bar: each of them would turn
//! every digit into a two-region repaint, and none of them is worth that on a
//! display that takes ~100 ms to refresh a region regardless of its size.

use paper_sdk::chrome::{self, MARGIN};
use paper_sdk::{
    Action, Canvas, Damage, MAX_DAMAGE_RECTS, Point, PointerEvent, Rect, TextStyle, palette,
};
use paper_sudoku_rules::{Cell, Difficulty, Game, MoveError};

use crate::layout::{GridLayout, PadKey, PadLayout};

/// Height of the row the digit pad occupies.
const PAD_HEIGHT: f32 = 150.0;

/// Height of the band the status and notice line is drawn in.
///
/// A band rather than a measured text box: it is claimed as damage whenever
/// the line changes, and a rectangle that covers the tallest thing the line
/// can say is easier to keep honest than one that tracks the glyphs.
const STATUS_BAND: f32 = 96.0;

/// What the screen is saying about the last press, on top of the board state
/// [`Game`] owns.
///
/// All three are refusals. A Sudoku's own state cannot record them — the board
/// is exactly what it was before the refused tap — so they live here and are
/// cleared by the next press that does something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// A digit was tapped with no cell selected.
    NoSelection,
    /// A given was tapped, or aimed at.
    Given,
    /// That digit is already in `with`.
    Conflict {
        /// The cell that already holds it.
        with: Cell,
    },
}

impl Notice {
    /// The line this notice puts on the screen.
    pub fn text(self) -> String {
        match self {
            Notice::NoSelection => "TAP A CELL FIRST".to_owned(),
            Notice::Given => "THAT CELL IS A GIVEN".to_owned(),
            Notice::Conflict { with } => format!("ALREADY IN {}", with.name()),
        }
    }
}

/// What a press did.
#[derive(Debug, Clone, PartialEq)]
pub struct Press {
    /// What the app should ask the platform for.
    pub action: Action,
    /// What changed on the glass.
    pub damage: Damage,
}

impl Press {
    /// A press that changed nothing.
    fn nothing() -> Self {
        Self {
            action: Action::None,
            damage: Damage::Regions {
                regions: Vec::new(),
            },
        }
    }

    /// A press that asks for `action` without changing the screen.
    fn leaving(action: Action) -> Self {
        Self {
            action,
            damage: Damage::Regions {
                regions: Vec::new(),
            },
        }
    }

    /// A press that changed `regions` and nothing else.
    ///
    /// An empty list is [`Self::nothing`] rather than a redraw of no
    /// rectangles, and more rectangles than the protocol accepts becomes
    /// [`Damage::Full`] — over-claiming costs one larger panel update, and
    /// silently dropping the overflow costs a stale cell.
    fn regions(regions: Vec<Rect>) -> Self {
        if regions.is_empty() {
            return Self::nothing();
        }
        if regions.len() > MAX_DAMAGE_RECTS {
            return Self::everything();
        }
        Self {
            action: Action::Redraw,
            damage: Damage::Regions { regions },
        }
    }

    /// A press that changed enough of the screen not to enumerate it.
    fn everything() -> Self {
        Self {
            action: Action::Redraw,
            damage: Damage::Full,
        }
    }
}

/// What the Sudoku screen is showing, on top of the board state [`Game`] owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SudokuScreen {
    /// The cell a digit would go into.
    pub selected: Option<Cell>,
    /// What the screen is saying about the last refused press.
    pub notice: Option<Notice>,
    /// Whether NEW PUZZLE was tapped once and awaits a confirming second tap.
    pub new_puzzle_armed: bool,
    /// The level the next NEW PUZZLE will generate at.
    ///
    /// Separate from [`Game::difficulty`], which is the level the puzzle on
    /// screen was generated at: changing this must not throw away a puzzle
    /// someone is part-way through, so it takes effect at the next new one and
    /// the footer says so ("NEXT: HARD").
    pub next_difficulty: Difficulty,
}

impl SudokuScreen {
    /// A fresh screen: nothing selected, and a new puzzle would be at
    /// `difficulty`.
    pub fn new(difficulty: Difficulty) -> Self {
        Self {
            selected: None,
            notice: None,
            new_puzzle_armed: false,
            next_difficulty: difficulty,
        }
    }

    /// Handles a tap against the layout a draw already produced, mutating
    /// `game` when the tap enters or clears a digit.
    ///
    /// Takes no [`paper_sdk::Context`]: nothing here needs one, and the tests
    /// in this module need one not to exist.
    pub fn press(
        &mut self,
        game: &mut Game,
        layout: &SudokuLayout,
        pointer: &PointerEvent,
    ) -> Press {
        if !pointer.is_tap() {
            return Press::nothing();
        }
        let at = pointer.at;

        if layout.home.contains(at) {
            return Press::leaving(Action::Home);
        }

        if layout.new_puzzle.contains(at) {
            if !self.new_puzzle_armed {
                self.new_puzzle_armed = true;
                return Press::regions(vec![layout.new_puzzle]);
            }
            game.new_puzzle(self.next_difficulty);
            self.new_puzzle_armed = false;
            self.selected = None;
            self.notice = None;
            // A new grid, a new status bar and a footer label: everything.
            return Press::everything();
        }

        // Any other press disarms the confirmation, which rewrites its label.
        let mut changed = Vec::new();
        if self.new_puzzle_armed {
            self.new_puzzle_armed = false;
            changed.push(layout.new_puzzle);
        }

        if layout.difficulty.contains(at) {
            self.next_difficulty = self.next_difficulty.next();
            changed.push(layout.difficulty);
            return Press::regions(changed);
        }

        if game.is_solved() {
            // The puzzle is finished; only the footer above does anything.
            return Press::regions(changed);
        }

        if let Some(key) = layout.pad.key_at(at) {
            return self.enter(game, layout, key, changed);
        }
        if let Some(cell) = layout.grid.cell_at(at) {
            return self.select(game, layout, cell, changed);
        }
        Press::regions(changed)
    }

    /// A tap on the grid.
    fn select(
        &mut self,
        game: &Game,
        layout: &SudokuLayout,
        cell: Cell,
        mut changed: Vec<Rect>,
    ) -> Press {
        if game.is_given(cell) {
            // The selection is left where it was: a given cannot take a digit,
            // and clearing the selection would make a mis-tap cost two taps.
            self.note(Some(Notice::Given), layout, &mut changed);
            return Press::regions(changed);
        }
        self.note(None, layout, &mut changed);
        let previous = self.selected;
        // Tapping the selected cell again deselects it, and claims that one
        // cell once rather than twice.
        if let Some(left) = previous.filter(|left| *left != cell) {
            changed.push(layout.grid.cell_rect(left));
        }
        self.selected = (previous != Some(cell)).then_some(cell);
        changed.push(layout.grid.cell_rect(cell));
        Press::regions(changed)
    }

    /// A tap on the digit pad.
    fn enter(
        &mut self,
        game: &mut Game,
        layout: &SudokuLayout,
        key: PadKey,
        mut changed: Vec<Rect>,
    ) -> Press {
        let Some(cell) = self.selected else {
            self.note(Some(Notice::NoSelection), layout, &mut changed);
            return Press::regions(changed);
        };
        let outcome = match key {
            PadKey::Digit(digit) => game.set(cell, digit),
            PadKey::Clear => game.clear(cell),
        };
        match outcome {
            Ok(()) => {
                self.note(None, layout, &mut changed);
                if game.is_solved() {
                    // SOLVED replaces the status line and the selection
                    // outline retires, so this frame is not a one-cell change.
                    self.selected = None;
                    return Press::everything();
                }
                changed.push(layout.grid.cell_rect(cell));
                Press::regions(changed)
            }
            Err(MoveError::Conflict { with }) => {
                self.note(Some(Notice::Conflict { with }), layout, &mut changed);
                Press::regions(changed)
            }
            Err(MoveError::Given) => {
                self.note(Some(Notice::Given), layout, &mut changed);
                Press::regions(changed)
            }
            // `MoveError::Solved` cannot arrive — a solved game is handled
            // before this is reached — and the enum is `#[non_exhaustive]`,
            // so a variant this build has never heard of falls here and
            // redraws rather than pretending to know what it meant.
            Err(_) => Press::everything(),
        }
    }

    /// Sets the notice, claiming the status band if it actually changed.
    fn note(&mut self, notice: Option<Notice>, layout: &SudokuLayout, changed: &mut Vec<Rect>) {
        if self.notice != notice {
            self.notice = notice;
            changed.push(layout.status);
        }
    }
}

/// Where everything on the Sudoku screen was drawn, so a caller can hit-test
/// a press against exactly what was on the glass — and claim damage against
/// exactly what it drew.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SudokuLayout {
    /// The 9x9 grid.
    pub grid: GridLayout,
    /// The digit pad.
    pub pad: PadLayout,
    /// The band the status and notice line is drawn in.
    pub status: Rect,
    /// The NEW PUZZLE footer action.
    pub new_puzzle: Rect,
    /// The NEXT: <LEVEL> footer action.
    pub difficulty: Rect,
    /// The HOME footer action.
    pub home: Rect,
}

/// Draws the Sudoku screen and returns the layout used.
pub fn render(canvas: &mut Canvas, screen: &SudokuScreen, game: &Game) -> SudokuLayout {
    let bounds = canvas.bounds();
    canvas.clear(palette::PAPER);
    let content = chrome::draw_status_bar(
        canvas,
        "SUDOKU",
        &format!(
            "{} \u{00B7} {} GIVENS",
            game.difficulty().label(),
            game.puzzle().given_count()
        ),
    );

    let heading_bottom = chrome::draw_section_heading(
        canvas,
        "PUZZLE",
        Point::new(MARGIN, content.y + 40.0),
        bounds.width - MARGIN * 2.0,
    );

    let status = Rect::new(0.0, heading_bottom + 8.0, bounds.width, STATUS_BAND);
    canvas.draw_text(
        &status_text(screen, game),
        Point::new(MARGIN, status.y + 26.0),
        TextStyle::new(52.0, palette::INK)
            .with_weight(0.12)
            .with_tracking(0.06),
    );

    let footer_top = bounds.height - chrome::FOOTER_HEIGHT;
    let pad = PadLayout::fit(Rect::new(
        MARGIN,
        footer_top - 24.0 - PAD_HEIGHT,
        bounds.width - MARGIN * 2.0,
        PAD_HEIGHT,
    ));
    let grid_top = status.bottom() + 24.0;
    let grid = GridLayout::fit(Rect::new(
        MARGIN,
        grid_top,
        bounds.width - MARGIN * 2.0,
        pad.keys()[0].1.y - 28.0 - grid_top,
    ));

    draw_grid(canvas, grid, screen, game);
    draw_pad(canvas, &pad);

    let new_puzzle_label = if screen.new_puzzle_armed {
        "CONFIRM NEW PUZZLE?"
    } else {
        "NEW PUZZLE"
    };
    let next_label = format!("NEXT: {}", screen.next_difficulty.label());
    let actions = chrome::draw_footer_actions(canvas, &[new_puzzle_label, &next_label, "HOME"]);

    SudokuLayout {
        grid,
        pad,
        status,
        new_puzzle: actions[0],
        difficulty: actions[1],
        home: actions[2],
    }
}

/// The line under the heading: what the last press did, or what to do next.
fn status_text(screen: &SudokuScreen, game: &Game) -> String {
    if game.is_solved() {
        return "SOLVED".to_owned();
    }
    match screen.notice {
        Some(notice) => notice.text(),
        None => match screen.selected {
            Some(_) => "TAP A DIGIT".to_owned(),
            None => "TAP A CELL, THEN A DIGIT".to_owned(),
        },
    }
}

fn draw_grid(canvas: &mut Canvas, layout: GridLayout, screen: &SudokuScreen, game: &Game) {
    let area = layout.grid();
    let cell_size = layout.cell_size();

    // Givens sit on a tinted cell; the player's own cells stay paper. One of
    // three cues that separate them, none of which is colour — see
    // `draw_digit` for the other two.
    for cell in Cell::all() {
        if game.is_given(cell) {
            canvas.fill_rect(layout.cell_rect(cell), palette::TILE);
        }
    }

    // Cell rules first, then the heavier block rules over them, so a block
    // boundary is unambiguous at a glance.
    for index in 1..9u8 {
        let offset = f32::from(index) * cell_size;
        canvas.hairline(
            Point::new(area.x, area.y + offset),
            area.width,
            palette::HAIRLINE,
        );
        canvas.fill_rect(
            Rect::new(area.x + offset, area.y, 2.0, area.height),
            palette::HAIRLINE,
        );
    }
    for index in [3.0, 6.0] {
        let offset = index * cell_size;
        canvas.fill_rect(
            Rect::new(area.x, area.y + offset - 2.0, area.width, 4.0),
            palette::INK,
        );
        canvas.fill_rect(
            Rect::new(area.x + offset - 2.0, area.y, 4.0, area.height),
            palette::INK,
        );
    }
    canvas.stroke_rect(area.inset(-3.0), palette::INK, 6.0);

    // Inside the cell, never over its edge: the selection outline is part of
    // what a one-cell damage claim promises to cover.
    if let Some(cell) = screen.selected {
        let rect = layout.cell_rect(cell);
        canvas.stroke_rect(rect.inset(7.0), palette::INK, 8.0);
        canvas.stroke_rect(rect.inset(16.0), palette::PAPER, 4.0);
    }

    for cell in Cell::all() {
        if let Some(digit) = game.digit_at(cell) {
            draw_digit(canvas, layout.cell_rect(cell), digit, game.is_given(cell));
        }
    }
}

/// Draws one digit, given or entered.
///
/// The two are distinguishable without colour, which §7's e-ink rules require
/// and which a greyscale panel makes non-optional: a given is larger and
/// drawn with a heavier stroke, on a tinted cell. Both are
/// [`palette::INK`] — there is no faint grey here that could disappear on
/// the glass.
fn draw_digit(canvas: &mut Canvas, rect: Rect, digit: paper_sudoku_rules::Digit, given: bool) {
    let style = if given {
        TextStyle::new(rect.height * 0.58, palette::INK)
            .with_weight(0.20)
            .centered()
    } else {
        TextStyle::new(rect.height * 0.46, palette::INK)
            .with_weight(0.10)
            .centered()
    };
    let center = rect.center();
    canvas.draw_text(
        &digit.to_string(),
        Point::new(center.x, center.y - style.size / 2.0),
        style,
    );
}

fn draw_pad(canvas: &mut Canvas, pad: &PadLayout) {
    for &(key, rect) in pad.keys() {
        canvas.fill_round_rect(rect, 14.0, palette::PAPER);
        canvas.stroke_round_rect(rect, 14.0, palette::INK, 4.0);
        let label = key.label();
        let style = chrome::fit_text(
            &label,
            TextStyle::new(rect.height * 0.44, palette::INK)
                .with_weight(0.13)
                .with_tracking(0.08)
                .centered(),
            rect.width - 40.0,
            20.0,
        );
        let center = rect.center();
        canvas.draw_text(
            &label,
            Point::new(center.x, center.y - style.size / 2.0),
            style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Notice, SudokuLayout, SudokuScreen, render};
    use crate::layout::PadKey;
    use paper_sdk::chrome::MIN_TOUCH_TARGET;
    use paper_sdk::{
        Action, Canvas, ContactId, Damage, Point, Pointer, PointerEvent, PointerPhase, Rect, SCREEN,
    };
    use paper_sudoku_rules::{Cell, Difficulty, Digit, Game};

    fn screen_canvas() -> Canvas {
        Canvas::new(SCREEN).expect("a screen-sized canvas")
    }

    fn tap(at: Point) -> PointerEvent {
        PointerEvent::new(at, PointerPhase::Up, Pointer::Touch, ContactId::FIRST)
    }

    /// A game, its screen, and the layout a real render produced.
    fn started() -> (Game, SudokuScreen, Canvas, SudokuLayout) {
        let game = Game::start(Difficulty::Easy, 4_242);
        let screen = SudokuScreen::new(game.difficulty());
        let mut canvas = screen_canvas();
        let layout = render(&mut canvas, &screen, &game);
        (game, screen, canvas, layout)
    }

    /// An empty cell, and a digit that is legal in it.
    fn playable(game: &Game) -> (Cell, Digit) {
        Cell::all()
            .filter(|cell| game.digit_at(*cell).is_none())
            .find_map(|cell| {
                Digit::ALL
                    .into_iter()
                    .find(|digit| game.board().accepts(cell, *digit))
                    .map(|digit| (cell, digit))
            })
            .expect("some empty cell takes some digit")
    }

    fn regions(damage: &Damage) -> Vec<Rect> {
        damage
            .regions()
            .expect("the press claimed the whole screen")
            .to_vec()
    }

    #[test]
    fn the_screen_fits_between_its_chrome_and_is_big_enough_to_touch() {
        let (_, _, _, layout) = started();
        let grid = layout.grid.grid();
        assert!(grid.x >= 0.0 && grid.right() <= SCREEN.width as f32);
        assert!(grid.y >= paper_sdk::chrome::STATUS_BAR_HEIGHT);
        assert!(layout.grid.cell_size() >= MIN_TOUCH_TARGET);

        let pad_top = layout.pad.keys()[0].1;
        assert!(grid.bottom() <= pad_top.y, "the grid runs into the pad");
        assert!(
            pad_top.bottom() <= SCREEN.height as f32 - paper_sdk::chrome::FOOTER_HEIGHT,
            "the pad runs into the action row"
        );
        assert!(layout.status.bottom() <= grid.y);
        for rect in [layout.new_puzzle, layout.difficulty, layout.home] {
            assert!(rect.shortest_side() >= MIN_TOUCH_TARGET);
        }
    }

    #[test]
    fn the_screen_actually_draws_something_without_going_solid() {
        let (_, _, canvas, _) = started();
        let coverage = canvas.ink_coverage();
        assert!(coverage > 0.10, "coverage was {coverage}");
        assert!(coverage < 0.95, "coverage was {coverage}");
    }

    #[test]
    fn tapping_a_cell_then_a_digit_enters_it() {
        let (mut game, mut screen, mut canvas, layout) = started();
        let (cell, digit) = playable(&game);

        screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(cell).center()),
        );
        assert_eq!(screen.selected, Some(cell));

        let layout = render(&mut canvas, &screen, &game);
        let key = layout
            .pad
            .key_rect(PadKey::Digit(digit))
            .expect("the pad has that digit");
        let press = screen.press(&mut game, &layout, &tap(key.center()));

        assert_eq!(game.digit_at(cell), Some(digit));
        assert_eq!(press.action, Action::Redraw);
        assert_eq!(
            regions(&press.damage),
            vec![layout.grid.cell_rect(cell)],
            "entering a digit must claim exactly the cell it changed"
        );
    }

    #[test]
    fn entering_a_digit_only_changes_pixels_inside_the_cell_it_claimed() {
        // The claim the module comment makes, checked against the pixels: a
        // digit entry may not alter anything outside the rectangle it says it
        // altered. This is the failure that leaves a stale region on e-ink,
        // and no amount of reasoning about the render substitutes for looking.
        let (mut game, mut screen, mut before, layout) = started();
        let (cell, digit) = playable(&game);
        screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(cell).center()),
        );
        let layout = render(&mut before, &screen, &game);

        let key = layout
            .pad
            .key_rect(PadKey::Digit(digit))
            .expect("the pad has that digit");
        let press = screen.press(&mut game, &layout, &tap(key.center()));
        let claimed = regions(&press.damage);

        let mut after = screen_canvas();
        render(&mut after, &screen, &game);

        let mut outside = 0u32;
        let mut inside = 0u32;
        for y in 0..SCREEN.height {
            for x in 0..SCREEN.width {
                if before.pixel(x, y) == after.pixel(x, y) {
                    continue;
                }
                let point = Point::new(x as f32 + 0.5, y as f32 + 0.5);
                if claimed.iter().any(|rect| rect.contains(point)) {
                    inside += 1;
                } else {
                    outside += 1;
                }
            }
        }
        assert_eq!(outside, 0, "{outside} pixels changed outside the claim");
        assert!(inside > 0, "the frame did not change at all");
    }

    #[test]
    fn moving_the_selection_claims_both_cells_and_nothing_else() {
        let (mut game, mut screen, _, layout) = started();
        let empty: Vec<Cell> = Cell::all()
            .filter(|cell| !game.is_given(*cell))
            .take(2)
            .collect();
        let (first, second) = (empty[0], empty[1]);

        let press = screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(first).center()),
        );
        assert_eq!(regions(&press.damage), vec![layout.grid.cell_rect(first)]);

        let press = screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(second).center()),
        );
        assert_eq!(screen.selected, Some(second));
        assert_eq!(
            regions(&press.damage),
            vec![layout.grid.cell_rect(first), layout.grid.cell_rect(second)],
            "the outline left one cell and arrived at another"
        );
    }

    #[test]
    fn tapping_the_selected_cell_again_clears_the_selection() {
        let (mut game, mut screen, _, layout) = started();
        let cell = Cell::all()
            .find(|cell| !game.is_given(*cell))
            .expect("a puzzle has empty cells");
        let at = tap(layout.grid.cell_rect(cell).center());

        screen.press(&mut game, &layout, &at);
        assert_eq!(screen.selected, Some(cell));
        let press = screen.press(&mut game, &layout, &at);
        assert_eq!(screen.selected, None);
        assert_eq!(regions(&press.damage), vec![layout.grid.cell_rect(cell)]);
    }

    #[test]
    fn a_given_refuses_the_tap_and_says_so_without_losing_the_selection() {
        let (mut game, mut screen, _, layout) = started();
        let (cell, _) = playable(&game);
        let given = Cell::all()
            .find(|cell| game.is_given(*cell))
            .expect("a puzzle has givens");

        screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(cell).center()),
        );
        let press = screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(given).center()),
        );

        assert_eq!(screen.notice, Some(Notice::Given));
        assert_eq!(screen.selected, Some(cell), "the selection was lost");
        assert_eq!(regions(&press.damage), vec![layout.status]);
    }

    #[test]
    fn an_invalid_placement_is_refused_and_names_the_cell_in_the_way() {
        let (mut game, mut screen, mut canvas, layout) = started();
        // A cell and a digit its row, column or block already holds.
        let (cell, digit, with) = Cell::all()
            .filter(|cell| game.digit_at(*cell).is_none())
            .find_map(|cell| {
                Digit::ALL.into_iter().find_map(|digit| {
                    game.board()
                        .conflict(cell, digit)
                        .map(|with| (cell, digit, with))
                })
            })
            .expect("an empty cell has some digit ruled out");

        screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(cell).center()),
        );
        let layout = render(&mut canvas, &screen, &game);
        let key = layout
            .pad
            .key_rect(PadKey::Digit(digit))
            .expect("the pad has that digit");
        let press = screen.press(&mut game, &layout, &tap(key.center()));

        assert_eq!(game.digit_at(cell), None, "a refused digit was entered");
        assert_eq!(screen.notice, Some(Notice::Conflict { with }));
        assert_eq!(regions(&press.damage), vec![layout.status]);
        assert!(super::status_text(&screen, &game).contains(&with.name()));
    }

    #[test]
    fn a_digit_with_nothing_selected_asks_for_a_cell() {
        let (mut game, mut screen, _, layout) = started();
        let key = layout
            .pad
            .key_rect(PadKey::Digit(Digit::ALL[0]))
            .expect("the pad has a 1");
        let press = screen.press(&mut game, &layout, &tap(key.center()));

        assert_eq!(screen.notice, Some(Notice::NoSelection));
        assert_eq!(regions(&press.damage), vec![layout.status]);
        assert_eq!(game.entered(), 0);
    }

    #[test]
    fn the_clear_key_empties_the_selected_cell_and_claims_only_it() {
        let (mut game, mut screen, mut canvas, layout) = started();
        let (cell, digit) = playable(&game);
        game.set(cell, digit).expect("a legal entry");
        screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(cell).center()),
        );

        let layout = render(&mut canvas, &screen, &game);
        let key = layout
            .pad
            .key_rect(PadKey::Clear)
            .expect("the pad has a clear key");
        let press = screen.press(&mut game, &layout, &tap(key.center()));

        assert_eq!(game.digit_at(cell), None);
        assert_eq!(regions(&press.damage), vec![layout.grid.cell_rect(cell)]);
    }

    #[test]
    fn new_puzzle_needs_two_taps_and_another_action_disarms_it() {
        let (mut game, mut screen, _, layout) = started();
        let first = *game.puzzle();

        let press = screen.press(&mut game, &layout, &tap(layout.new_puzzle.center()));
        assert!(screen.new_puzzle_armed);
        assert_eq!(regions(&press.damage), vec![layout.new_puzzle]);
        assert_eq!(game.puzzle(), &first, "the puzzle changed on one tap");

        let press = screen.press(&mut game, &layout, &tap(layout.difficulty.center()));
        assert!(!screen.new_puzzle_armed, "another action did not disarm it");
        assert_eq!(
            regions(&press.damage),
            vec![layout.new_puzzle, layout.difficulty]
        );
        assert_eq!(screen.next_difficulty, Difficulty::Easy.next());

        screen.press(&mut game, &layout, &tap(layout.new_puzzle.center()));
        let press = screen.press(&mut game, &layout, &tap(layout.new_puzzle.center()));
        assert_ne!(game.puzzle(), &first, "the second tap did nothing");
        assert_eq!(game.difficulty(), Difficulty::Easy.next());
        assert_eq!(
            press.damage,
            Damage::Full,
            "a new grid is not a per-cell change"
        );
    }

    #[test]
    fn home_is_always_reachable() {
        let (mut game, mut screen, _, layout) = started();
        let press = screen.press(&mut game, &layout, &tap(layout.home.center()));
        assert_eq!(press.action, Action::Home);
    }

    #[test]
    fn a_hover_is_never_treated_as_a_tap() {
        let (mut game, mut screen, _, layout) = started();
        let cell = Cell::all()
            .find(|cell| !game.is_given(*cell))
            .expect("a puzzle has empty cells");
        let hover = PointerEvent::new(
            layout.grid.cell_rect(cell).center(),
            PointerPhase::Hover,
            Pointer::Pen,
            ContactId::FIRST,
        );
        let press = screen.press(&mut game, &layout, &hover);
        assert_eq!(press.action, Action::None);
        assert_eq!(screen.selected, None);
        assert!(regions(&press.damage).is_empty());
    }

    #[test]
    fn a_press_on_nothing_changes_nothing() {
        let (mut game, mut screen, _, layout) = started();
        let between = Point::new(
            layout.grid.grid().center().x,
            layout.grid.grid().bottom() + 6.0,
        );
        let press = screen.press(&mut game, &layout, &tap(between));
        assert_eq!(press.action, Action::None);
        assert!(regions(&press.damage).is_empty());
    }

    #[test]
    fn a_solved_puzzle_says_so_and_only_answers_footer_taps() {
        let mut game = Game::start(Difficulty::Easy, 8_080);
        let mut screen = SudokuScreen::new(game.difficulty());
        let solution = *game.puzzle().solution();
        let mut canvas = screen_canvas();
        let layout = render(&mut canvas, &screen, &game);

        // Everything but the last cell, through the rules core; the last one
        // through the screen, so the frame that finishes the puzzle is the one
        // a tap produced.
        let last = Cell::all()
            .filter(|cell| !game.is_given(*cell))
            .last()
            .expect("a puzzle has empty cells");
        for cell in Cell::all() {
            if !game.is_given(cell) && cell != last {
                let digit = solution.get(cell).expect("a solution is complete");
                game.set(cell, digit).expect("the solution is legal");
            }
        }
        screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(last).center()),
        );
        let layout = render(&mut canvas, &screen, &game);
        let digit = solution.get(last).expect("a solution is complete");
        let key = layout
            .pad
            .key_rect(PadKey::Digit(digit))
            .expect("the pad has that digit");
        let press = screen.press(&mut game, &layout, &tap(key.center()));

        assert!(game.is_solved());
        assert_eq!(
            press.damage,
            Damage::Full,
            "SOLVED is a whole-screen change"
        );
        assert_eq!(super::status_text(&screen, &game), "SOLVED");

        // Nothing on the board answers any more, but Home still does.
        let layout = render(&mut canvas, &screen, &game);
        let quiet = screen.press(
            &mut game,
            &layout,
            &tap(layout.grid.cell_rect(last).center()),
        );
        assert_eq!(quiet.action, Action::None);
        assert_eq!(
            screen
                .press(&mut game, &layout, &tap(layout.home.center()))
                .action,
            Action::Home
        );
    }

    #[test]
    fn givens_and_entries_are_drawn_differently_enough_to_tell_apart() {
        // Not a colour test: both are INK. What must differ is the shape —
        // size and stroke weight — because on a greyscale panel that is the
        // only cue left.
        let mut game = Game::start(Difficulty::Easy, 31);
        let (cell, digit) = playable(&game);
        let given = Cell::all()
            .find(|cell| game.is_given(*cell))
            .expect("a puzzle has givens");
        game.set(cell, digit).expect("a legal entry");

        let screen = SudokuScreen::new(game.difficulty());
        let mut canvas = screen_canvas();
        let layout = render(&mut canvas, &screen, &game);

        let ink = |rect: Rect| {
            let mut count = 0u32;
            for y in rect.y as u32..rect.bottom() as u32 {
                for x in rect.x as u32..rect.right() as u32 {
                    if canvas.pixel(x, y) == Some(paper_sdk::palette::INK) {
                        count += 1;
                    }
                }
            }
            count
        };
        let given_ink = ink(layout.grid.cell_rect(given).inset(8.0));
        let entered_ink = ink(layout.grid.cell_rect(cell).inset(8.0));
        assert!(entered_ink > 0, "the entered digit did not draw");
        assert!(
            given_ink > entered_ink * 3 / 2,
            "a given ({given_ink} px) is not visibly heavier than an entry ({entered_ink} px)"
        );
    }
}
