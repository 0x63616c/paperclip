//! Game state: legality (via `chess`, ADR-0017), check/checkmate/stalemate
//! (also `chess`), and the draw classification `chess` does not do —
//! insufficient material, threefold repetition, the fifty-move rule, and
//! their automatic fivefold/seventy-five-move cousins.

use std::collections::HashMap;

use chess::{Board, BoardStatus, ChessMove, MoveGen};

use crate::error::{ClaimError, MoveError};
use crate::types::{Color, Move, PieceKind, Square};

/// Half-moves since the last capture or pawn move before which the
/// fifty-move rule cannot yet be claimed (FIDE 9.3).
const FIFTY_MOVE_CLAIM: u32 = 100;
/// Automatic once no capture or pawn move has happened for 75 full moves
/// (FIDE 9.6.2) — contrast [`ClaimableDraw::FiftyMoveRule`], which a player
/// must claim before then.
const SEVENTY_FIVE_MOVE_AUTOMATIC: u32 = 150;
/// Occurrences of one position before a draw is claimable (FIDE 9.2/9.3).
const THREEFOLD_CLAIM: u8 = 3;
/// Automatic once a position has recurred this many times (FIDE 9.6.1) —
/// contrast [`ClaimableDraw::ThreefoldRepetition`].
const FIVEFOLD_AUTOMATIC: u8 = 5;

/// A draw either side may claim. Claiming is a decision a player makes, not
/// something that ends the game on its own — see [`Outcome`] for the
/// automatic counterparts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimableDraw {
    /// The current position has occurred three times (FIDE 9.2).
    ThreefoldRepetition,
    /// Fifty moves have passed with no capture or pawn move (FIDE 9.3).
    FiftyMoveRule,
}

/// How a finished game ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Outcome {
    /// The side to move has no legal move and is in check.
    Checkmate {
        /// The side that delivered it.
        winner: Color,
    },
    /// The side to move has no legal move and is not in check.
    Stalemate,
    /// Neither side has enough material to deliver checkmate by any legal
    /// sequence of moves. Scoped to the unambiguous cases only: bare king,
    /// or king plus one minor piece, on both sides combined — see ADR-0017.
    InsufficientMaterial,
    /// The same position has recurred five times (FIDE 9.6.1). Automatic;
    /// contrast [`ClaimableDraw::ThreefoldRepetition`].
    FivefoldRepetition,
    /// Seventy-five moves have passed with no capture or pawn move (FIDE
    /// 9.6.2). Automatic; contrast [`ClaimableDraw::FiftyMoveRule`].
    SeventyFiveMoveRule,
    /// A player claimed a draw that was available.
    Claimed(ClaimableDraw),
}

impl Outcome {
    /// Whether this outcome is a draw rather than a decisive result.
    pub const fn is_draw(self) -> bool {
        !matches!(self, Outcome::Checkmate { .. })
    }
}

/// What [`Game::apply`] did, for a caller that wants to react without
/// re-deriving it from the position that resulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MoveRecord {
    /// The move as applied.
    pub mv: Move,
    /// The piece that moved.
    pub piece: PieceKind,
    /// The piece captured, if any — including a pawn taken en passant.
    pub captured: Option<PieceKind>,
    /// Whether the side now to move is in check.
    pub is_check: bool,
}

/// One entry in a game's history, in the shape the save format persists.
///
/// Deliberately not `Move` or `ClaimableDraw` re-used directly: this is the
/// on-disk contract, and giving it its own type means a future change to
/// either of those does not silently change the save format underneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum GameEvent {
    /// A move was made.
    Move {
        /// Where from.
        from: Square,
        /// Where to.
        to: Square,
        /// What a promoting pawn became.
        promotion: Option<PieceKind>,
    },
    /// A claimable draw was claimed.
    ClaimDraw {
        /// Which one.
        reason: ClaimableDraw,
    },
}

impl From<Move> for GameEvent {
    fn from(mv: Move) -> Self {
        GameEvent::Move {
            from: mv.from,
            to: mv.to,
            promotion: mv.promotion,
        }
    }
}

