//! Where the grid and the digit pad land on the canvas.
//!
//! Both are laid out from whatever area they are given rather than from
//! constants, so the one thing a caller cannot do is compute a cell's position
//! a second, slightly different way. Hit-testing is the same object that drew:
//! [`render`](crate::render) hands its [`SudokuLayout`](crate::SudokuLayout)
//! back for exactly that reason.

use paper_sdk::{Point, Rect};
use paper_sudoku_rules::{Cell, Digit};

/// Where the 9x9 grid sits, and how big one cell is.
///
/// Fitted to whole pixels, like [`paper_chess::BoardLayout`]: nine cells of
/// 158.2 px leave a rounding seam somewhere, and on e-ink a seam that moves
/// between frames is a visible artefact rather than a rounding detail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridLayout {
    grid: Rect,
    cell: f32,
}

impl GridLayout {
    /// Fits the largest whole-pixel grid into `area`, centred horizontally and
    /// aligned to its top.
    pub fn fit(area: Rect) -> Self {
        let cell = (area.width.min(area.height) / 9.0).floor().max(1.0);
        let side = cell * 9.0;
        let grid = Rect::new(area.x + (area.width - side) / 2.0, area.y, side, side);
        Self { grid, cell }
    }

    /// The grid's outer rectangle.
    pub fn grid(self) -> Rect {
        self.grid
    }

    /// The side length of one cell.
    pub fn cell_size(self) -> f32 {
        self.cell
    }

    /// Where `cell` is drawn.
    ///
    /// This rectangle is also the damage a change to that one cell claims, so
    /// everything drawn for a cell — its digit, its tint, its selection
    /// outline — has to stay inside it.
    pub fn cell_rect(self, cell: Cell) -> Rect {
        Rect::new(
            self.grid.x + f32::from(cell.column()) * self.cell,
            self.grid.y + f32::from(cell.row()) * self.cell,
            self.cell,
            self.cell,
        )
    }

    /// Which cell a canvas point is in, or `None` if it is off the grid.
    pub fn cell_at(self, point: Point) -> Option<Cell> {
        if !self.grid.contains(point) {
            return None;
        }
        let column = ((point.x - self.grid.x) / self.cell).floor() as u8;
        let row = ((point.y - self.grid.y) / self.cell).floor() as u8;
        Cell::new(row, column)
    }
}

/// One key on the digit pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadKey {
    /// Enter this digit in the selected cell.
    Digit(Digit),
    /// Empty the selected cell.
    Clear,
}

impl PadKey {
    /// Every key, in the order they are drawn.
    pub fn all() -> [PadKey; 10] {
        let mut keys = [PadKey::Clear; 10];
        for (index, digit) in Digit::ALL.into_iter().enumerate() {
            keys[index] = PadKey::Digit(digit);
        }
        keys
    }

    /// What the key says.
    pub fn label(self) -> String {
        match self {
            PadKey::Digit(digit) => digit.to_string(),
            PadKey::Clear => "CLR".to_owned(),
        }
    }
}

/// Where the ten pad keys landed.
///
/// A single row of ten rather than a 3x3 block with a clear key: the panel is
/// 1620 px wide, which leaves ten keys 140 px across — past
/// [`MIN_TOUCH_TARGET`](paper_sdk::chrome::MIN_TOUCH_TARGET) — and one row
/// means the digit you want is never behind a mode, a page or a scroll.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PadLayout {
    keys: [(PadKey, Rect); 10],
}

impl PadLayout {
    /// Tiles ten keys across `area`.
    pub fn fit(area: Rect) -> Self {
        const GAP: f32 = 12.0;
        let width = ((area.width - GAP * 9.0) / 10.0).floor().max(1.0);
        let mut keys = [(PadKey::Clear, Rect::new(0.0, 0.0, 0.0, 0.0)); 10];
        for (index, key) in PadKey::all().into_iter().enumerate() {
            let rect = Rect::new(
                area.x + index as f32 * (width + GAP),
                area.y,
                width,
                area.height,
            );
            keys[index] = (key, rect);
        }
        Self { keys }
    }

    /// Every key and where it was drawn.
    pub fn keys(&self) -> &[(PadKey, Rect)] {
        &self.keys
    }

    /// Which key a canvas point is on, or `None` for a gap or a miss.
    pub fn key_at(&self, point: Point) -> Option<PadKey> {
        self.keys
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|&(key, _)| key)
    }

    /// Where `key` was drawn.
    pub fn key_rect(&self, key: PadKey) -> Option<Rect> {
        self.keys
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|&(_, rect)| rect)
    }
}

