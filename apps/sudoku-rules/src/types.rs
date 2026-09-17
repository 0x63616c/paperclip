//! A digit, a cell, and the grid the two address.

use std::fmt;

/// Every digit's bit set at once: bits `1..=9` of a candidate mask.
pub(crate) const ALL_DIGITS: u16 = 0b11_1111_1110;

/// A digit, `1` through `9`.
///
/// A newtype rather than a `u8` because a Sudoku grid holds `Option<Digit>`,
/// and "empty" being `None` rather than `0` is what stops an empty cell ever
/// being compared against, placed, or counted as a digit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digit(u8);

impl Digit {
    /// Every digit, ascending.
    pub const ALL: [Digit; 9] = [
        Digit(1),
        Digit(2),
        Digit(3),
        Digit(4),
        Digit(5),
        Digit(6),
        Digit(7),
        Digit(8),
        Digit(9),
    ];

    /// Builds a digit from `1..=9`, or `None` for anything else.
    pub const fn new(value: u8) -> Option<Self> {
        if matches!(value, 1..=9) {
            Some(Self(value))
        } else {
            None
        }
    }

    /// The `1..=9` value.
    pub const fn value(self) -> u8 {
        self.0
    }

    /// The bit this digit occupies in a candidate mask.
    pub(crate) const fn mask(self) -> u16 {
        1 << self.0
    }
}

impl fmt::Display for Digit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One of the eighty-one cells: `row` down from the top, `column` across from
/// the left, both `0..9`.
///
/// Screen order on purpose — row-major, origin top-left — because unlike
/// chess there is no established board-relative convention to preserve, and
/// every consumer (the renderer, the save file, the solver's flat array) wants
/// the same order. The one-based `R4C7` spelling exists for people, in
/// [`Self::name`], and nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cell {
    row: u8,
    column: u8,
}

impl Cell {
    /// Builds a cell from `0..9` indices, or `None` if either is off the grid.
    pub const fn new(row: u8, column: u8) -> Option<Self> {
        if row < 9 && column < 9 {
            Some(Self { row, column })
        } else {
            None
        }
    }

    /// The `0..9` row, counting down from the top.
    pub const fn row(self) -> u8 {
        self.row
    }

    /// The `0..9` column, counting across from the left.
    pub const fn column(self) -> u8 {
        self.column
    }

    /// Which 3x3 block this cell is in, `0..9` in reading order.
    pub const fn block(self) -> u8 {
        (self.row / 3) * 3 + self.column / 3
    }

    /// The `0..81` row-major index.
    pub const fn index(self) -> usize {
        self.row as usize * 9 + self.column as usize
    }

    /// The cell at a `0..81` row-major index.
    pub const fn from_index(index: usize) -> Option<Self> {
        if index < 81 {
            Self::new((index / 9) as u8, (index % 9) as u8)
        } else {
            None
        }
    }

    /// Every cell, in reading order.
    pub fn all() -> impl Iterator<Item = Cell> {
        (0..81).filter_map(Self::from_index)
    }

    /// The twenty other cells that share a row, a column or a block with this
    /// one — exactly the cells a digit here rules out.
    pub fn peers(self) -> impl Iterator<Item = Cell> {
        Self::all().filter(move |other| {
            *other != self
                && (other.row == self.row
                    || other.column == self.column
                    || other.block() == self.block())
        })
    }

    /// The one-based `R4C7` spelling, for a status line or an error message.
    pub fn name(self) -> String {
        format!("R{}C{}", self.row + 1, self.column + 1)
    }
}

/// A 9x9 grid of optional digits.
///
/// Both a puzzle's givens and a board being played are this type. What
/// distinguishes them is which [`Puzzle`](crate::Puzzle) they came from, not
/// anything stored here — a grid has no notion of a cell being locked, so no
/// code path can accidentally decide one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    cells: [Option<Digit>; 81],
}

impl Grid {
    /// Eighty-one empty cells.
    pub const EMPTY: Self = Self { cells: [None; 81] };

    /// What is in `cell`.
    pub const fn get(&self, cell: Cell) -> Option<Digit> {
        self.cells[cell.index()]
    }

