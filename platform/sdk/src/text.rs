//! A built-in stroke font.
//!
//! Stage 1 needs a few dozen legible capitals and digits on a 1620 × 2160
//! canvas, and needs them without committing to a text stack or vendoring a
//! font binary before anyone has seen type on the actual panel. So glyphs are
//! polylines here: no shaping, no hinting, no licence, no binary blob, and
//! stroke weight that stays crisp when the preview is scaled down.
//!
//! This is explicitly not the answer for app text. Real shaping — lower case,
//! kerning, wrapping, non-Latin — is a Stage 6 decision (§18 item 5), taken
//! once the device tells us what weight actually survives an e-ink refresh.
//! See ADR-0004.

use crate::color::Color;

/// Glyph design box width, in font units.
const GLYPH_WIDTH: f32 = 6.0;
/// Glyph design box height, in font units. A style's `size` is this height.
const GLYPH_HEIGHT: f32 = 10.0;
/// Pen advance per glyph, in font units; the surplus is the side bearing.
const ADVANCE: f32 = 8.0;

/// Where a run of text sits relative to the point it is drawn at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    /// The point is the left edge of the run.
    Left,
    /// The point is the horizontal middle of the run.
    Center,
    /// The point is the right edge of the run.
    Right,
}

/// How a run of text is drawn.
///
/// `size` is the cap height in canvas pixels, and the point passed to
/// [`Canvas::draw_text`](crate::Canvas::draw_text) is the **top** of that box,
/// not a baseline — there are no descenders to hang below one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    /// Cap height in canvas pixels.
    pub size: f32,
    /// Stroke colour.
    pub color: Color,
    /// Stroke width as a fraction of `size`.
    pub weight: f32,
    /// Extra space between glyphs, as a fraction of `size`.
    pub tracking: f32,
    /// Horizontal anchoring.
    pub align: TextAlign,
}

impl TextStyle {
    /// A left-aligned run at `size` in `color`, at the default weight.
    pub fn new(size: f32, color: Color) -> Self {
        Self {
            size,
            color,
            weight: 0.1,
            tracking: 0.0,
            align: TextAlign::Left,
        }
    }

    /// The same style, centred on the point it is drawn at.
    pub fn centered(self) -> Self {
        Self {
            align: TextAlign::Center,
            ..self
        }
    }

    /// The same style, ending at the point it is drawn at.
    pub fn right_aligned(self) -> Self {
        Self {
            align: TextAlign::Right,
            ..self
        }
    }

    /// The same style at a different stroke weight.
    pub fn with_weight(self, weight: f32) -> Self {
        Self { weight, ..self }
    }

    /// The same style with extra letter spacing — what a small label needs to
    /// stay readable once the preview is scaled down.
    pub fn with_tracking(self, tracking: f32) -> Self {
        Self { tracking, ..self }
    }

    /// The same style in a different colour.
    pub fn with_color(self, color: Color) -> Self {
        Self { color, ..self }
    }

    pub(crate) fn stroke_width(self) -> f32 {
        (self.size * self.weight).max(1.0)
    }
}

/// How wide `text` will be, in canvas pixels.
///
/// Callers use this to lay out before drawing; `draw_text` uses the same
/// function, so a measured box and a drawn run always agree.
pub fn measure_text(text: &str, style: TextStyle) -> f32 {
    let count = text.chars().count();
    if count == 0 {
        return 0.0;
    }
    let unit = style.size / GLYPH_HEIGHT;
    let gap = ADVANCE * unit - GLYPH_WIDTH * unit + style.tracking * style.size;
    count as f32 * GLYPH_WIDTH * unit + (count - 1) as f32 * gap
}

/// A glyph's strokes as polylines in the design box, and the pen advance to
/// use afterwards. Unknown characters render as nothing but still advance, so
/// a stray character leaves a gap rather than shifting the whole run.
pub(crate) fn layout(
    text: &str,
    style: TextStyle,
) -> impl Iterator<Item = (f32, &'static [Stroke])> {
    let unit = style.size / GLYPH_HEIGHT;
    let step = GLYPH_WIDTH * unit + (ADVANCE - GLYPH_WIDTH) * unit + style.tracking * style.size;
    text.chars()
        .flat_map(char::to_uppercase)
        .enumerate()
        .map(move |(index, character)| (index as f32 * step, strokes(character)))
}

/// One pen-down polyline in the design box.
pub(crate) type Stroke = &'static [(f32, f32)];