/// A key that is provably FIDE-correct for detecting repeated positions,
/// regardless of what `chess::Board::get_hash` happens to fold in (ADR-0017):
/// piece placement, side to move, both sides' castling rights, and en passant
/// availability. Built from plain values rather than the library's own
/// `Color`/`CastleRights`/`Square` types so it does not depend on their
/// deriving `Eq`/`Hash` either.
type RepetitionKey = (u64, bool, u8, u8, Option<u8>);

fn repetition_key(board: &Board) -> RepetitionKey {
    (
        board.get_hash(),
        board.side_to_move() == chess::Color::White,
        castle_bits(board.castle_rights(chess::Color::White)),
        castle_bits(board.castle_rights(chess::Color::Black)),
        board.en_passant().map(|square| square.to_index() as u8),
    )
}

fn castle_bits(rights: chess::CastleRights) -> u8 {
    match rights {
        chess::CastleRights::NoRights => 0,
        chess::CastleRights::KingSide => 1,
        chess::CastleRights::QueenSide => 2,
        chess::CastleRights::Both => 3,
    }
}

/// Neither side has enough material to deliver checkmate by any legal
/// sequence of moves, restricted to the cases no arbiter would disagree
/// about. See [`Outcome::InsufficientMaterial`] and ADR-0017.
fn insufficient_material(board: &Board) -> bool {
    let no_heavy_material = board.pieces(chess::Piece::Pawn).popcnt() == 0
        && board.pieces(chess::Piece::Rook).popcnt() == 0
        && board.pieces(chess::Piece::Queen).popcnt() == 0;
    let minors =
        board.pieces(chess::Piece::Knight).popcnt() + board.pieces(chess::Piece::Bishop).popcnt();
    no_heavy_material && minors <= 1
}

fn to_lib_square(square: Square) -> chess::Square {
    chess::Square::make_square(
        chess::Rank::from_index(square.rank.index() as usize),
        chess::File::from_index(square.file.index() as usize),
    )
}

/// Every `chess::Square` is on the board by construction, so this cannot fail
/// — but it stays a function rather than an `unwrap()` at each call site so
/// that claim is written once.
fn from_lib_square(square: chess::Square) -> Square {
    let file = square.get_file().to_index() as u8;
    let rank = square.get_rank().to_index() as u8;
    Square::new(file, rank).unwrap_or_else(|| unreachable!("chess::Square {file},{rank}"))
}

const fn to_color(color: chess::Color) -> Color {
    match color {
        chess::Color::White => Color::White,
        chess::Color::Black => Color::Black,
    }
}

const fn to_lib_piece(piece: PieceKind) -> chess::Piece {
    match piece {
        PieceKind::Pawn => chess::Piece::Pawn,
        PieceKind::Knight => chess::Piece::Knight,
        PieceKind::Bishop => chess::Piece::Bishop,
        PieceKind::Rook => chess::Piece::Rook,
        PieceKind::Queen => chess::Piece::Queen,
        PieceKind::King => chess::Piece::King,
    }
}

const fn from_lib_piece(piece: chess::Piece) -> PieceKind {
    match piece {
        chess::Piece::Pawn => PieceKind::Pawn,
        chess::Piece::Knight => PieceKind::Knight,
        chess::Piece::Bishop => PieceKind::Bishop,
        chess::Piece::Rook => PieceKind::Rook,
        chess::Piece::Queen => PieceKind::Queen,
        chess::Piece::King => PieceKind::King,
    }
}

/// One local two-player game: the position, whether it is over, and the
/// history needed to resume it and to judge repetition and the fifty-move
/// rule (§16).
#[derive(Debug, Clone)]
pub struct Game {
    board: Board,
    outcome: Option<Outcome>,
    halfmove_clock: u32,
    position_counts: HashMap<RepetitionKey, u8>,
    events: Vec<GameEvent>,
}

impl Game {
    /// The starting position, White to move.
    pub fn new() -> Self {
        Self::from_board(Board::default(), 0)
    }

    /// Builds a position from FEN, for setting up a specific board — a
    /// puzzle, a fixture for testing an app built on this crate — rather than
    /// playing to it one legal move at a time.
    ///
    /// `None` for anything [`chess::Board`]'s own FEN parser and sanity check
    /// refuse, including a position with no king or with the side not to move
    /// already in check. The fifty-move clock and repetition history are not
    /// part of FEN and start fresh, exactly as [`Self::new`]'s do.
    pub fn from_fen(fen: &str) -> Option<Self> {
        use std::str::FromStr as _;
        let board = Board::from_str(fen).ok()?;
        Some(Self::from_board(board, 0))
    }

