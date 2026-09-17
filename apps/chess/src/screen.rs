//! Composing the Chess screen, and the interaction on top of it.
//!
//! §16's rules — legality, check, checkmate, stalemate, the draw conditions —
//! are [`paper_chess_rules::Game`]'s, not this module's (ADR-0017). What is
//! here is presentation and the one translation this crate needs: its own
//! [`Square`] is a screen coordinate `paper_chess_rules` has never heard of,
//! and [`paper_chess_rules::Square`] is a rules-engine value this screen
//! never draws directly. [`to_rules_square`] and [`from_rules_square`] are
//! the whole of that boundary.

use paper_chess_rules::{ClaimableDraw, Color as RulesColor, Game, Move, Outcome, PieceKind};
use paper_sdk::chrome::{self, MARGIN};
use paper_sdk::{Action, Canvas, Point, PointerEvent, Rect, TextStyle, palette};

use crate::board::{BoardLayout, Square};
use crate::pieces::{self, Piece, Placement, Side};

/// Room left either side of the board for rank and file labels.
const LABEL_GUTTER: f32 = 52.0;

/// Converts a screen square into the one `paper_chess_rules` understands.
///
/// Never fails: both sides share the same `0..8` file/rank convention, so
/// this is a relabelling, not a projection.
fn to_rules_square(square: Square) -> paper_chess_rules::Square {
    paper_chess_rules::Square::new(square.file.index(), square.rank.index())
        .unwrap_or_else(|| unreachable!("a screen Square is always on the board"))
}

/// The other direction of [`to_rules_square`].
fn from_rules_square(square: paper_chess_rules::Square) -> Square {
    Square::new(square.file.index(), square.rank.index())
        .unwrap_or_else(|| unreachable!("a rules Square is always on the board"))
}

const fn to_side(color: RulesColor) -> Side {
    match color {
        RulesColor::White => Side::White,
        RulesColor::Black => Side::Black,
    }
}

const fn to_piece(kind: PieceKind) -> Piece {
    match kind {
        PieceKind::Pawn => Piece::Pawn,
        PieceKind::Knight => Piece::Knight,
        PieceKind::Bishop => Piece::Bishop,
        PieceKind::Rook => Piece::Rook,
        PieceKind::Queen => Piece::Queen,
        PieceKind::King => Piece::King,
    }
}

/// What the Chess screen is currently showing, on top of the game state
/// [`Game`] owns.
///
/// Everything here is either presentation (`flipped`, `selected`) or a
/// question the board alone cannot answer (`pending_promotion` — which piece
/// a completed move becomes is the one choice a player makes that is not
/// "which square" — and `new_game_armed`, §16's "Deliberate New Game").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChessScreen {
    /// Whether Black is at the bottom.
    pub flipped: bool,
    /// The square holding the piece currently picked up.
    pub selected: Option<Square>,
    /// A move is legal except for which piece it promotes to; these are its
    /// `from`/`to` squares, waiting on the fourth tap.
    pub pending_promotion: Option<(Square, Square)>,
    /// Whether NEW GAME was just tapped once and awaits a confirming second
    /// tap.
    pub new_game_armed: bool,
}

impl ChessScreen {
    /// A fresh screen: White at the bottom, nothing selected.
    pub fn new() -> Self {
        Self::default()
    }

    /// Turns the board around.
    pub fn flip(&mut self) {
        self.flipped = !self.flipped;
    }