    /// Puts `digit` in `cell`, or empties it.
    ///
    /// Unchecked on purpose: this is the storage primitive, and legality is
    /// [`Game`](crate::Game)'s to enforce. Generation and solving both need to
    /// place digits they have already checked by other means, and a checked
    /// setter here would either duplicate that work or tempt a caller to skip
    /// it.
    pub fn set(&mut self, cell: Cell, digit: Option<Digit>) {
        self.cells[cell.index()] = digit;
    }

    /// How many cells hold a digit.
    pub fn filled(&self) -> usize {
        self.cells.iter().filter(|cell| cell.is_some()).count()
    }

    /// Whether every cell holds a digit. Says nothing about legality.
    pub fn is_complete(&self) -> bool {
        self.cells.iter().all(Option::is_some)
    }

    /// The first cell sharing a row, column or block with `cell` that already
    /// holds `digit`, or `None` if `digit` is placeable there.
    ///
    /// Returns the *cell*, not a bool: "R3C4 already has a 7" is the only
    /// version of this answer a person can act on.
    pub fn conflict(&self, cell: Cell, digit: Digit) -> Option<Cell> {
        cell.peers().find(|peer| self.get(*peer) == Some(digit))
    }

    /// Whether `digit` can go in `cell` without breaking a constraint.
    pub fn accepts(&self, cell: Cell, digit: Digit) -> bool {
        self.conflict(cell, digit).is_none()
    }

    /// Whether no row, column or block holds a digit twice.
    ///
    /// True of an empty grid and of any legal position part-way through: this
    /// is the constraint, not completion.
    pub fn is_valid(&self) -> bool {
        let mut rows = [0u16; 9];
        let mut columns = [0u16; 9];
        let mut blocks = [0u16; 9];
        for cell in Cell::all() {
            let Some(digit) = self.get(cell) else {
                continue;
            };
            let mask = digit.mask();
            for used in [
                &mut rows[cell.row() as usize],
                &mut columns[cell.column() as usize],
                &mut blocks[cell.block() as usize],
            ] {
                if *used & mask != 0 {
                    return false;
                }
                *used |= mask;
            }
        }
        true
    }

    /// Whether this grid is a finished, legal Sudoku.
    pub fn is_solved(&self) -> bool {
        self.is_complete() && self.is_valid()
    }

    /// Eighty-one characters in reading order, `.` for an empty cell.
    ///
    /// The save format's whole grid representation, and the shape every
    /// Sudoku fixture in the wild is written in, which is why the tests can
    /// use published puzzles directly.
    pub fn to_line(&self) -> String {
        self.cells
            .iter()
            .map(|cell| match cell {
                Some(digit) => char::from(b'0' + digit.value()),
                None => '.',
            })
            .collect()
    }

    /// Parses [`Self::to_line`]'s output, accepting `0` and `.` for an empty
    /// cell. `None` if it is not exactly 81 valid characters.
    pub fn parse_line(text: &str) -> Option<Self> {
        let mut grid = Self::EMPTY;
        let mut seen = 0usize;
        for (index, character) in text.chars().enumerate() {
            let cell = Cell::from_index(index)?;
            match character {
                '.' | '0' => {}
                '1'..='9' => grid.set(cell, Digit::new(character as u8 - b'0')),
                _ => return None,
            }
            seen = index + 1;
        }
        (seen == 81).then_some(grid)
    }
}

#[cfg(test)]
mod tests {
    use super::{Cell, Digit, Grid};

    #[test]
    fn a_digit_is_one_through_nine_and_nothing_else() {
        assert_eq!(Digit::new(1).map(Digit::value), Some(1));
        assert_eq!(Digit::new(9).map(Digit::value), Some(9));
        assert_eq!(Digit::new(0), None);
        assert_eq!(Digit::new(10), None);
        assert_eq!(Digit::ALL.len(), 9);
        assert_eq!(Digit::ALL[0], Digit::new(1).unwrap());
        assert_eq!(Digit::ALL[8].to_string(), "9");
    }

    #[test]
    fn there_are_eighty_one_cells_and_the_index_round_trips() {
        let all: Vec<_> = Cell::all().collect();
        assert_eq!(all.len(), 81);
        for cell in all {
            assert_eq!(Cell::from_index(cell.index()), Some(cell));
        }
        assert_eq!(Cell::from_index(81), None);
        assert_eq!(Cell::new(9, 0), None);
        assert_eq!(Cell::new(0, 9), None);
    }