    fn from_board(board: Board, halfmove_clock: u32) -> Self {
        let mut position_counts = HashMap::new();
        position_counts.insert(repetition_key(&board), 1);
        Self {
            board,
            outcome: None,
            halfmove_clock,
            position_counts,
            events: Vec::new(),
        }
    }

    /// Who moves next.
    pub fn side_to_move(&self) -> Color {
        to_color(self.board.side_to_move())
    }

    /// How many moves and claims have been recorded so far.
    ///
    /// Cheap, and useful for exactly one thing: telling a caller whether a
    /// tap actually changed the game, as distinct from changing only what a
    /// screen is showing (a selection, a highlight) — a save worth writing
    /// again versus one that would not.
    pub fn ply(&self) -> usize {
        self.events.len()
    }

    /// The piece on `square`, if any.
    pub fn piece_at(&self, square: Square) -> Option<(Color, PieceKind)> {
        let lib_square = to_lib_square(square);
        let piece = self.board.piece_on(lib_square)?;
        let color = self.board.color_on(lib_square)?;
        Some((to_color(color), from_lib_piece(piece)))
    }

    /// Whether the side to move is in check. True in a checkmate position as
    /// well as an ordinary one — checking [`Self::outcome`] distinguishes them.
    pub fn is_in_check(&self) -> bool {
        self.board.checkers().popcnt() > 0
    }

    /// How the game ended, or `None` while it continues.
    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    /// A draw available to claim, or `None` if there is not one right now.
    ///
    /// Returns `None` once the game already has an [`Outcome`]: a draw that
    /// was available is not still "claimable" after checkmate ended the game
    /// first, and there is nothing left for a claim to do.
    pub fn claimable_draw(&self) -> Option<ClaimableDraw> {
        if self.outcome.is_some() {
            return None;
        }
        let occurrences = self
            .position_counts
            .get(&repetition_key(&self.board))
            .copied()
            .unwrap_or(0);
        if occurrences >= THREEFOLD_CLAIM {
            Some(ClaimableDraw::ThreefoldRepetition)
        } else if self.halfmove_clock >= FIFTY_MOVE_CLAIM {
            Some(ClaimableDraw::FiftyMoveRule)
        } else {
            None
        }
    }

    /// Every square a piece on `from` may legally move to. Empty once the
    /// game has an outcome, or if `from` holds no piece, or is not the moving
    /// side's — a legal-destination highlight has nothing to show any of
    /// those cases, and this is the one place that decides so an app screen
    /// never has to duplicate the rule.
    pub fn legal_destinations(&self, from: Square) -> Vec<Square> {
        if self.outcome.is_some() {
            return Vec::new();
        }
        let from_lib = to_lib_square(from);
        let mut destinations = Vec::new();
        for candidate in MoveGen::new_legal(&self.board) {
            if candidate.get_source() == from_lib {
                let dest = from_lib_square(candidate.get_dest());
                if !destinations.contains(&dest) {
                    destinations.push(dest);
                }
            }
        }
        destinations
    }

