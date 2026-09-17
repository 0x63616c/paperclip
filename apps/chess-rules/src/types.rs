//! Board vocabulary shared by every other module here.
//!
//! Nothing in this file names a `chess::` type: it is the boundary ADR-0017
//! draws, and application code should never need to import `chess` itself to
//! use this crate.

/// Which side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    /// Moves first.
    White,
    /// Moves second.
    Black,
}

impl Color {
    /// The side that did not just move.
    pub const fn opponent(self) -> Self {
        match self {
            Color::White => Color::Black,
            Color::Black => Color::White,
        }
    }
}

/// Which kind of piece, independent of colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PieceKind {
    /// Pawn.
    Pawn,
    /// Knight.
    Knight,
    /// Bishop.
    Bishop,
    /// Rook.
    Rook,
    /// Queen.
    Queen,
    /// King.
    King,
}

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

    fn from_letter(letter: char) -> Option<Self> {
        if letter.is_ascii_lowercase() {
            Self::new(letter as u8 - b'a')
        } else {
            None
        }
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

    fn from_number(digit: char) -> Option<Self> {
        let number = digit.to_digit(10)?;
        if (1..=8).contains(&number) {
            Self::new(number as u8 - 1)
        } else {
            None
        }
    }
}

/// One of the sixty-four squares.
///
/// Serialises as its algebraic name (`"e4"`), not as nested file/rank
/// numbers, so a save file reads the way a person would write the move down.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct Square {
    /// Which file.
    pub file: File,
    /// Which rank.
    pub rank: Rank,
}

impl TryFrom<String> for Square {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Square::parse(&value).ok_or_else(|| format!("{value:?} is not a square"))
    }
}

impl From<Square> for String {
    fn from(square: Square) -> Self {
        square.name()
    }
}

impl Square {
    /// Builds a square from `0..8` indices, or `None` if either is off the board.
    pub fn new(file: u8, rank: u8) -> Option<Self> {
        Some(Self {
            file: File::new(file)?,
            rank: Rank::new(rank)?,
        })
    }

    /// Algebraic notation, such as `e4`.
    pub fn name(self) -> String {
        format!("{}{}", self.file.letter(), self.rank.number())
    }

    /// Parses algebraic notation, such as `e4`. `None` for anything else,
    /// including the wrong case, wrong length, or an out-of-range file/rank.
    pub fn parse(text: &str) -> Option<Self> {
        let mut chars = text.chars();
        let file = File::from_letter(chars.next()?)?;
        let rank = Rank::from_number(chars.next()?)?;
        if chars.next().is_some() {
            return None;
        }
        Some(Self { file, rank })
    }

    /// Every square, `a1` first, in file-major order.
    pub fn all() -> impl Iterator<Item = Square> {
        (0..8).flat_map(|rank| (0..8).filter_map(move |file| Square::new(file, rank)))
    }
}

/// A move as a player states it: where a piece is picked up, where it is put
/// down, and — only when a pawn reaches the back rank — what it becomes.
///
/// This is the whole shape of user intent §16 asks for: "select piece, then
/// destination", with promotion as the one extra choice a player ever makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Move {
    /// Where the piece starts.
    pub from: Square,
    /// Where the piece ends up.
    pub to: Square,
    /// What a pawn reaching the back rank becomes. Ignored, and must be
    /// `None`, for every other move.
    pub promotion: Option<PieceKind>,
}

impl Move {
    /// A move with no promotion, which is every move except a pawn reaching
    /// the back rank.
    pub const fn new(from: Square, to: Square) -> Self {
        Self {
            from,
            to,
            promotion: None,
        }
    }

    /// A pawn promoting on arrival.
    pub const fn promoting(from: Square, to: Square, promotion: PieceKind) -> Self {
        Self {
            from,
            to,
            promotion: Some(promotion),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Move, PieceKind, Square};

    #[test]
    fn squares_round_trip_through_algebraic_notation() {
        for square in Square::all() {
            assert_eq!(Square::parse(&square.name()), Some(square));
        }
    }

    #[test]
    fn parsing_rejects_nonsense() {
        for bad in ["", "e", "e9", "i4", "E4", "e4x", "44"] {
            assert_eq!(Square::parse(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_plain_move_carries_no_promotion() {
        let e2 = Square::parse("e2").unwrap();
        let e4 = Square::parse("e4").unwrap();
        assert_eq!(Move::new(e2, e4).promotion, None);
    }

    #[test]
    fn a_promoting_move_carries_the_chosen_piece() {
        let e7 = Square::parse("e7").unwrap();
        let e8 = Square::parse("e8").unwrap();
        assert_eq!(
            Move::promoting(e7, e8, PieceKind::Queen).promotion,
            Some(PieceKind::Queen)
        );
    }
}