    #[test]
    fn blocks_are_numbered_in_reading_order() {
        assert_eq!(Cell::new(0, 0).unwrap().block(), 0);
        assert_eq!(Cell::new(0, 8).unwrap().block(), 2);
        assert_eq!(Cell::new(4, 4).unwrap().block(), 4);
        assert_eq!(Cell::new(8, 0).unwrap().block(), 6);
        assert_eq!(Cell::new(8, 8).unwrap().block(), 8);
    }

    #[test]
    fn cells_are_named_one_based_row_first() {
        assert_eq!(Cell::new(0, 0).unwrap().name(), "R1C1");
        assert_eq!(Cell::new(3, 6).unwrap().name(), "R4C7");
        assert_eq!(Cell::new(8, 8).unwrap().name(), "R9C9");
    }

    #[test]
    fn every_cell_has_exactly_twenty_peers() {
        for cell in Cell::all() {
            let peers: Vec<_> = cell.peers().collect();
            assert_eq!(peers.len(), 20, "{}", cell.name());
            assert!(!peers.contains(&cell), "a cell is not its own peer");
            // The count is only right if the three units overlap the way they
            // should: 8 + 8 + 8 minus the 4 the block shares with the row and
            // column.
            assert!(peers.iter().all(|peer| peer.row() == cell.row()
                || peer.column() == cell.column()
                || peer.block() == cell.block()));
        }
    }

    #[test]
    fn a_conflict_names_the_cell_that_already_holds_the_digit() {
        let mut grid = Grid::EMPTY;
        let five = Digit::new(5).unwrap();
        let held = Cell::new(0, 0).unwrap();
        grid.set(held, Some(five));

        assert_eq!(grid.conflict(Cell::new(0, 8).unwrap(), five), Some(held));
        assert_eq!(grid.conflict(Cell::new(8, 0).unwrap(), five), Some(held));
        assert_eq!(grid.conflict(Cell::new(1, 1).unwrap(), five), Some(held));
        assert_eq!(grid.conflict(Cell::new(4, 4).unwrap(), five), None);
        assert!(grid.accepts(Cell::new(4, 4).unwrap(), five));
    }

    #[test]
    fn an_empty_grid_is_valid_but_not_solved() {
        let grid = Grid::EMPTY;
        assert!(grid.is_valid());
        assert!(!grid.is_complete());
        assert!(!grid.is_solved());
        assert_eq!(grid.filled(), 0);
    }

    #[test]
    fn a_duplicate_in_any_unit_makes_a_grid_invalid() {
        let five = Digit::new(5).unwrap();
        for (row, column) in [(0, 8), (8, 0), (1, 1)] {
            let mut grid = Grid::EMPTY;
            grid.set(Cell::new(0, 0).unwrap(), Some(five));
            grid.set(Cell::new(row, column).unwrap(), Some(five));
            assert!(!grid.is_valid(), "duplicate at R{row}C{column} accepted");
        }
    }

    #[test]
    fn a_grid_round_trips_through_its_line_form() {
        const PUZZLE: &str =
            "53..7....6..195....98....6.8...6...34..8.3..17...2...6.6....28....419..5....8..79";
        let grid = Grid::parse_line(PUZZLE).expect("a published puzzle parses");
        assert_eq!(grid.to_line(), PUZZLE);
        assert_eq!(grid.filled(), 30);
        assert!(grid.is_valid());
        assert_eq!(
            grid.get(Cell::new(0, 0).unwrap()),
            Some(Digit::new(5).unwrap())
        );
        assert_eq!(grid.get(Cell::new(0, 2).unwrap()), None);
    }

    #[test]
    fn a_line_that_is_not_a_grid_is_refused_rather_than_padded() {
        assert!(Grid::parse_line("").is_none());
        assert!(Grid::parse_line(&".".repeat(80)).is_none());
        assert!(Grid::parse_line(&".".repeat(82)).is_none());
        assert!(Grid::parse_line(&"x".repeat(81)).is_none());
        // `0` is the other spelling of an empty cell, and is accepted.
        assert_eq!(Grid::parse_line(&"0".repeat(81)), Some(Grid::EMPTY));
    }
}