    /// Applies a move, or refuses it.
    ///
    /// `mv.promotion` is required exactly when `mv` is a pawn reaching the
    /// back rank, and refused otherwise — see [`MoveError::PromotionRequired`]
    /// and [`MoveError::PromotionNotAllowed`].
    pub fn apply(&mut self, mv: Move) -> Result<MoveRecord, MoveError> {
        if self.outcome.is_some() {
            return Err(MoveError::GameOver);
        }
        let from = to_lib_square(mv.from);
        let to = to_lib_square(mv.to);
        let Some(moving_piece) = self.board.piece_on(from) else {
            return Err(MoveError::EmptySquare);
        };
        // `piece_on` answering does not imply `color_on` does; both read the
        // same occupancy, so this is unreachable rather than a real case.
        let moving_color = self
            .board
            .color_on(from)
            .unwrap_or_else(|| unreachable!("a square with a piece has a color"));
        if moving_color != self.board.side_to_move() {
            return Err(MoveError::WrongColor);
        }

        let candidates: Vec<ChessMove> = MoveGen::new_legal(&self.board)
            .filter(|candidate| candidate.get_source() == from && candidate.get_dest() == to)
            .collect();
        if candidates.is_empty() {
            return Err(MoveError::Illegal);
        }
        let needs_promotion = candidates
            .iter()
            .any(|candidate| candidate.get_promotion().is_some());
        let chosen = match (needs_promotion, mv.promotion) {
            (true, None) => return Err(MoveError::PromotionRequired),
            (true, Some(piece)) => {
                let wanted = to_lib_piece(piece);
                *candidates
                    .iter()
                    .find(|candidate| candidate.get_promotion() == Some(wanted))
                    .ok_or(MoveError::Illegal)?
            }
            (false, Some(_)) => return Err(MoveError::PromotionNotAllowed),
            (false, None) => candidates[0],
        };

        // A pawn only ever moves diagonally to capture, so a diagonal pawn
        // move onto an empty square can only be an en passant capture — and
        // `MoveGen` already established this move is legal, so it must be
        // one. More robust than matching `Board::en_passant()`'s square
        // against `to`, which makes an assumption about its convention this
        // does not need to make.
        let captured = if let Some(piece) = self.board.piece_on(to) {
            Some(piece)
        } else if moving_piece == chess::Piece::Pawn && from.get_file() != to.get_file() {
            Some(chess::Piece::Pawn)
        } else {
            None
        };
        let resets_clock = moving_piece == chess::Piece::Pawn || captured.is_some();

        self.board = self.board.make_move_new(chosen);
        self.halfmove_clock = if resets_clock {
            0
        } else {
            self.halfmove_clock + 1
        };
        let occurrences = {
            let count = self
                .position_counts
                .entry(repetition_key(&self.board))
                .or_insert(0);
            *count += 1;
            *count
        };
        self.outcome = self.derive_automatic_outcome(occurrences);
        self.events.push(GameEvent::from(mv));

        Ok(MoveRecord {
            mv,
            piece: from_lib_piece(moving_piece),
            captured: captured.map(from_lib_piece),
            is_check: self.is_in_check(),
        })
    }

    /// Claims the available draw, ending the game.
    pub fn claim_draw(&mut self) -> Result<ClaimableDraw, ClaimError> {
        if self.outcome.is_some() {
            return Err(ClaimError::GameOver);
        }
        let claim = self.claimable_draw().ok_or(ClaimError::NothingToClaim)?;
        self.outcome = Some(Outcome::Claimed(claim));
        self.events.push(GameEvent::ClaimDraw { reason: claim });
        Ok(claim)
    }

    fn derive_automatic_outcome(&self, occurrences: u8) -> Option<Outcome> {
        match self.board.status() {
            BoardStatus::Checkmate => Some(Outcome::Checkmate {
                // `status()` is read from `self.board`, which already belongs
                // to the side with no legal move — the mated side.
                winner: to_color(!self.board.side_to_move()),
            }),
            BoardStatus::Stalemate => Some(Outcome::Stalemate),
            BoardStatus::Ongoing if insufficient_material(&self.board) => {
                Some(Outcome::InsufficientMaterial)
            }
            BoardStatus::Ongoing if occurrences >= FIVEFOLD_AUTOMATIC => {
                Some(Outcome::FivefoldRepetition)
            }
            BoardStatus::Ongoing if self.halfmove_clock >= SEVENTY_FIVE_MOVE_AUTOMATIC => {
                Some(Outcome::SeventyFiveMoveRule)
            }
            BoardStatus::Ongoing => None,
        }
    }

    /// The recorded history, in the order it happened. Used by [`crate::save`]
    /// to write and replay the save format; not otherwise part of this
    /// crate's public surface.
    pub(crate) fn events(&self) -> &[GameEvent] {
        &self.events
    }