    /// Handles a tap against the layout a draw already produced, mutating
    /// `game` when the tap completes a move or a claim, and says what the app
    /// should ask the platform for.
    ///
    /// Takes no [`paper_sdk::Context`]: nothing here needs one, and testing
    /// it directly — as the tests in this module do — needs one not to
    /// exist.
    pub fn press(
        &mut self,
        game: &mut Game,
        layout: &ChessLayout,
        pointer: &PointerEvent,
    ) -> Action {
        if !pointer.is_tap() {
            return Action::None;
        }
        let at = pointer.at;

        if let Some(promotion) = &layout.promotion {
            if let Some(piece) = promotion.hit_test(at)
                && let Some((from, to)) = self.pending_promotion.take()
            {
                let mv = Move::promoting(to_rules_square(from), to_rules_square(to), piece);
                // Legality up to the promotion choice was already confirmed
                // when `PromotionRequired` first arrived; only the piece was
                // missing.
                let _ = game.apply(mv);
                self.selected = None;
            }
            return Action::Redraw;
        }

        if let Some(claim) = layout.claim_draw
            && claim.contains(at)
        {
            let _ = game.claim_draw();
            return Action::Redraw;
        }

        if layout.home.contains(at) {
            return Action::Home;
        }

        if layout.flip_board.contains(at) {
            self.flip();
            self.new_game_armed = false;
            return Action::Redraw;
        }

        if layout.new_game.contains(at) {
            if self.new_game_armed {
                *game = Game::new();
                self.new_game_armed = false;
                self.selected = None;
            } else {
                self.new_game_armed = true;
            }
            return Action::Redraw;
        }
        self.new_game_armed = false;

        if game.outcome().is_some() {
            // The game is over; only the footer controls above do anything.
            return Action::None;
        }

        let Some(square) = layout.board.square_at(at) else {
            return Action::None;
        };

        match self.selected {
            Some(from) if from == square => self.selected = None,
            Some(from) => {
                let rules_from = to_rules_square(from);
                let rules_to = to_rules_square(square);
                let legal = game.legal_destinations(rules_from);
                if legal.contains(&rules_to) {
                    match game.apply(Move::new(rules_from, rules_to)) {
                        Ok(_) => self.selected = None,
                        Err(paper_chess_rules::MoveError::PromotionRequired) => {
                            self.pending_promotion = Some((from, square));
                        }
                        Err(_) => {}
                    }
                } else {
                    self.selected = owns_piece_at(game, square).then_some(square);
                }
            }
            None => self.selected = owns_piece_at(game, square).then_some(square),
        }
        Action::Redraw
    }
}

/// Whether the side to move owns the piece on `square`.
fn owns_piece_at(game: &Game, square: Square) -> bool {
    game.piece_at(to_rules_square(square))
        .is_some_and(|(color, _)| color == game.side_to_move())
}

/// Where the four promotion choices are drawn, and which piece a tap on each
/// one means.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PromotionLayout {
    /// One box per piece a pawn may promote to, in the order drawn.
    pub boxes: [(PieceKind, Rect); 4],
}

impl PromotionLayout {
    /// Which piece a tap chose, if it landed in one of the boxes.
    pub fn hit_test(&self, at: Point) -> Option<PieceKind> {
        self.boxes
            .iter()
            .find(|(_, rect)| rect.contains(at))
            .map(|&(kind, _)| kind)
    }
}

/// Where everything on the Chess screen was drawn, so a caller can hit-test
/// a press against exactly what was on the glass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChessLayout {
    /// The board itself.
    pub board: BoardLayout,
    /// The NEW GAME footer action.
    pub new_game: Rect,
    /// The FLIP BOARD footer action.
    pub flip_board: Rect,
    /// The HOME footer action.
    pub home: Rect,
    /// The CLAIM DRAW footer action, present only while one is available.
    pub claim_draw: Option<Rect>,
    /// The promotion picker, present only while a move is waiting on it.
    pub promotion: Option<PromotionLayout>,
}

/// Draws the Chess screen and returns the layout used, so the caller can
/// hit-test presses against exactly what was drawn.
pub fn render(canvas: &mut Canvas, screen: &ChessScreen, game: &Game) -> ChessLayout {
    let bounds = canvas.bounds();
    canvas.clear(palette::PAPER);
    let content = chrome::draw_status_bar(canvas, "CHESS", "TWO PLAYER");

    let heading_bottom = chrome::draw_section_heading(
        canvas,
        "GAME",
        Point::new(MARGIN, content.y + 40.0),
        bounds.width - MARGIN * 2.0,
    );

    draw_status_line(canvas, screen, game, heading_bottom);

    let board_top = heading_bottom + 152.0;
    let board_bottom = bounds.height - chrome::FOOTER_HEIGHT - 96.0;
    let layout = BoardLayout::fit(
        Rect::new(
            MARGIN + LABEL_GUTTER,
            board_top,
            bounds.width - (MARGIN + LABEL_GUTTER) * 2.0,
            board_bottom - board_top,
        ),
        screen.flipped,
    );

    let legal_destinations = screen
        .selected
        .map(|from| game.legal_destinations(to_rules_square(from)))
        .unwrap_or_default();

    draw_squares(canvas, layout, screen.selected, &legal_destinations);
    draw_coordinates(canvas, layout);
    draw_position(canvas, layout, game);

    let claimable = game.outcome().is_none() && game.claimable_draw().is_some();
    let new_game_label = if screen.new_game_armed {
        "CONFIRM NEW GAME?"
    } else {
        "NEW GAME"
    };
    let mut labels = vec![new_game_label, "FLIP BOARD", "HOME"];
    if claimable {
        labels.push("CLAIM DRAW");
    }
    let actions = chrome::draw_footer_actions(canvas, &labels);

    let promotion = screen
        .pending_promotion
        .map(|_| draw_promotion_picker(canvas, layout, game.side_to_move()));

    ChessLayout {
        board: layout,
        new_game: actions[0],
        flip_board: actions[1],
        home: actions[2],
        claim_draw: claimable.then(|| actions[3]),
        promotion,
    }
}

