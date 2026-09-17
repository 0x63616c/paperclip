//! Piece drawing.
//!
//! Pieces are vector silhouettes rather than font glyphs: the canvas is
//! greyscale and the panel is reflective, so a piece needs a solid body and a
//! contrasting outline to stay readable on a dark square. A Unicode chess
//! glyph would need a font with those code points, and would hand us a thin
//! outline set we cannot thicken. See ADR-0004.

use paper_sdk::{Canvas, Color, Point, Rect, palette};

/// Which piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Piece {
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

/// Which player's piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// The light pieces.
    White,
    /// The dark pieces.
    Black,
}

/// A piece belonging to a side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Placement {
    /// Which piece.
    pub piece: Piece,
    /// Whose it is.
    pub side: Side,
}

impl Placement {
    /// Builds a placement.
    pub const fn new(side: Side, piece: Piece) -> Self {
        Self { piece, side }
    }
}

impl Side {
    /// Body colour.
    fn fill(self) -> Color {
        match self {
            Side::White => palette::PAPER,
            Side::Black => palette::INK,
        }
    }

    /// Outline colour — always the opposite of the body, so a piece separates
    /// from whichever square it is standing on.
    fn outline(self) -> Color {
        match self {
            Side::White => palette::INK,
            Side::Black => palette::PAPER,
        }
    }
}

/// Draws `placement` centred in `square`.
pub fn draw(canvas: &mut Canvas, square: Rect, placement: Placement) {
    let box_ = square
        .centered_square()
        .inset(square.shortest_side() * 0.06);
    let fill = placement.side.fill();
    let outline = placement.side.outline();
    let weight = (box_.width * 0.035).max(1.5);

    // A dark piece on a dark square needs a halo to read; a light piece on a
    // light square needs its outline for the same reason. One rule, both cases.
    let shapes: &[&[(f32, f32)]] = match placement.piece {
        Piece::Pawn => PAWN,
        Piece::Knight => KNIGHT,
        Piece::Bishop => BISHOP,
        Piece::Rook => ROOK,
        Piece::Queen => QUEEN,
        Piece::King => KING,
    };

    for shape in shapes {
        let points = map(box_, shape);
        canvas.fill_polygon(&points, fill);
        let mut closed = points;
        if let Some(&first) = closed.first() {
            closed.push(first);
        }
        canvas.stroke_polyline(&closed, outline, weight);
    }

    match placement.piece {
        Piece::Knight => {
            // The eye, in the outline colour so it survives on both sides.
            canvas.fill_circle(point(box_, 0.312, 0.392), box_.width * 0.030, outline);
        }
        Piece::Bishop => {
            canvas.fill_circle(point(box_, 0.50, 0.105), box_.width * 0.05, fill);
            canvas.stroke_polyline(
                &[point(box_, 0.515, 0.245), point(box_, 0.625, 0.375)],
                outline,
                weight,
            );
        }
        Piece::Queen => {
            for x in [0.155, 0.385, 0.615, 0.845] {
                canvas.fill_circle(point(box_, x, 0.105), box_.width * 0.042, fill);
                canvas.stroke_polyline(
                    &circle_outline(box_, x, 0.105, 0.042),
                    outline,
                    weight * 0.8,
                );
            }
        }
        _ => {}
    }
}

fn point(box_: Rect, x: f32, y: f32) -> Point {
    Point::new(box_.x + x * box_.width, box_.y + y * box_.height)
}

fn map(box_: Rect, shape: &[(f32, f32)]) -> Vec<Point> {
    shape.iter().map(|&(x, y)| point(box_, x, y)).collect()
}

/// A polygon approximating a circle, used where a stroked outline is wanted
/// around a filled disc.
fn circle_outline(box_: Rect, cx: f32, cy: f32, radius: f32) -> Vec<Point> {
    (0..=16)
        .map(|step| {
            let angle = step as f32 / 16.0 * std::f32::consts::TAU;
            point(box_, cx + radius * angle.cos(), cy + radius * angle.sin())
        })
        .collect()
}

