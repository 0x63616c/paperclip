//! Composing the Chess screen.

use paper_sdk::chrome::{self, MARGIN};
use paper_sdk::{Canvas, Point, Rect, TextStyle, palette};

use crate::board::{BoardLayout, Square};
use crate::pieces::{self, Piece, Placement, Side};

/// Room left either side of the board for rank and file labels.
const LABEL_GUTTER: f32 = 52.0;

/// What the Chess screen is currently showing.
///
/// Two fields, both about presentation. There is no move history, no side to
/// move that anything derives from, and no board state that a tap can change:
/// the position is the starting position, always, until WWW-6 brings a rules
/// library (§16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChessScreen {
    /// Whether Black is at the bottom.
    pub flipped: bool,
    /// The square last pressed. Highlight only — it does not pick up a piece.
    pub selected: Option<Square>,
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

    /// Records a press, if it landed on a square.
    ///
    /// Pressing the selected square clears it, which is the only
    /// state transition this screen has.
    pub fn press(&mut self, layout: BoardLayout, at: Point) {
        if let Some(square) = layout.square_at(at) {
            self.selected = if self.selected == Some(square) {
                None
            } else {
                Some(square)
            };
        }
    }
}

/// Draws the Chess screen and returns the board layout used, so the caller can
/// hit-test presses against exactly what was drawn.
pub fn render(canvas: &mut Canvas, screen: &ChessScreen) -> BoardLayout {
    let bounds = canvas.bounds();
    canvas.clear(palette::PAPER);
    let content = chrome::draw_status_bar(canvas, "CHESS", "TWO PLAYER");

    let heading_bottom = chrome::draw_section_heading(
        canvas,
        "GAME",
        Point::new(MARGIN, content.y + 40.0),
        bounds.width - MARGIN * 2.0,
    );

    canvas.draw_text(
        "WHITE TO MOVE",
        Point::new(MARGIN, heading_bottom + 34.0),
        TextStyle::new(56.0, palette::INK)
            .with_weight(0.12)
            .with_tracking(0.08),
    );
    let selection = screen
        .selected
        .map(|square| square.name().to_uppercase())
        .unwrap_or_else(|| "--".to_owned());
    canvas.draw_text(
        &selection,
        Point::new(bounds.width - MARGIN, heading_bottom + 38.0),
        TextStyle::new(48.0, palette::INK_SOFT)
            .with_tracking(0.1)
            .right_aligned(),
    );

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

    draw_squares(canvas, layout, screen.selected);
    draw_coordinates(canvas, layout);
    draw_position(canvas, layout);

    canvas.draw_text(
        "STARTING POSITION \u{00B7} NO RULES ENGINE YET",
        Point::new(bounds.width / 2.0, layout.board().bottom() + 96.0),
        TextStyle::new(26.0, palette::INK_FAINT)
            .with_tracking(0.22)
            .centered(),
    );

    chrome::draw_footer_actions(canvas, &["NEW GAME", "FLIP BOARD", "HOME"]);

    layout
}

