//! Board coordinates and where each square lands on the canvas.

use paper_sdk::{Point, Rect};

/// A file, `a` through `h`, stored as `0..8` from the queenside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct File(u8);

/// A rank, `1` through `8`, stored as `0..8` from White's back rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rank(u8);

impl File {
    /// Builds a file from `0..8`, or `None` for anything else.
    pub const fn new(index: u8) -> Option<Self> {
        if index < 8 { Some(Self(index)) } else { None }
    }

    /// The `0..8` index.
    pub const fn index(self) -> u8 {
        self.0
    }

    /// The algebraic letter.
    pub const fn letter(self) -> char {
        (b'a' + self.0) as char
    }
}

impl Rank {
    /// Builds a rank from `0..8`, or `None` for anything else.
    pub const fn new(index: u8) -> Option<Self> {
        if index < 8 { Some(Self(index)) } else { None }
    }

    /// The `0..8` index, counting up from White's back rank.
    pub const fn index(self) -> u8 {
        self.0
    }

    /// The algebraic number, `1` through `8`.
    pub const fn number(self) -> u8 {
        self.0 + 1
    }
}

/// One of the sixty-four squares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Square {
    /// Which file.
    pub file: File,
    /// Which rank.
    pub rank: Rank,
}

impl Square {
    /// Builds a square from `0..8` indices, or `None` if either is off the board.
    pub fn new(file: u8, rank: u8) -> Option<Self> {
        Some(Self {
            file: File::new(file)?,
            rank: Rank::new(rank)?,
        })
    }

    /// Whether this is a light square. `a1` is dark, which is the check every
    /// board-drawing bug fails.
    pub const fn is_light(self) -> bool {
        (self.file.0 + self.rank.0) % 2 == 1
    }

    /// Algebraic notation, such as `e4`.
    pub fn name(self) -> String {
        format!("{}{}", self.file.letter(), self.rank.number())
    }

    /// Every square, `a1` first, in file-major order.
    pub fn all() -> impl Iterator<Item = Square> {
        (0..8).flat_map(|rank| (0..8).filter_map(move |file| Square::new(file, rank)))
    }
}

/// Where the board sits on the canvas, and which way round it is.
///
/// Built by fitting the largest whole-pixel board into an available area, so
/// squares are all exactly the same size and no rounding seam appears between
/// them at full resolution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoardLayout {
    board: Rect,
    square: f32,
    flipped: bool,
}

impl BoardLayout {
    /// Fits a board into `area`, centred horizontally and aligned to its top.
    pub fn fit(area: Rect, flipped: bool) -> Self {
        let side = (area.width.min(area.height) / 8.0).floor().max(1.0);
        let board_side = side * 8.0;
        let board = Rect::new(
            area.x + (area.width - board_side) / 2.0,
            area.y,
            board_side,
            board_side,
        );
        Self {
            board,
            square: side,
            flipped,
        }
    }

    /// The board's outer rectangle.
    pub fn board(self) -> Rect {
        self.board
    }

    /// The side length of one square.
    pub fn square_size(self) -> f32 {
        self.square
    }

    /// Whether Black is at the bottom.
    pub fn is_flipped(self) -> bool {
        self.flipped
    }

    /// Where `square` is drawn.
    pub fn square_rect(self, square: Square) -> Rect {
        let (column, row) = self.grid_position(square);
        Rect::new(
            self.board.x + column as f32 * self.square,
            self.board.y + row as f32 * self.square,
            self.square,
            self.square,
        )
    }

    /// Which square a canvas point is on, or `None` if it is off the board.
    pub fn square_at(self, point: Point) -> Option<Square> {
        if !self.board.contains(point) {
            return None;
        }
        let column = ((point.x - self.board.x) / self.square).floor() as u8;
        let row = ((point.y - self.board.y) / self.square).floor() as u8;
        let (file, rank) = if self.flipped {
            (7u8.checked_sub(column)?, row)
        } else {
            (column, 7u8.checked_sub(row)?)
        };
        Square::new(file, rank)
    }