/// Every piece shares this foot, so a row of them sits on one line.
const BASE: &[(f32, f32)] = &[
    (0.135, 0.945),
    (0.865, 0.945),
    (0.795, 0.855),
    (0.205, 0.855),
];

const PAWN: &[&[(f32, f32)]] = &[
    &[
        (0.500, 0.165),
        (0.590, 0.215),
        (0.622, 0.310),
        (0.575, 0.400),
        (0.640, 0.455),
        (0.360, 0.455),
        (0.425, 0.400),
        (0.378, 0.310),
        (0.410, 0.215),
    ],
    &[
        (0.385, 0.455),
        (0.615, 0.455),
        (0.680, 0.700),
        (0.720, 0.860),
        (0.280, 0.860),
        (0.320, 0.700),
    ],
    BASE,
];

const ROOK: &[&[(f32, f32)]] = &[
    &[
        (0.210, 0.185),
        (0.330, 0.185),
        (0.330, 0.270),
        (0.430, 0.270),
        (0.430, 0.185),
        (0.570, 0.185),
        (0.570, 0.270),
        (0.670, 0.270),
        (0.670, 0.185),
        (0.790, 0.185),
        (0.790, 0.370),
        (0.210, 0.370),
    ],
    &[
        (0.270, 0.370),
        (0.730, 0.370),
        (0.690, 0.450),
        (0.310, 0.450),
    ],
    &[
        (0.310, 0.450),
        (0.690, 0.450),
        (0.735, 0.860),
        (0.265, 0.860),
    ],
    BASE,
];

/// A horse's head in profile, facing the queenside. Drawn as one silhouette
/// rather than head-plus-neck parts: the outline is what makes a knight read
/// as a knight, and a seam across the jaw breaks it.
const KNIGHT: &[&[(f32, f32)]] = &[
    &[
        (0.270, 0.860),
        (0.282, 0.700),
        (0.226, 0.596),
        (0.150, 0.520),
        (0.128, 0.446),
        (0.196, 0.408),
        (0.248, 0.416),
        (0.292, 0.336),
        (0.366, 0.256),
        (0.442, 0.196),
        (0.452, 0.086),
        (0.528, 0.192),
        (0.586, 0.098),
        (0.632, 0.238),
        (0.706, 0.344),
        (0.752, 0.470),
        (0.736, 0.604),
        (0.712, 0.724),
        (0.730, 0.860),
    ],
    BASE,
];

const BISHOP: &[&[(f32, f32)]] = &[
    &[
        (0.500, 0.128),
        (0.600, 0.240),
        (0.660, 0.360),
        (0.640, 0.462),
        (0.560, 0.522),
        (0.440, 0.522),
        (0.360, 0.462),
        (0.340, 0.360),
        (0.400, 0.240),
    ],
    &[
        (0.360, 0.522),
        (0.640, 0.522),
        (0.660, 0.585),
        (0.340, 0.585),
    ],
    &[
        (0.360, 0.585),
        (0.640, 0.585),
        (0.712, 0.860),
        (0.288, 0.860),
    ],
    BASE,
];

const QUEEN: &[&[(f32, f32)]] = &[
    &[
        (0.200, 0.370),
        (0.155, 0.135),
        (0.300, 0.285),
        (0.385, 0.120),
        (0.500, 0.270),
        (0.615, 0.120),
        (0.700, 0.285),
        (0.845, 0.135),
        (0.800, 0.370),
    ],
    &[
        (0.240, 0.370),
        (0.760, 0.370),
        (0.740, 0.448),
        (0.260, 0.448),
    ],
    &[
        (0.278, 0.448),
        (0.722, 0.448),
        (0.664, 0.630),
        (0.748, 0.860),
        (0.252, 0.860),
        (0.336, 0.630),
    ],
    BASE,
];