fn draw_status_line(canvas: &mut Canvas, screen: &ChessScreen, game: &Game, heading_bottom: f32) {
    let bounds = canvas.bounds();
    let text = status_text(game);
    canvas.draw_text(
        &text,
        Point::new(MARGIN, heading_bottom + 34.0),
        TextStyle::new(52.0, palette::INK)
            .with_weight(0.12)
            .with_tracking(0.06),
    );

    let trailing = if screen.pending_promotion.is_some() {
        "CHOOSE PROMOTION".to_owned()
    } else {
        screen
            .selected
            .map(|square| square.name().to_uppercase())
            .unwrap_or_else(|| "--".to_owned())
    };
    canvas.draw_text(
        &trailing,
        Point::new(bounds.width - MARGIN, heading_bottom + 38.0),
        TextStyle::new(48.0, palette::INK_SOFT)
            .with_tracking(0.1)
            .right_aligned(),
    );
}

/// The line describing whose move it is, or how the game ended.
fn status_text(game: &Game) -> String {
    let Some(outcome) = game.outcome() else {
        let side = match game.side_to_move() {
            RulesColor::White => "WHITE",
            RulesColor::Black => "BLACK",
        };
        return if game.is_in_check() {
            format!("{side} TO MOVE \u{00B7} CHECK")
        } else {
            format!("{side} TO MOVE")
        };
    };
    match outcome {
        Outcome::Checkmate { winner } => {
            let winner = match winner {
                RulesColor::White => "WHITE",
                RulesColor::Black => "BLACK",
            };
            format!("CHECKMATE \u{00B7} {winner} WINS")
        }
        Outcome::Stalemate => "STALEMATE \u{00B7} DRAW".to_owned(),
        Outcome::InsufficientMaterial => "DRAW \u{00B7} INSUFFICIENT MATERIAL".to_owned(),
        Outcome::FivefoldRepetition => "DRAW \u{00B7} FIVEFOLD REPETITION".to_owned(),
        Outcome::SeventyFiveMoveRule => "DRAW \u{00B7} 75-MOVE RULE".to_owned(),
        Outcome::Claimed(ClaimableDraw::ThreefoldRepetition) => {
            "DRAW CLAIMED \u{00B7} THREEFOLD REPETITION".to_owned()
        }
        Outcome::Claimed(ClaimableDraw::FiftyMoveRule) => {
            "DRAW CLAIMED \u{00B7} FIFTY-MOVE RULE".to_owned()
        }
        // `Outcome` is `#[non_exhaustive]`: a future draw reason this build
        // has never heard of still ends the game, and still says so, just
        // without the specific words for it yet.
        _ => "GAME OVER \u{00B7} DRAW".to_owned(),
    }
}