fn draw_squares(canvas: &mut Canvas, layout: BoardLayout, selected: Option<Square>) {
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

fn draw_position(canvas: &mut Canvas, layout: BoardLayout) {
    for square in Square::all() {
        if let Some(placement) = starting_placement(square) {
            pieces::draw(canvas, layout.square_rect(square), placement);
        }
    }
}

/// The piece that starts on `square`, if any.
///
/// A lookup table, not a position type. The moment this grows a "move" it has
/// become the thing §16 says not to write here.
pub fn starting_placement(square: Square) -> Option<Placement> {
    const BACK_RANK: [Piece; 8] = [
        Piece::Rook,
        Piece::Knight,
        Piece::Bishop,
        Piece::Queen,
        Piece::King,
        Piece::Bishop,
        Piece::Knight,
        Piece::Rook,
    ];

    let file = square.file.index() as usize;
    match square.rank.index() {
        0 => Some(Placement::new(Side::White, BACK_RANK[file])),
        1 => Some(Placement::new(Side::White, Piece::Pawn)),
        6 => Some(Placement::new(Side::Black, Piece::Pawn)),
        7 => Some(Placement::new(Side::Black, BACK_RANK[file])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ChessScreen, render, starting_placement};
    use crate::board::Square;
    use crate::pieces::{Piece, Side};
    use paper_sdk::chrome::MIN_TOUCH_TARGET;
    use paper_sdk::{Canvas, SCREEN};

    fn screen_canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    #[test]
    fn the_starting_position_has_thirty_two_pieces_in_the_right_places() {
        let placed: Vec<_> = Square::all().filter_map(starting_placement).collect();
        assert_eq!(placed.len(), 32);
        assert_eq!(placed.iter().filter(|p| p.side == Side::White).count(), 16);
        assert_eq!(placed.iter().filter(|p| p.piece == Piece::Pawn).count(), 16);
        assert_eq!(placed.iter().filter(|p| p.piece == Piece::King).count(), 2);
        assert_eq!(placed.iter().filter(|p| p.piece == Piece::Queen).count(), 2);
    }

    #[test]
    fn the_queens_start_on_their_own_colour() {
        let d1 = Square::new(3, 0).expect("d1");
        let d8 = Square::new(3, 7).expect("d8");
        assert_eq!(starting_placement(d1).unwrap().piece, Piece::Queen);
        assert_eq!(starting_placement(d8).unwrap().piece, Piece::Queen);
        assert_eq!(starting_placement(d1).unwrap().side, Side::White);
        assert_eq!(starting_placement(d8).unwrap().side, Side::Black);

        // Queen on her own colour: d1 is a light square, and the white queen
        // is the light side. Mirror the board and this stops being true.
        assert!(d1.is_light());
        assert!(!d8.is_light());
        assert_eq!(
            starting_placement(Square::new(4, 0).unwrap())
                .unwrap()
                .piece,
            Piece::King
        );
    }

    #[test]
    fn the_middle_of_the_board_is_empty() {
        for rank in 2..6u8 {
            for file in 0..8u8 {
                assert!(starting_placement(Square::new(file, rank).unwrap()).is_none());
            }
        }
    }

    #[test]
    fn the_screen_renders_within_its_bounds_and_leaves_room_below_the_board() {
        let mut canvas = screen_canvas();
        let layout = render(&mut canvas, &ChessScreen::new());
        let board = layout.board();

        assert!(board.x >= 0.0 && board.right() <= SCREEN.width as f32);
        assert!(board.y >= paper_sdk::chrome::STATUS_BAR_HEIGHT);
        assert!(
            board.bottom() <= SCREEN.height as f32 - paper_sdk::chrome::FOOTER_HEIGHT,
            "the board runs into the action row"
        );
        assert!(layout.square_size() >= MIN_TOUCH_TARGET);
    }

    #[test]
    fn the_screen_actually_draws_something_without_going_solid() {
        let mut canvas = screen_canvas();
        render(&mut canvas, &ChessScreen::new());
        let coverage = canvas.ink_coverage();
        assert!(coverage > 0.20, "coverage was {coverage}");
        assert!(coverage < 0.95, "coverage was {coverage}");
    }

    #[test]
    fn flipping_changes_what_is_drawn() {
        let mut upright = screen_canvas();
        let mut flipped = screen_canvas();
        render(&mut upright, &ChessScreen::new());
        render(
            &mut flipped,
            &ChessScreen {
                flipped: true,
                selected: None,
            },
        );
        assert_ne!(
            upright.to_png().expect("encodes"),
            flipped.to_png().expect("encodes")
        );
    }

    #[test]
    fn pressing_a_square_selects_it_and_pressing_it_again_clears_it() {
        let mut canvas = screen_canvas();
        let mut screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen);

        let e4 = Square::new(4, 3).expect("e4");
        let center = layout.square_rect(e4).center();

        screen.press(layout, center);
        assert_eq!(screen.selected, Some(e4));
        screen.press(layout, center);
        assert_eq!(screen.selected, None);
    }

    #[test]
    fn pressing_off_the_board_leaves_the_selection_alone() {
        let mut canvas = screen_canvas();
        let mut screen = ChessScreen::new();
        let layout = render(&mut canvas, &screen);

        let e4 = Square::new(4, 3).expect("e4");
        screen.press(layout, layout.square_rect(e4).center());
        screen.press(layout, paper_sdk::Point::new(4.0, 4.0));
        assert_eq!(screen.selected, Some(e4));
    }

    #[test]
    fn a_selection_is_visible_on_the_screen() {
        let mut plain = screen_canvas();
        let mut selected = screen_canvas();
        render(&mut plain, &ChessScreen::new());
        render(
            &mut selected,
            &ChessScreen {
                flipped: false,
                selected: Square::new(4, 3),
            },
        );
        assert_ne!(
            plain.to_png().expect("encodes"),
            selected.to_png().expect("encodes")
        );
    }
}