const KING: &[&[(f32, f32)]] = &[
    &[
        (0.452, 0.035),
        (0.548, 0.035),
        (0.548, 0.108),
        (0.620, 0.108),
        (0.620, 0.192),
        (0.548, 0.192),
        (0.548, 0.272),
        (0.452, 0.272),
        (0.452, 0.192),
        (0.380, 0.192),
        (0.380, 0.108),
        (0.452, 0.108),
    ],
    &[
        (0.232, 0.448),
        (0.282, 0.300),
        (0.500, 0.262),
        (0.718, 0.300),
        (0.768, 0.448),
    ],
    &[
        (0.262, 0.448),
        (0.738, 0.448),
        (0.716, 0.522),
        (0.284, 0.522),
    ],
    &[
        (0.284, 0.522),
        (0.716, 0.522),
        (0.648, 0.665),
        (0.752, 0.860),
        (0.248, 0.860),
        (0.352, 0.665),
    ],
    BASE,
];

#[cfg(test)]
mod tests {
    use super::{BASE, BISHOP, KING, KNIGHT, PAWN, Piece, Placement, QUEEN, ROOK, Side, draw};
    use paper_sdk::{Canvas, Rect, Size, palette};

    /// A named piece and the polygons it is built from.
    type NamedShapes = (&'static str, &'static [&'static [(f32, f32)]]);

    fn all_shapes() -> Vec<NamedShapes> {
        vec![
            ("pawn", PAWN),
            ("rook", ROOK),
            ("knight", KNIGHT),
            ("bishop", BISHOP),
            ("queen", QUEEN),
            ("king", KING),
        ]
    }

    #[test]
    fn every_piece_stays_inside_its_square() {
        for (name, shapes) in all_shapes() {
            for shape in shapes {
                for &(x, y) in *shape {
                    assert!((0.0..=1.0).contains(&x), "{name} x={x}");
                    assert!((0.0..=1.0).contains(&y), "{name} y={y}");
                }
            }
        }
    }

    #[test]
    fn every_piece_stands_on_the_shared_base() {
        for (name, shapes) in all_shapes() {
            assert!(
                shapes.contains(&BASE),
                "{name} does not sit on the common base"
            );
        }
    }

    #[test]
    fn every_piece_shape_is_a_real_polygon() {
        for (name, shapes) in all_shapes() {
            for shape in shapes {
                assert!(shape.len() >= 3, "{name} has a degenerate shape");
            }
        }
    }

    /// Fraction of a square's pixels the piece changed.
    fn coverage_on(side: Side, piece: Piece, square_color: paper_sdk::Color) -> f32 {
        const SIDE: u32 = 190;
        let mut canvas = Canvas::new(Size::new(SIDE, SIDE)).expect("a square-sized canvas");
        canvas.clear(square_color);
        draw(
            &mut canvas,
            Rect::new(0.0, 0.0, SIDE as f32, SIDE as f32),
            Placement::new(side, piece),
        );
        let changed = (0..SIDE)
            .flat_map(|y| (0..SIDE).map(move |x| (x, y)))
            .filter(|&(x, y)| canvas.pixel(x, y) != Some(square_color))
            .count();
        changed as f32 / (SIDE * SIDE) as f32
    }

    #[test]
    fn every_piece_is_visible_on_both_square_colours() {
        for side in [Side::White, Side::Black] {
            for piece in [
                Piece::Pawn,
                Piece::Knight,
                Piece::Bishop,
                Piece::Rook,
                Piece::Queen,
                Piece::King,
            ] {
                for square_color in [palette::BOARD_LIGHT, palette::BOARD_DARK] {
                    let coverage = coverage_on(side, piece, square_color);
                    assert!(
                        coverage > 0.10,
                        "{side:?} {piece:?} covers only {coverage} of a {square_color:?} square"
                    );
                    assert!(
                        coverage < 0.75,
                        "{side:?} {piece:?} covers {coverage} of a {square_color:?} square"
                    );
                }
            }
        }
    }

    #[test]
    fn pieces_differ_from_one_another() {
        let render = |piece: Piece| {
            (coverage_on(Side::White, piece, palette::BOARD_LIGHT) * 10_000.0).round() as u32
        };
        let coverages: Vec<u32> = [
            Piece::Pawn,
            Piece::Knight,
            Piece::Bishop,
            Piece::Rook,
            Piece::Queen,
            Piece::King,
        ]
        .into_iter()
        .map(render)
        .collect();

        let mut unique = coverages.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), coverages.len(), "two pieces render alike");
    }
}