#[cfg(test)]
mod tests {
    use super::{GridLayout, PadKey, PadLayout};
    use paper_sdk::chrome::MIN_TOUCH_TARGET;
    use paper_sdk::{Point, Rect};
    use paper_sudoku_rules::{Cell, Digit};

    fn grid() -> GridLayout {
        GridLayout::fit(Rect::new(56.0, 360.0, 1508.0, 1420.0))
    }

    fn pad() -> PadLayout {
        PadLayout::fit(Rect::new(56.0, 1800.0, 1508.0, 150.0))
    }

    #[test]
    fn cells_are_whole_pixels_and_tile_the_grid_exactly() {
        let layout = grid();
        assert_eq!(layout.cell_size(), layout.cell_size().floor());
        assert!((layout.grid().width - layout.cell_size() * 9.0).abs() < 1e-6);
        assert!((layout.grid().height - layout.cell_size() * 9.0).abs() < 1e-6);
        assert!(layout.cell_size() >= MIN_TOUCH_TARGET);
    }

    #[test]
    fn the_grid_reads_top_left_to_bottom_right() {
        let layout = grid();
        let first = layout.cell_rect(Cell::new(0, 0).unwrap());
        let last = layout.cell_rect(Cell::new(8, 8).unwrap());
        assert!((first.x - layout.grid().x).abs() < 1e-6);
        assert!((first.y - layout.grid().y).abs() < 1e-6);
        assert!(last.x > first.x && last.y > first.y);
        assert!((last.right() - layout.grid().right()).abs() < 1e-6);
        assert!((last.bottom() - layout.grid().bottom()).abs() < 1e-6);
    }

    #[test]
    fn every_cell_round_trips_through_its_own_centre() {
        let layout = grid();
        for cell in Cell::all() {
            let center = layout.cell_rect(cell).center();
            assert_eq!(layout.cell_at(center), Some(cell), "{}", cell.name());
        }
    }

    #[test]
    fn the_corners_of_a_cell_belong_to_it() {
        let layout = grid();
        let cell = Cell::new(4, 4).unwrap();
        let rect = layout.cell_rect(cell);
        assert_eq!(layout.cell_at(Point::new(rect.x, rect.y)), Some(cell));
        assert_eq!(
            layout.cell_at(Point::new(rect.right() - 0.5, rect.bottom() - 0.5)),
            Some(cell)
        );
        assert_ne!(
            layout.cell_at(Point::new(rect.right(), rect.bottom())),
            Some(cell)
        );
    }

    #[test]
    fn presses_off_the_grid_hit_nothing() {
        let layout = grid();
        let area = layout.grid();
        assert_eq!(
            layout.cell_at(Point::new(area.x - 1.0, area.y + 10.0)),
            None
        );
        assert_eq!(
            layout.cell_at(Point::new(area.right(), area.y + 10.0)),
            None
        );
        assert_eq!(
            layout.cell_at(Point::new(area.x + 10.0, area.bottom())),
            None
        );
        assert_eq!(layout.cell_at(Point::new(0.0, 0.0)), None);
    }

    #[test]
    fn the_pad_has_nine_digits_and_a_clear_key_in_order() {
        let layout = pad();
        let keys: Vec<_> = layout.keys().iter().map(|&(key, _)| key).collect();
        assert_eq!(keys.len(), 10);
        for (index, digit) in Digit::ALL.into_iter().enumerate() {
            assert_eq!(keys[index], PadKey::Digit(digit));
        }
        assert_eq!(keys[9], PadKey::Clear);
        assert_eq!(PadKey::Clear.label(), "CLR");
        assert_eq!(PadKey::Digit(Digit::ALL[4]).label(), "5");
    }

    #[test]
    fn pad_keys_are_big_enough_to_hit_and_do_not_overlap() {
        let layout = pad();
        let mut previous: Option<Rect> = None;
        for &(key, rect) in layout.keys() {
            assert!(
                rect.shortest_side() >= MIN_TOUCH_TARGET,
                "{key:?} is {rect:?}"
            );
            assert_eq!(layout.key_at(rect.center()), Some(key));
            assert_eq!(layout.key_rect(key), Some(rect));
            if let Some(before) = previous {
                assert!(before.right() <= rect.x, "keys overlap");
            }
            previous = Some(rect);
        }
        let last = layout.keys()[9].1;
        assert!(last.right() <= 56.0 + 1508.0, "the row overflows its area");
    }

    #[test]
    fn a_press_in_the_gap_between_keys_is_not_a_key() {
        let layout = pad();
        let first = layout.keys()[0].1;
        let gap = Point::new(first.right() + 4.0, first.center().y);
        assert_eq!(layout.key_at(gap), None);
        assert_eq!(layout.key_at(Point::new(0.0, 0.0)), None);
    }
}
