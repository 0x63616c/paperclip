//! Section: a grid that claims damage one cell at a time.
//!
//! ADR-0021 claims that `paper_sudoku`'s per-cell entries reach the wire as
//! one region, not the whole board. This is the generic version of that
//! claim, isolated from Sudoku's rules: a plain grid where a tap toggles
//! exactly the cell it landed on, and nothing else, so the one-cell claim is
//! confirmed on any screen rather than only on a puzzle.

use paper_sdk::{Canvas, Point, Rect, TextStyle, palette};

pub(crate) const COLUMNS: usize = 5;
pub(crate) const ROWS: usize = 4;
const CELL_COUNT: usize = COLUMNS * ROWS;

/// Which cells are filled. Starts empty; a fresh launch owes nothing to a
/// previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DamageGridState {
    filled: [bool; CELL_COUNT],
}

impl Default for DamageGridState {
    fn default() -> Self {
        Self {
            filled: [false; CELL_COUNT],
        }
    }
}

/// Where the grid landed, so a press can hit-test against exactly what a draw
/// put on the glass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DamageGridLayout {
    grid: Rect,
    cell_width: f32,
    cell_height: f32,
}

impl DamageGridLayout {
    /// Where cell `index` was drawn.
    pub(crate) fn cell_rect(&self, index: usize) -> Rect {
        let column = (index % COLUMNS) as f32;
        let row = (index / COLUMNS) as f32;
        Rect::new(
            self.grid.x + column * self.cell_width,
            self.grid.y + row * self.cell_height,
            self.cell_width,
            self.cell_height,
        )
        .inset(4.0)
    }

    /// Which cell a point landed in, if any.
    fn cell_at(&self, at: Point) -> Option<usize> {
        if !self.grid.contains(at) {
            return None;
        }
        let column = ((at.x - self.grid.x) / self.cell_width) as usize;
        let row = ((at.y - self.grid.y) / self.cell_height) as usize;
        Some(row * COLUMNS + column).filter(|index| *index < CELL_COUNT)
    }
}

const CELL_HEIGHT: f32 = 220.0;

pub(crate) fn render(
    canvas: &mut Canvas,
    content: Rect,
    state: DamageGridState,
) -> DamageGridLayout {
    canvas.draw_text(
        "TAP A CELL - EACH TAP CLAIMS ONLY THAT CELL (ADR-0021)",
        Point::new(content.x, content.y),
        TextStyle::new(26.0, palette::INK_SOFT).with_tracking(0.06),
    );

    let grid = Rect::new(
        content.x,
        content.y + 44.0,
        content.width,
        CELL_HEIGHT * ROWS as f32,
    );
    let layout = DamageGridLayout {
        grid,
        cell_width: grid.width / COLUMNS as f32,
        cell_height: grid.height / ROWS as f32,
    };

    for index in 0..CELL_COUNT {
        let rect = layout.cell_rect(index);
        let fill = if state.filled[index] {
            palette::EMPHASIS
        } else {
            palette::PAPER
        };
        canvas.fill_rect(rect, fill);
        canvas.stroke_rect(rect, palette::HAIRLINE, 2.0);
    }

    layout
}

/// Handles a tap. `Some` carries the one cell that changed; `None` means the
/// tap missed the grid.
pub(crate) fn press(
    state: &mut DamageGridState,
    layout: &DamageGridLayout,
    at: Point,
) -> Option<Rect> {
    let index = layout.cell_at(at)?;
    state.filled[index] = !state.filled[index];
    Some(layout.cell_rect(index))
}

#[cfg(test)]
mod tests {
    use super::{CELL_COUNT, DamageGridState, press, render};
    use paper_sdk::{Canvas, Rect, SCREEN};

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("a canvas")
    }

    #[test]
    fn tapping_a_cell_toggles_only_that_cell() {
        let mut canvas = canvas();
        let content = Rect::new(40.0, 40.0, 1500.0, 1600.0);
        let mut state = DamageGridState::default();
        let layout = render(&mut canvas, content, state);
        let target = layout.cell_rect(3);

        let changed = press(&mut state, &layout, target.center());
        assert_eq!(changed, Some(target));
        assert!(state.filled[3]);
        for (index, filled) in state.filled.iter().enumerate() {
            if index != 3 {
                assert!(!filled, "cell {index} changed too");
            }
        }
    }

    #[test]
    fn a_tap_outside_the_grid_changes_nothing() {
        let mut canvas = canvas();
        let content = Rect::new(40.0, 40.0, 1500.0, 1600.0);
        let mut state = DamageGridState::default();
        let layout = render(&mut canvas, content, state);
        assert_eq!(
            press(&mut state, &layout, paper_sdk::Point::new(0.0, 0.0)),
            None
        );
        assert_eq!(state, DamageGridState::default());
    }

    #[test]
    fn every_cell_is_reachable() {
        let mut canvas = canvas();
        let content = Rect::new(40.0, 40.0, 1500.0, 1600.0);
        let state = DamageGridState::default();
        let layout = render(&mut canvas, content, state);
        for index in 0..CELL_COUNT {
            let rect = layout.cell_rect(index);
            assert_eq!(layout.cell_at(rect.center()), Some(index));
        }
    }
}