fn draw_squares(
    canvas: &mut Canvas,
    layout: BoardLayout,
    selected: Option<Square>,
    legal_destinations: &[paper_chess_rules::Square],
) {
    for square in Square::all() {
        let rect = layout.square_rect(square);
        let color = if square.is_light() {
            palette::BOARD_LIGHT
        } else {
            palette::BOARD_DARK
        };
        canvas.fill_rect(rect, color);
    }

    // A frame slightly outside the squares, so the board edge is unambiguous
    // against the paper.
    canvas.stroke_rect(layout.board().inset(-3.0), palette::INK, 6.0);

    if let Some(square) = selected {
        let rect = layout.square_rect(square);
        canvas.stroke_rect(rect.inset(9.0), palette::INK, 9.0);
        canvas.stroke_rect(rect.inset(19.0), palette::PAPER, 5.0);
    }

    // A lighter ring than the selection outline, on the edge of the square
    // rather than over its centre, so a destination that holds a capturable
    // piece still shows the piece rather than covering it. Shape, not just
    // shade, carries the meaning — legal squares never rely on colour alone.
    for &destination in legal_destinations {
        let rect = layout.square_rect(from_rules_square(destination));
        canvas.stroke_rect(rect.inset(14.0), palette::INK, 6.0);
    }
}

fn draw_coordinates(canvas: &mut Canvas, layout: BoardLayout) {
    let board = layout.board();
    let style = TextStyle::new(28.0, palette::INK_SOFT)
        .with_weight(0.12)
        .with_tracking(0.1)
        .centered();

    for index in 0..8u8 {
        let Some(square) = Square::new(index, 0) else {
            continue;
        };
        let rect = layout.square_rect(square);
        canvas.draw_text(
            &square.file.letter().to_string(),
            Point::new(rect.center().x, board.bottom() + 16.0),
            style,
        );
    }
    for index in 0..8u8 {
        let Some(square) = Square::new(0, index) else {
            continue;
        };
        let rect = layout.square_rect(square);
        canvas.draw_text(
            &square.rank.number().to_string(),
            Point::new(board.x - LABEL_GUTTER / 2.0, rect.center().y - 14.0),
            style,
        );
    }
}

fn draw_position(canvas: &mut Canvas, layout: BoardLayout, game: &Game) {
    for square in Square::all() {
        if let Some((color, kind)) = game.piece_at(to_rules_square(square)) {
            let placement = Placement::new(to_side(color), to_piece(kind));
            pieces::draw(canvas, layout.square_rect(square), placement);
        }
    }
}

/// Draws the four-choice promotion picker over the board and returns where
/// each choice landed.
fn draw_promotion_picker(
    canvas: &mut Canvas,
    layout: BoardLayout,
    side: RulesColor,
) -> PromotionLayout {
    const CHOICES: [PieceKind; 4] = [
        PieceKind::Queen,
        PieceKind::Rook,
        PieceKind::Bishop,
        PieceKind::Knight,
    ];
    let board = layout.board();
    let gap = 24.0;
    let box_side = ((board.width - gap * 3.0) / 4.0).min(board.height * 0.32);
    let total_width = box_side * 4.0 + gap * 3.0;
    let left = board.center().x - total_width / 2.0;
    let top = board.center().y - box_side / 2.0;

    let panel = Rect::new(left - 32.0, top - 32.0, total_width + 64.0, box_side + 64.0);
    canvas.fill_round_rect(panel, 20.0, palette::PAPER);
    canvas.stroke_round_rect(panel, 20.0, palette::INK, 5.0);

    let mut boxes = [(CHOICES[0], Rect::new(0.0, 0.0, 0.0, 0.0)); 4];
    for (index, &kind) in CHOICES.iter().enumerate() {
        let rect = Rect::new(
            left + index as f32 * (box_side + gap),
            top,
            box_side,
            box_side,
        );
        canvas.fill_round_rect(rect, 12.0, palette::TILE);
        canvas.stroke_round_rect(rect, 12.0, palette::INK, 4.0);
        pieces::draw(canvas, rect, Placement::new(to_side(side), to_piece(kind)));
        boxes[index] = (kind, rect);
    }

    PromotionLayout { boxes }
}

#[cfg(test)]
mod tests {
    use super::{ChessScreen, render};
    use crate::board::Square;
    use paper_chess_rules::{Color, Game, Move, PieceKind};
    use paper_sdk::chrome::MIN_TOUCH_TARGET;
    use paper_sdk::{Canvas, ContactId, Point, Pointer, PointerEvent, PointerPhase, SCREEN};

    fn screen_canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    fn tap(at: Point) -> PointerEvent {
        PointerEvent::new(at, PointerPhase::Up, Pointer::Touch, ContactId::FIRST)
    }