    /// Rebuilds a game by replaying a recorded history from the start.
    ///
    /// The `Err` string names what went wrong during replay — an out-of-turn
    /// move, an illegal one, or a claim recorded when nothing was claimable —
    /// which is precisely a save file that does not describe a legal game.
    pub(crate) fn from_events(events: &[GameEvent]) -> Result<Self, String> {
        let mut game = Self::new();
        for event in events {
            match *event {
                GameEvent::Move {
                    from,
                    to,
                    promotion,
                } => {
                    game.apply(Move {
                        from,
                        to,
                        promotion,
                    })
                    .map_err(|error| error.to_string())?;
                }
                GameEvent::ClaimDraw { reason } => {
                    let claimed = game.claim_draw().map_err(|error| error.to_string())?;
                    if claimed != reason {
                        return Err(format!(
                            "recorded claim {reason:?} does not match the draw actually \
                             available ({claimed:?})"
                        ));
                    }
                }
            }
        }
        Ok(game)
    }
}

impl Default for Game {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl Game {
    /// Builds a position directly from FEN, with a given fifty-move clock —
    /// for reaching positions no reasonable move sequence should have to
    /// construct just to test what happens *at* them (checkmate, stalemate,
    /// insufficient material, a pin, a clock already near its limit).
    /// History-dependent rules — "castling after the rook has moved",
    /// repetition counting — are tested through real [`Self::apply`] calls
    /// instead, because this constructor cannot fake history, only a
    /// snapshot.
    fn from_position(fen: &str, halfmove_clock: u32) -> Self {
        use std::str::FromStr as _;
        let board = Board::from_str(fen)
            .unwrap_or_else(|error| panic!("{fen:?} is not valid FEN: {error}"));
        Self::from_board(board, halfmove_clock)
    }
}

#[cfg(test)]
mod tests {
    use super::{ClaimableDraw, Game, Outcome};
    use crate::error::{ClaimError, MoveError};
    use crate::types::{Color, Move, PieceKind, Square};

    fn sq(name: &str) -> Square {
        Square::parse(name).unwrap_or_else(|| panic!("{name} is a valid square"))
    }

    fn mv(from: &str, to: &str) -> Move {
        Move::new(sq(from), sq(to))
    }

    // --- castling -----------------------------------------------------------

    #[test]
    fn kingside_castling_moves_the_king_and_rook_together() {
        let mut game = Game::from_position("4k3/8/8/8/8/8/8/4K2R w K - 0 1", 0);
        game.apply(mv("e1", "g1")).expect("O-O is legal here");
        assert_eq!(
            game.piece_at(sq("g1")),
            Some((Color::White, PieceKind::King))
        );
        assert_eq!(
            game.piece_at(sq("f1")),
            Some((Color::White, PieceKind::Rook))
        );
        assert_eq!(game.piece_at(sq("e1")), None);
        assert_eq!(game.piece_at(sq("h1")), None);
    }

    #[test]
    fn queenside_castling_moves_the_king_and_rook_together() {
        let mut game = Game::from_position("4k3/8/8/8/8/8/8/R3K3 w Q - 0 1", 0);
        game.apply(mv("e1", "c1")).expect("O-O-O is legal here");
        assert_eq!(
            game.piece_at(sq("c1")),
            Some((Color::White, PieceKind::King))
        );
        assert_eq!(
            game.piece_at(sq("d1")),
            Some((Color::White, PieceKind::Rook))
        );
        assert_eq!(game.piece_at(sq("a1")), None);
    }

    #[test]
    fn castling_is_refused_once_rights_are_gone() {
        let mut game = Game::from_position("4k3/8/8/8/8/8/8/4K2R w - - 0 1", 0);
        assert_eq!(game.apply(mv("e1", "g1")), Err(MoveError::Illegal));
    }

    #[test]
    fn castling_is_refused_after_the_rook_moves_even_if_it_comes_back() {
        let mut game = Game::from_position("4k3/8/8/8/8/8/8/4K2R w K - 0 1", 0);
        game.apply(mv("h1", "h2")).expect("the rook can step out");
        game.apply(mv("e8", "d8")).expect("Black shuffles");
        game.apply(mv("h2", "h1")).expect("the rook comes back");
        game.apply(mv("d8", "e8")).expect("Black shuffles back");
        // Same rook, same square, same king — but the right was revoked the
        // moment the rook first moved, and moving it back does not restore
        // it. This is the case a stale-rights bug would pass anyway.
        assert_eq!(game.apply(mv("e1", "g1")), Err(MoveError::Illegal));
    }