/// Font units to canvas pixels.
pub(crate) fn unit_scale(style: TextStyle) -> f32 {
    style.size / GLYPH_HEIGHT
}

/// The alignment offset to apply before drawing a run.
pub(crate) fn align_offset(text: &str, style: TextStyle) -> f32 {
    match style.align {
        TextAlign::Left => 0.0,
        TextAlign::Center => -measure_text(text, style) / 2.0,
        TextAlign::Right => -measure_text(text, style),
    }
}

fn strokes(character: char) -> &'static [Stroke] {
    match character {
        'A' => &[
            &[(0.0, 10.0), (3.0, 0.0), (6.0, 10.0)],
            &[(1.2, 6.0), (4.8, 6.0)],
        ],
        'B' => &[
            &[
                (0.0, 10.0),
                (0.0, 0.0),
                (4.0, 0.0),
                (5.6, 1.4),
                (5.6, 3.6),
                (4.0, 5.0),
                (0.0, 5.0),
            ],
            &[(4.0, 5.0), (5.8, 6.4), (5.8, 8.6), (4.0, 10.0), (0.0, 10.0)],
        ],
        'C' => &[&[
            (6.0, 2.0),
            (4.0, 0.0),
            (2.0, 0.0),
            (0.0, 2.0),
            (0.0, 8.0),
            (2.0, 10.0),
            (4.0, 10.0),
            (6.0, 8.0),
        ]],
        'D' => &[&[
            (0.0, 0.0),
            (3.0, 0.0),
            (6.0, 3.0),
            (6.0, 7.0),
            (3.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ]],
        'E' => &[
            &[(6.0, 0.0), (0.0, 0.0), (0.0, 10.0), (6.0, 10.0)],
            &[(0.0, 5.0), (4.4, 5.0)],
        ],
        'F' => &[
            &[(6.0, 0.0), (0.0, 0.0), (0.0, 10.0)],
            &[(0.0, 5.0), (4.4, 5.0)],
        ],
        'G' => &[&[
            (6.0, 2.0),
            (4.0, 0.0),
            (2.0, 0.0),
            (0.0, 2.0),
            (0.0, 8.0),
            (2.0, 10.0),
            (4.0, 10.0),
            (6.0, 8.0),
            (6.0, 5.4),
            (3.4, 5.4),
        ]],
        'H' => &[
            &[(0.0, 0.0), (0.0, 10.0)],
            &[(6.0, 0.0), (6.0, 10.0)],
            &[(0.0, 5.0), (6.0, 5.0)],
        ],
        'I' => &[
            &[(1.2, 0.0), (4.8, 0.0)],
            &[(3.0, 0.0), (3.0, 10.0)],
            &[(1.2, 10.0), (4.8, 10.0)],
        ],
        'J' => &[&[(6.0, 0.0), (6.0, 7.6), (4.2, 10.0), (2.0, 10.0), (0.2, 8.4)]],
        'K' => &[
            &[(0.0, 0.0), (0.0, 10.0)],
            &[(6.0, 0.0), (0.4, 5.2)],
            &[(2.2, 3.6), (6.0, 10.0)],
        ],
        'L' => &[&[(0.0, 0.0), (0.0, 10.0), (5.6, 10.0)]],
        'M' => &[&[(0.0, 10.0), (0.0, 0.0), (3.0, 5.0), (6.0, 0.0), (6.0, 10.0)]],
        'N' => &[&[(0.0, 10.0), (0.0, 0.0), (6.0, 10.0), (6.0, 0.0)]],
        'O' => &[&[
            (2.0, 0.0),
            (4.0, 0.0),
            (6.0, 2.0),
            (6.0, 8.0),
            (4.0, 10.0),
            (2.0, 10.0),
            (0.0, 8.0),
            (0.0, 2.0),
            (2.0, 0.0),
        ]],
        'P' => &[&[
            (0.0, 10.0),
            (0.0, 0.0),
            (4.0, 0.0),
            (6.0, 2.0),
            (6.0, 4.0),
            (4.0, 6.0),
            (0.0, 6.0),
        ]],
        'Q' => &[
            &[
                (2.0, 0.0),
                (4.0, 0.0),
                (6.0, 2.0),
                (6.0, 8.0),
                (4.0, 10.0),
                (2.0, 10.0),
                (0.0, 8.0),
                (0.0, 2.0),
                (2.0, 0.0),
            ],
            &[(3.6, 7.0), (6.2, 10.4)],
        ],
        'R' => &[
            &[
                (0.0, 10.0),
                (0.0, 0.0),
                (4.0, 0.0),
                (6.0, 2.0),
                (6.0, 4.0),
                (4.0, 6.0),
                (0.0, 6.0),
            ],
            &[(2.8, 6.0), (6.0, 10.0)],
        ],
        'S' => &[&[
            (5.8, 1.6),
            (4.0, 0.0),
            (2.0, 0.0),
            (0.2, 1.6),
            (0.2, 3.4),
            (2.0, 4.8),
            (4.0, 5.2),
            (5.8, 6.6),
            (5.8, 8.4),
            (4.0, 10.0),
            (2.0, 10.0),
            (0.2, 8.6),
        ]],
        'T' => &[&[(0.0, 0.0), (6.0, 0.0)], &[(3.0, 0.0), (3.0, 10.0)]],
        'U' => &[&[
            (0.0, 0.0),
            (0.0, 7.6),
            (2.0, 10.0),
            (4.0, 10.0),
            (6.0, 7.6),
            (6.0, 0.0),
        ]],
        'V' => &[&[(0.0, 0.0), (3.0, 10.0), (6.0, 0.0)]],
        'W' => &[&[(0.0, 0.0), (1.2, 10.0), (3.0, 4.0), (4.8, 10.0), (6.0, 0.0)]],
        'X' => &[&[(0.0, 0.0), (6.0, 10.0)], &[(6.0, 0.0), (0.0, 10.0)]],
        'Y' => &[
            &[(0.0, 0.0), (3.0, 5.2), (6.0, 0.0)],
            &[(3.0, 5.2), (3.0, 10.0)],
        ],
        'Z' => &[&[(0.0, 0.0), (6.0, 0.0), (0.0, 10.0), (6.0, 10.0)]],
        '0' => &[
            &[
                (2.0, 0.0),
                (4.0, 0.0),
                (6.0, 2.0),
                (6.0, 8.0),
                (4.0, 10.0),
                (2.0, 10.0),
                (0.0, 8.0),
                (0.0, 2.0),
                (2.0, 0.0),
            ],
            &[(1.2, 8.2), (4.8, 1.8)],
        ],
        '1' => &[
            &[(1.0, 2.0), (3.0, 0.0), (3.0, 10.0)],
            &[(1.0, 10.0), (5.0, 10.0)],
        ],
        '2' => &[&[
            (0.2, 2.2),
            (2.0, 0.0),
            (4.2, 0.0),
            (6.0, 2.0),
            (6.0, 3.8),
            (0.2, 10.0),
            (6.0, 10.0),
        ]],
        '3' => &[
            &[
                (0.4, 1.2),
                (2.2, 0.0),
                (4.2, 0.0),
                (6.0, 1.8),
                (4.6, 4.6),
                (2.8, 5.0),
            ],
            &[
                (2.8, 5.0),
                (4.8, 5.4),
                (6.0, 7.4),
                (4.4, 10.0),
                (2.2, 10.0),
                (0.4, 8.8),
            ],
        ],
        '4' => &[&[(4.4, 10.0), (4.4, 0.0), (0.0, 6.8), (6.0, 6.8)]],
        '5' => &[&[
            (5.6, 0.0),
            (0.6, 0.0),
            (0.2, 4.4),
            (3.4, 3.8),
            (5.8, 5.6),
            (5.8, 8.0),
            (4.0, 10.0),
            (1.8, 10.0),
            (0.2, 8.8),
        ]],
        '6' => &[&[
            (5.2, 0.4),
            (2.6, 0.6),
            (0.4, 3.4),
            (0.2, 7.4),
            (2.0, 10.0),
            (4.2, 10.0),
            (6.0, 8.2),
            (5.6, 5.6),
            (3.2, 4.4),
            (0.8, 5.2),
        ]],
        '7' => &[&[(0.0, 0.0), (6.0, 0.0), (2.2, 10.0)]],
        '8' => &[
            &[
                (2.2, 5.0),
                (0.4, 3.6),
                (0.4, 1.6),
                (2.2, 0.0),
                (4.0, 0.0),
                (5.8, 1.6),
                (5.8, 3.6),
                (4.0, 5.0),
                (2.2, 5.0),
            ],
            &[
                (2.2, 5.0),
                (0.2, 6.6),
                (0.2, 8.6),
                (2.2, 10.0),
                (4.0, 10.0),
                (6.0, 8.6),
                (6.0, 6.6),
                (4.0, 5.0),
            ],
        ],
        '9' => &[&[
            (0.8, 9.6),
            (3.4, 9.4),
            (5.6, 6.6),
            (5.8, 2.6),
            (4.0, 0.0),
            (1.8, 0.0),
            (0.0, 1.8),
            (0.4, 4.4),
            (2.8, 5.6),
            (5.2, 4.8),
        ]],
        '-' => &[&[(0.8, 5.0), (5.2, 5.0)]],
        '.' => &[&[(2.8, 9.6), (3.2, 9.6)]],
        ',' => &[&[(3.2, 9.2), (2.4, 10.8)]],
        ':' => &[&[(2.8, 3.2), (3.2, 3.2)], &[(2.8, 7.6), (3.2, 7.6)]],
        '/' => &[&[(6.0, 0.0), (0.0, 10.0)]],
        '+' => &[&[(3.0, 2.0), (3.0, 8.0)], &[(0.0, 5.0), (6.0, 5.0)]],
        '\u{00B7}' => &[&[(2.8, 5.0), (3.2, 5.0)]],
        '(' => &[&[(4.4, 0.0), (1.8, 3.0), (1.8, 7.0), (4.4, 10.0)]],
        ')' => &[&[(1.6, 0.0), (4.2, 3.0), (4.2, 7.0), (1.6, 10.0)]],
        '!' => &[&[(3.0, 0.0), (3.0, 6.8)], &[(2.8, 9.6), (3.2, 9.6)]],
        '?' => &[
            &[
                (0.4, 2.0),
                (2.2, 0.0),
                (4.0, 0.0),
                (5.8, 2.0),
                (5.4, 4.2),
                (3.0, 5.6),
                (3.0, 7.0),
            ],
            &[(2.8, 9.6), (3.2, 9.6)],
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::{TextAlign, TextStyle, align_offset, layout, measure_text, strokes};
    use crate::color::palette;

    fn style() -> TextStyle {
        TextStyle::new(100.0, palette::INK)
    }

    #[test]
    fn empty_text_measures_zero() {
        assert_eq!(measure_text("", style()), 0.0);
    }

    #[test]
    fn width_grows_with_character_count() {
        let one = measure_text("A", style());
        let two = measure_text("AB", style());
        let three = measure_text("ABC", style());
        assert!(two > one && three > two);
        assert!(
            (two - one - (three - two)).abs() < 1e-3,
            "advance is uniform"
        );
    }

    #[test]
    fn width_scales_with_size() {
        let small = measure_text("CHESS", TextStyle::new(50.0, palette::INK));
        let large = measure_text("CHESS", TextStyle::new(100.0, palette::INK));
        assert!((large - small * 2.0).abs() < 1e-3);
    }

    #[test]
    fn tracking_widens_the_run_but_not_the_first_glyph() {
        let plain = measure_text("HOME", style());
        let tracked = measure_text("HOME", style().with_tracking(0.2));
        assert!((tracked - plain - 3.0 * 0.2 * 100.0).abs() < 1e-3);
    }

    #[test]
    fn alignment_offsets_are_zero_half_and_full_width() {
        let width = measure_text("HOME", style());
        assert_eq!(align_offset("HOME", style()), 0.0);
        assert!((align_offset("HOME", style().centered()) + width / 2.0).abs() < 1e-3);
        assert!((align_offset("HOME", style().right_aligned()) + width).abs() < 1e-3);
        assert_eq!(style().centered().align, TextAlign::Center);
    }

    #[test]
    fn every_glyph_the_stage_one_screens_use_is_drawable() {
        let used = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-.,:/+\u{00B7}()!?";
        for character in used.chars() {
            assert!(
                !strokes(character).is_empty(),
                "`{character}` has no strokes"
            );
        }
    }

    #[test]
    fn lowercase_renders_as_its_capital() {
        let shape = |text: &str| -> Vec<usize> {
            layout(text, style())
                .map(|(_, glyph)| glyph.len())
                .collect()
        };
        assert_eq!(shape("home"), shape("HOME"));
        assert!(shape("home").iter().all(|strokes| *strokes > 0));
    }

    #[test]
    fn glyphs_stay_inside_their_design_box() {
        let used = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-:/+()!?";
        for character in used.chars() {
            for stroke in strokes(character) {
                for &(x, y) in *stroke {
                    assert!((-0.5..=6.5).contains(&x), "`{character}` x={x}");
                    assert!((-0.5..=10.5).contains(&y), "`{character}` y={y}");
                }
            }
        }
    }

    #[test]
    fn stroke_width_never_collapses_to_nothing() {
        assert!(TextStyle::new(4.0, palette::INK).stroke_width() >= 1.0);
    }
}