    fn sq(file: u8, rank: u8) -> Square {
        Square::new(file, rank).expect("a square on the board")
    }

    #[test]
    fn the_starting_position_has_thirty_two_pieces_in_the_right_places() {
        let game = Game::new();
        let placed: Vec<_> = paper_chess_rules::Square::all()
            .filter_map(|square| game.piece_at(square))
            .collect();
        assert_eq!(placed.len(), 32);
        assert_eq!(
            placed.iter().filter(|(c, _)| *c == Color::White).count(),
            16
        );
        assert_eq!(
            placed.iter().filter(|(_, k)| *k == PieceKind::Pawn).count(),
            16
        );
        assert_eq!(
            placed.iter().filter(|(_, k)| *k == PieceKind::King).count(),
            2
        );
    }

    #[test]
    fn the_screen_renders_within_its_bounds_and_leaves_room_below_the_board() {
        let mut canvas = screen_canvas();
        let layout = render(&mut canvas, &ChessScreen::new(), &Game::new());
        let board = layout.board.board();

        assert!(board.x >= 0.0 && board.right() <= SCREEN.width as f32);
        assert!(board.y >= paper_sdk::chrome::STATUS_BAR_HEIGHT);
        assert!(
            board.bottom() <= SCREEN.height as f32 - paper_sdk::chrome::FOOTER_HEIGHT,
            "the board runs into the action row"
        );
        assert!(layout.board.square_size() >= MIN_TOUCH_TARGET);
    }

    #[test]
    fn the_screen_actually_draws_something_without_going_solid() {
        let mut canvas = screen_canvas();
        render(&mut canvas, &ChessScreen::new(), &Game::new());
        let coverage = canvas.ink_coverage();
        assert!(coverage > 0.20, "coverage was {coverage}");
        assert!(coverage < 0.95, "coverage was {coverage}");
    }

    #[test]
    fn flipping_changes_what_is_drawn() {
        let mut upright = screen_canvas();
        let mut flipped = screen_canvas();
        let game = Game::new();
        render(&mut upright, &ChessScreen::new(), &game);
        render(
            &mut flipped,
            &ChessScreen {
                flipped: true,
                ..ChessScreen::new()
            },
            &game,
        );
        assert_ne!(
            upright.to_png().expect("encodes"),
            flipped.to_png().expect("encodes")
        );
    }

    #[test]
    fn tapping_a_piece_then_a_legal_destination_moves_it() {
        let mut canvas = screen_canvas();
        let mut game = Game::new();
        let mut screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen, &game);

        let e2 = layout.board.square_rect(sq(4, 1)).center();
        screen.press(&mut game, &layout, &tap(e2));
        assert_eq!(screen.selected, Some(sq(4, 1)));

        let layout = render(&mut canvas, &screen, &game);
        let e4 = layout.board.square_rect(sq(4, 3)).center();
        screen.press(&mut game, &layout, &tap(e4));