    #[test]
    fn castling_through_an_attacked_square_is_refused() {
        // A Black rook on f8 attacks f1 down the open f-file: the king is not
        // currently in check, but O-O would pass through it.
        let mut game = Game::from_position("k4r2/8/8/8/8/8/8/4K2R w K - 0 1", 0);
        assert!(!game.is_in_check(), "the king itself is not in check yet");
        assert_eq!(game.apply(mv("e1", "g1")), Err(MoveError::Illegal));
    }

    // --- en passant -----------------------------------------------------------

    #[test]
    fn en_passant_captures_the_pawn_that_just_passed_it() {
        let mut game = Game::new();
        game.apply(mv("e2", "e4")).unwrap();
        game.apply(mv("a7", "a6")).unwrap();
        game.apply(mv("e4", "e5")).unwrap();
        let record = game
            .apply(mv("d7", "d5"))
            .expect("Black's double step opens en passant");
        assert!(!record.is_check);

        let record = game.apply(mv("e5", "d6")).expect("en passant capture");
        assert_eq!(record.captured, Some(PieceKind::Pawn));
        assert_eq!(
            game.piece_at(sq("d6")),
            Some((Color::White, PieceKind::Pawn))
        );
        assert_eq!(
            game.piece_at(sq("d5")),
            None,
            "the captured pawn is removed"
        );
    }

    #[test]
    fn en_passant_stops_being_legal_after_one_move_passes() {
        let mut game = Game::new();
        game.apply(mv("e2", "e4")).unwrap();
        game.apply(mv("a7", "a6")).unwrap();
        game.apply(mv("e4", "e5")).unwrap();
        game.apply(mv("d7", "d5")).unwrap();
        game.apply(mv("b1", "c3")).unwrap(); // White declines the capture.
        game.apply(mv("a6", "a5")).unwrap(); // Black plays on.
        assert_eq!(game.apply(mv("e5", "d6")), Err(MoveError::Illegal));
    }

    // --- promotion --------------------------------------------------------

    const PROMOTION_FEN: &str = "k7/4P3/8/8/8/8/8/4K3 w - - 0 1";

    #[test]
    fn promoting_requires_a_chosen_piece() {
        let mut game = Game::from_position(PROMOTION_FEN, 0);
        assert_eq!(
            game.apply(mv("e7", "e8")),
            Err(MoveError::PromotionRequired)
        );
    }

    #[test]
    fn a_promotion_piece_is_refused_on_a_move_that_does_not_promote() {
        let mut game = Game::from_position(PROMOTION_FEN, 0);
        let not_a_promotion = Move::promoting(sq("e1"), sq("e2"), PieceKind::Queen);
        assert_eq!(
            game.apply(not_a_promotion),
            Err(MoveError::PromotionNotAllowed)
        );
    }

    #[test]
    fn a_pawn_may_promote_to_any_of_the_four_pieces() {
        for piece in [
            PieceKind::Queen,
            PieceKind::Rook,
            PieceKind::Bishop,
            PieceKind::Knight,
        ] {
            let mut game = Game::from_position(PROMOTION_FEN, 0);
            game.apply(Move::promoting(sq("e7"), sq("e8"), piece))
                .unwrap_or_else(|error| panic!("promoting to {piece:?} failed: {error}"));
            assert_eq!(game.piece_at(sq("e8")), Some((Color::White, piece)));
        }
    }

    // --- moves that would expose the king ----------------------------------

    #[test]
    fn a_pinned_piece_may_not_move_off_the_pin() {
        // White rook on e2 is pinned to its own king by the Black rook on e8.
        let mut game = Game::from_position("k3r3/8/8/8/8/8/4R3/4K3 w - - 0 1", 0);
        assert_eq!(game.apply(mv("e2", "a2")), Err(MoveError::Illegal));
        // Moving along the pin, so the king stays covered, is fine.
        game.apply(mv("e2", "e5"))
            .expect("staying on the pin is legal");
    }

    // --- check, checkmate, stalemate ---------------------------------------

