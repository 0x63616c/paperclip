//! Section: the greyscale ramp (ADR-0005's ten palette steps), plus a
//! continuous gradient for banding.
//!
//! ADR-0005 chose greyscale first because the panel's colour behaviour was an
//! open hardware gate. This section is that gate's instrument: every step the
//! palette actually defines, in luminance order, and a ramp fine enough that
//! banding — a real e-ink risk a ten-step swatch list would hide — has
//! somewhere to show up.

use paper_sdk::{Canvas, Color, Point, Rect, TextStyle, palette};

const ROW_HEIGHT: f32 = 76.0;
const SWATCH_WIDTH: f32 = 220.0;
const SWATCH_HEIGHT: f32 = 60.0;
const GRADIENT_HEIGHT: f32 = 130.0;

/// The ten palette constants, lightest first.
///
/// Sorted by [`Color::luminance`] rather than hand-ordered, so this list
/// tracks the palette if a step's value ever changes instead of silently
/// drifting from it.
fn steps() -> [(&'static str, Color); 10] {
    // Spaces rather than the constants' own underscores: the stroke font
    // (`platform/sdk/src/text.rs`) has no glyph for `_`, and a name drawn with
    // one leaves a silent gap instead of a legible label.
    let mut steps = [
        ("PAPER", palette::PAPER),
        ("INK", palette::INK),
        ("INK SOFT", palette::INK_SOFT),
        ("INK FAINT", palette::INK_FAINT),
        ("HAIRLINE", palette::HAIRLINE),
        ("TILE", palette::TILE),
        ("BOARD LIGHT", palette::BOARD_LIGHT),
        ("BOARD DARK", palette::BOARD_DARK),
        ("EMPHASIS", palette::EMPHASIS),
        ("LETTERBOX", palette::LETTERBOX),
    ];
    steps.sort_by(|a, b| b.1.luminance().total_cmp(&a.1.luminance()));
    steps
}

/// Draws every palette step as a labelled swatch, then a continuous
/// white-to-black ramp underneath.
pub(crate) fn render(canvas: &mut Canvas, content: Rect) {
    let mut y = content.y;
    for (name, color) in steps() {
        let swatch = Rect::new(content.x, y, SWATCH_WIDTH, SWATCH_HEIGHT);
        canvas.fill_rect(swatch, color);
        canvas.stroke_rect(swatch, palette::HAIRLINE, 2.0);
        canvas.draw_text(
            // No leading `#`: the stroke font has no glyph for it either.
            &format!("{name}  {:02X}{:02X}{:02X}", color.r, color.g, color.b),
            Point::new(swatch.right() + 24.0, swatch.y + 16.0),
            TextStyle::new(28.0, palette::INK).with_tracking(0.04),
        );
        y += ROW_HEIGHT;
    }

    y += 24.0;
    canvas.draw_text(
        "CONTINUOUS RAMP - BANDING SHOWS UP HERE, NOT IN THE SWATCHES ABOVE",
        Point::new(content.x, y),
        TextStyle::new(24.0, palette::INK_SOFT).with_tracking(0.06),
    );
    y += 36.0;

    let ramp = Rect::new(content.x, y, content.width, GRADIENT_HEIGHT);
    let width = ramp.width.max(1.0) as u32;
    let last = (width - 1).max(1);
    for column in 0..width {
        let grey = (255.0 * (1.0 - column as f32 / last as f32)).round() as u8;
        canvas.fill_rect(
            Rect::new(ramp.x + column as f32, ramp.y, 1.0, ramp.height),
            Color::grey(grey),
        );
    }
    canvas.stroke_rect(ramp, palette::HAIRLINE, 2.0);

    canvas.draw_text(
        "WHITE END (GREY 255)",
        Point::new(ramp.x, ramp.bottom() + 12.0),
        TextStyle::new(22.0, palette::INK_SOFT),
    );
    canvas.draw_text(
        "BLACK END (GREY 0)",
        Point::new(ramp.right(), ramp.bottom() + 12.0),
        TextStyle::new(22.0, palette::INK_SOFT).right_aligned(),
    );
}

#[cfg(test)]
mod tests {
    use super::render;
    use paper_sdk::{Canvas, Rect, SCREEN};

    #[test]
    fn draws_something_without_going_solid() {
        let mut canvas = Canvas::new(SCREEN).expect("a canvas");
        render(&mut canvas, Rect::new(40.0, 40.0, 1500.0, 1600.0));
        let coverage = canvas.ink_coverage();
        assert!(coverage > 0.05, "coverage was {coverage}");
        assert!(coverage < 0.98, "coverage was {coverage}");
    }

    #[test]
    fn the_ten_steps_are_all_distinct() {
        let steps = super::steps();
        for (index, (name, color)) in steps.iter().enumerate() {
            for (other_name, other_color) in &steps[index + 1..] {
                assert_ne!(color, other_color, "{name} and {other_name} share a colour");
            }
        }
    }
}