        assert_eq!(screen.selected, None);
        assert_eq!(
            game.piece_at(paper_chess_rules::Square::new(4, 3).unwrap())
                .map(|(_, kind)| kind),
            Some(PieceKind::Pawn)
        );
        assert_eq!(game.side_to_move(), Color::Black);
    }

    #[test]
    fn tapping_the_same_square_twice_clears_the_selection() {
        let mut canvas = screen_canvas();
        let mut game = Game::new();
        let mut screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen, &game);
        let e2 = layout.board.square_rect(sq(4, 1)).center();

        screen.press(&mut game, &layout, &tap(e2));
        assert_eq!(screen.selected, Some(sq(4, 1)));
        screen.press(&mut game, &layout, &tap(e2));
        assert_eq!(screen.selected, None);
    }

    #[test]
    fn tapping_an_illegal_destination_reselects_rather_than_moves() {
        let mut canvas = screen_canvas();
        let mut game = Game::new();
        let mut screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen, &game);

        let a2 = layout.board.square_rect(sq(0, 1)).center();
        screen.press(&mut game, &layout, &tap(a2));
        assert_eq!(screen.selected, Some(sq(0, 1)));

        // b1's knight is not where a2's pawn can go, but it is White's own
        // piece, so the tap picks it up instead of doing nothing.
        let b1 = layout.board.square_rect(sq(1, 0)).center();
        screen.press(&mut game, &layout, &tap(b1));
        assert_eq!(screen.selected, Some(sq(1, 0)));
        assert_eq!(game.side_to_move(), Color::White, "no move was made");
    }

    #[test]
    fn a_promoting_move_opens_the_picker_and_a_choice_completes_it() {
        let mut canvas = screen_canvas();
        // White pawn one step from promoting; kings present, nothing else in
        // the way.
        let mut game = Game::from_fen("k7/4P3/8/8/8/8/8/4K3 w - - 0 1").expect("valid FEN");
        let mut screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen, &game);

        let e7 = layout.board.square_rect(sq(4, 6)).center();
        screen.press(&mut game, &layout, &tap(e7));
        let e8 = layout.board.square_rect(sq(4, 7)).center();
        screen.press(&mut game, &layout, &tap(e8));
        assert_eq!(screen.pending_promotion, Some((sq(4, 6), sq(4, 7))));
        assert_eq!(game.outcome(), None, "no move applied yet");

        let layout = render(&mut canvas, &screen, &game);
        let picker = layout.promotion.expect("the picker is open");
        let (_, knight_box) = picker
            .boxes
            .iter()
            .find(|(kind, _)| *kind == PieceKind::Knight)
            .copied()
            .expect("a knight choice");
        screen.press(&mut game, &layout, &tap(knight_box.center()));

        assert_eq!(screen.pending_promotion, None);
        assert_eq!(
            game.piece_at(paper_chess_rules::Square::new(4, 7).unwrap())
                .map(|(_, kind)| kind),
            Some(PieceKind::Knight)
        );
    }

    #[test]
    fn new_game_needs_two_taps_and_flip_board_disarms_it() {
        let mut canvas = screen_canvas();
        let mut game = Game::new();
        game.apply(Move::new(
            paper_chess_rules::Square::new(4, 1).unwrap(),
            paper_chess_rules::Square::new(4, 3).unwrap(),
        ))
        .unwrap();
        let mut screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen, &game);

        screen.press(&mut game, &layout, &tap(layout.new_game.center()));
        assert!(screen.new_game_armed);
        assert_eq!(game.side_to_move(), Color::Black, "not reset yet");

        screen.press(&mut game, &layout, &tap(layout.flip_board.center()));
        assert!(!screen.new_game_armed, "a different action disarms it");

        screen.press(&mut game, &layout, &tap(layout.new_game.center()));
        screen.press(&mut game, &layout, &tap(layout.new_game.center()));
        assert_eq!(game.side_to_move(), Color::White, "reset to the start");
    }

    #[test]
    fn home_is_always_reachable() {
        let mut canvas = screen_canvas();
        let mut game = Game::new();
        let screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen, &game);
        let mut screen = screen;
        assert_eq!(
            screen.press(&mut game, &layout, &tap(layout.home.center())),
            paper_sdk::Action::Home
        );
    }

    #[test]
    fn a_finished_game_only_answers_footer_taps() {
        let mut canvas = screen_canvas();
        let mut game = Game::from_fen("7k/6pp/8/8/8/8/8/K3Q3 w - - 0 1").expect("valid FEN");
        game.apply(Move::new(
            paper_chess_rules::Square::new(4, 0).unwrap(),
            paper_chess_rules::Square::new(4, 7).unwrap(),
        ))
        .unwrap();
        assert!(game.outcome().is_some());

        let mut screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen, &game);
        let a1 = layout.board.square_rect(sq(0, 0)).center();
        assert_eq!(
            screen.press(&mut game, &layout, &tap(a1)),
            paper_sdk::Action::None
        );
        assert_eq!(
            screen.press(&mut game, &layout, &tap(layout.home.center())),
            paper_sdk::Action::Home
        );
    }

    #[test]
    fn a_hover_is_never_treated_as_a_tap() {
        let mut canvas = screen_canvas();
        let mut game = Game::new();
        let mut screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen, &game);
        let e2 = layout.board.square_rect(sq(4, 1)).center();
        let hover = PointerEvent::new(e2, PointerPhase::Hover, Pointer::Pen, ContactId::FIRST);
        assert_eq!(
            screen.press(&mut game, &layout, &hover),
            paper_sdk::Action::None
        );
        assert_eq!(screen.selected, None);
    }
}