    #[test]
    fn a_move_that_checks_without_mating_leaves_the_game_ongoing() {
        let mut game = Game::from_position("4k3/8/8/8/8/8/8/3QK3 w - - 0 1", 0);
        let record = game.apply(mv("d1", "d8")).expect("Qd8+ is legal");
        assert!(record.is_check);
        assert!(game.is_in_check());
        assert_eq!(game.outcome(), None);
    }

    #[test]
    fn checkmate_ends_the_game_with_a_winner() {
        // Corner mate: Kh8 boxed in by its own pawns, Qe8 covers the only
        // square it could otherwise step to.
        let mut game = Game::from_position("7k/6pp/8/8/8/8/8/K3Q3 w - - 0 1", 0);
        let record = game.apply(mv("e1", "e8")).expect("Qe8# is legal");
        assert!(record.is_check);
        assert_eq!(
            game.outcome(),
            Some(Outcome::Checkmate {
                winner: Color::White
            })
        );
        assert!(game.legal_destinations(sq("h8")).is_empty());
    }

    #[test]
    fn stalemate_ends_the_game_as_a_draw_with_no_winner() {
        let mut game = Game::from_position("7k/5K2/8/8/8/8/6Q1/8 w - - 0 1", 0);
        game.apply(mv("g2", "g6")).expect("Qg6 is legal");
        assert_eq!(game.outcome(), Some(Outcome::Stalemate));
        assert!(game.outcome().unwrap().is_draw());
    }

    // --- insufficient material ----------------------------------------------

    #[test]
    fn bare_kings_are_insufficient_material() {
        let mut game = Game::from_position("7k/8/8/8/8/8/8/K7 w - - 0 1", 0);
        game.apply(mv("a1", "a2")).unwrap();
        assert_eq!(game.outcome(), Some(Outcome::InsufficientMaterial));
    }

    #[test]
    fn king_and_one_minor_piece_is_insufficient_material() {
        let mut game = Game::from_position("7k/8/8/8/8/8/8/K3N3 w - - 0 1", 0);
        game.apply(mv("a1", "a2")).unwrap();
        assert_eq!(game.outcome(), Some(Outcome::InsufficientMaterial));
    }

    #[test]
    fn a_lone_rook_is_sufficient_material() {
        let mut game = Game::from_position("7k/8/8/8/8/8/8/K3R3 w - - 0 1", 0);
        game.apply(mv("a1", "a2")).unwrap();
        assert_eq!(game.outcome(), None);
    }

    // --- fifty-move rule and the seventy-five-move automatic draw -----------

    const ROOK_ENDGAME_FEN: &str = "7k/8/8/8/8/8/8/K2R4 w - - 0 1";

    #[test]
    fn the_fifty_move_rule_is_claimable_but_does_not_end_the_game_by_itself() {
        let mut game = Game::from_position(ROOK_ENDGAME_FEN, 99);
        game.apply(mv("a1", "a2")).unwrap();
        assert_eq!(game.claimable_draw(), Some(ClaimableDraw::FiftyMoveRule));
        assert_eq!(
            game.outcome(),
            None,
            "a claimable draw is not an automatic one"
        );
    }

    #[test]
    fn claiming_the_fifty_move_rule_ends_the_game() {
        let mut game = Game::from_position(ROOK_ENDGAME_FEN, 100);
        assert_eq!(game.claim_draw(), Ok(ClaimableDraw::FiftyMoveRule));
        assert_eq!(
            game.outcome(),
            Some(Outcome::Claimed(ClaimableDraw::FiftyMoveRule))
        );
        assert_eq!(game.claim_draw(), Err(ClaimError::GameOver));
    }

    #[test]
    fn claiming_a_draw_with_nothing_available_is_refused() {
        let mut game = Game::from_position(ROOK_ENDGAME_FEN, 0);
        assert_eq!(game.claim_draw(), Err(ClaimError::NothingToClaim));
    }

    #[test]
    fn seventy_five_moves_without_progress_ends_the_game_automatically() {
        let mut game = Game::from_position(ROOK_ENDGAME_FEN, 149);
        game.apply(mv("a1", "a2")).unwrap();
        assert_eq!(game.outcome(), Some(Outcome::SeventyFiveMoveRule));
        assert!(game.outcome().unwrap().is_draw());
    }