    /// Column and row on screen, top-left origin.
    fn grid_position(self, square: Square) -> (u8, u8) {
        if self.flipped {
            (7 - square.file.index(), square.rank.index())
        } else {
            (square.file.index(), 7 - square.rank.index())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BoardLayout, Square};
    use paper_sdk::{Point, Rect};

    fn layout(flipped: bool) -> BoardLayout {
        BoardLayout::fit(Rect::new(50.0, 400.0, 1520.0, 1520.0), flipped)
    }

    #[test]
    fn the_dark_corners_are_a1_and_h8() {
        // The check every board-drawing bug fails: a1 and h8 dark, h1 and a8
        // light. Get this backwards and the whole board is mirrored.
        assert!(!Square::new(0, 0).unwrap().is_light(), "a1");
        assert!(!Square::new(7, 7).unwrap().is_light(), "h8");
        assert!(Square::new(7, 0).unwrap().is_light(), "h1");
        assert!(Square::new(0, 7).unwrap().is_light(), "a8");
        assert!(Square::new(1, 0).unwrap().is_light(), "b1");
    }

    #[test]
    fn squares_are_named_algebraically() {
        assert_eq!(Square::new(0, 0).unwrap().name(), "a1");
        assert_eq!(Square::new(4, 3).unwrap().name(), "e4");
        assert_eq!(Square::new(7, 7).unwrap().name(), "h8");
    }

    #[test]
    fn there_are_sixty_four_distinct_squares() {
        let all: Vec<_> = Square::all().collect();
        assert_eq!(all.len(), 64);
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 64);
    }

    #[test]
    fn squares_are_whole_pixels_and_tile_the_board_exactly() {
        let layout = layout(false);
        assert_eq!(layout.square_size(), layout.square_size().floor());
        assert!(
            (layout.board().width - layout.square_size() * 8.0).abs() < 1e-6,
            "the board is exactly eight squares wide"
        );
    }

    #[test]
    fn white_starts_at_the_bottom() {
        let layout = layout(false);
        let a1 = layout.square_rect(Square::new(0, 0).unwrap());
        let a8 = layout.square_rect(Square::new(0, 7).unwrap());
        assert!(a1.y > a8.y);
        assert!((a1.x - layout.board().x).abs() < 1e-6);
    }

    #[test]
    fn flipping_puts_black_at_the_bottom_without_moving_the_board() {
        let normal = layout(false);
        let flipped = layout(true);
        assert_eq!(normal.board(), flipped.board());

        let a1_normal = normal.square_rect(Square::new(0, 0).unwrap());
        let a1_flipped = flipped.square_rect(Square::new(0, 0).unwrap());
        assert!(a1_flipped.y < a1_normal.y);
        assert!(a1_flipped.x > a1_normal.x);
    }

    #[test]
    fn every_square_round_trips_through_its_own_centre() {
        for flipped in [false, true] {
            let layout = layout(flipped);
            for square in Square::all() {
                let center = layout.square_rect(square).center();
                assert_eq!(
                    layout.square_at(center),
                    Some(square),
                    "{} at {center:?} (flipped: {flipped})",
                    square.name()
                );
            }
        }
    }

    #[test]
    fn the_corners_of_a_square_belong_to_it() {
        let layout = layout(false);
        let square = Square::new(4, 3).unwrap();
        let rect = layout.square_rect(square);
        assert_eq!(layout.square_at(Point::new(rect.x, rect.y)), Some(square));
        assert_eq!(
            layout.square_at(Point::new(rect.right() - 0.5, rect.bottom() - 0.5)),
            Some(square)
        );
        // The far corner belongs to the next square along, not to this one.
        assert_ne!(
            layout.square_at(Point::new(rect.right(), rect.bottom())),
            Some(square)
        );
    }

    #[test]
    fn presses_off_the_board_hit_nothing() {
        let layout = layout(false);
        let board = layout.board();
        assert_eq!(
            layout.square_at(Point::new(board.x - 1.0, board.y + 10.0)),
            None
        );
        assert_eq!(
            layout.square_at(Point::new(board.right(), board.y + 10.0)),
            None
        );
        assert_eq!(
            layout.square_at(Point::new(board.x + 10.0, board.bottom())),
            None
        );
        assert_eq!(layout.square_at(Point::new(0.0, 0.0)), None);
    }

    #[test]
    fn squares_are_comfortably_bigger_than_a_fingertip() {
        assert!(layout(false).square_size() >= paper_sdk::chrome::MIN_TOUCH_TARGET);
    }
}