    #[test]
    fn a_capture_resets_the_fifty_move_clock() {
        // One move short of claimable; a quiet move would tip it over, but a
        // capture resets the clock to zero instead.
        let mut game = Game::from_position("7k/8/8/8/8/8/7r/K6R w - - 0 1", 99);
        game.apply(Move::new(sq("h1"), sq("h2"))).unwrap();
        assert_eq!(game.claimable_draw(), None);
    }

    #[test]
    fn a_pawn_move_resets_the_fifty_move_clock() {
        let mut game = Game::from_position("7k/8/8/8/8/4p3/8/K2R4 w - - 0 1", 99);
        game.apply(mv("a1", "a2")).unwrap();
        assert_eq!(game.claimable_draw(), Some(ClaimableDraw::FiftyMoveRule));
        game.apply(mv("e3", "e2")).unwrap();
        assert_eq!(game.claimable_draw(), None);
    }

    // --- threefold repetition, claimable then automatic at fivefold --------

    #[test]
    fn repetition_is_claimable_at_three_and_automatic_at_five() {
        let mut game = Game::new();
        let shuffle = [
            mv("g1", "f3"),
            mv("g8", "f6"),
            mv("f3", "g1"),
            mv("f6", "g8"),
        ];
        // Round 1: the starting position recurs for the 2nd time.
        for m in shuffle {
            game.apply(m).unwrap();
        }
        assert_eq!(game.claimable_draw(), None);

        // Round 2: the 3rd occurrence — claimable, not automatic.
        for m in shuffle {
            game.apply(m).unwrap();
        }
        assert_eq!(
            game.claimable_draw(),
            Some(ClaimableDraw::ThreefoldRepetition)
        );
        assert_eq!(game.outcome(), None);

        // Round 3: the 4th occurrence, still nothing forced.
        for m in shuffle {
            game.apply(m).unwrap();
        }
        assert_eq!(game.outcome(), None);

        // Round 4: the 5th occurrence is automatic (FIDE 9.6.1) — nobody had
        // to claim it.
        for m in shuffle {
            game.apply(m).unwrap();
        }
        assert_eq!(game.outcome(), Some(Outcome::FivefoldRepetition));
    }

    #[test]
    fn claiming_threefold_repetition_ends_the_game() {
        let mut game = Game::new();
        let shuffle = [
            mv("g1", "f3"),
            mv("g8", "f6"),
            mv("f3", "g1"),
            mv("f6", "g8"),
        ];
        for _ in 0..2 {
            for m in shuffle {
                game.apply(m).unwrap();
            }
        }
        assert_eq!(game.claim_draw(), Ok(ClaimableDraw::ThreefoldRepetition));
        assert_eq!(
            game.outcome(),
            Some(Outcome::Claimed(ClaimableDraw::ThreefoldRepetition))
        );
    }

    // --- game-over refuses further moves ------------------------------------

    #[test]
    fn a_finished_game_refuses_further_moves() {
        let mut game = Game::from_position("7k/6pp/8/8/8/8/8/K3Q3 w - - 0 1", 0);
        game.apply(mv("e1", "e8")).unwrap();
        assert_eq!(game.apply(mv("a1", "a2")), Err(MoveError::GameOver));
    }

    #[test]
    fn ply_counts_accepted_moves_and_claims_but_not_refused_ones() {
        let mut game = Game::new();
        assert_eq!(game.ply(), 0);
        game.apply(mv("e2", "e4")).unwrap();
        assert_eq!(game.ply(), 1);
        assert_eq!(game.apply(mv("e2", "e4")), Err(MoveError::EmptySquare));
        assert_eq!(game.ply(), 1, "a refused move does not count");
    }

    #[test]
    fn there_is_no_piece_to_move_from_an_empty_square() {
        let mut game = Game::new();
        assert_eq!(game.apply(mv("e4", "e5")), Err(MoveError::EmptySquare));
    }

    #[test]
    fn a_side_may_not_move_its_opponents_piece() {
        let mut game = Game::new();
        assert_eq!(game.apply(mv("e7", "e5")), Err(MoveError::WrongColor));
    }
}
