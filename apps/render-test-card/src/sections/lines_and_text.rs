//! Section: hairlines at each orientation, and the stroke font at every size
//! `paper_sdk::chrome` actually uses.
//!
//! `Canvas::hairline` only draws horizontally; the vertical and diagonal
//! lines here go through `fill_rect` and `stroke_polyline` directly, at the
//! same one-canvas-pixel width, so what is legible on this glass is read off
//! all three orientations rather than assumed from one.

use paper_sdk::{Canvas, Point, Rect, TextStyle, palette};

/// Every text size `paper_sdk::chrome` draws with today
/// (`draw_status_bar`, `draw_action`, `draw_nav`'s tab label, and the
/// section heading), so this section is exactly what the chrome asks the
/// stroke font to do, not a size nobody uses.
const CHROME_SIZES: [f32; 4] = [30.0, 34.0, 40.0, 52.0];

const LINE_BOX: f32 = 160.0;

pub(crate) fn render(canvas: &mut Canvas, content: Rect) {
    canvas.draw_text(
        "1px LINES, EACH ORIENTATION",
        Point::new(content.x, content.y),
        TextStyle::new(26.0, palette::INK_SOFT).with_tracking(0.08),
    );

    let box_top = content.y + 44.0;
    let gap = 60.0;
    let box_width = (content.width - gap * 2.0) / 3.0;

    let horizontal = Rect::new(content.x, box_top, box_width, LINE_BOX);
    canvas.stroke_rect(horizontal, palette::HAIRLINE, 1.0);
    canvas.fill_rect(
        Rect::new(
            horizontal.x + 12.0,
            horizontal.center().y,
            horizontal.width - 24.0,
            1.0,
        ),
        palette::INK,
    );
    label(canvas, horizontal, "HORIZONTAL");

    let vertical = Rect::new(horizontal.right() + gap, box_top, box_width, LINE_BOX);
    canvas.stroke_rect(vertical, palette::HAIRLINE, 1.0);
    canvas.fill_rect(
        Rect::new(
            vertical.center().x,
            vertical.y + 12.0,
            1.0,
            vertical.height - 24.0,
        ),
        palette::INK,
    );
    label(canvas, vertical, "VERTICAL");

    let diagonal = Rect::new(vertical.right() + gap, box_top, box_width, LINE_BOX);
    canvas.stroke_rect(diagonal, palette::HAIRLINE, 1.0);
    canvas.stroke_polyline(
        &[
            Point::new(diagonal.x + 12.0, diagonal.y + 12.0),
            Point::new(diagonal.right() - 12.0, diagonal.bottom() - 12.0),
        ],
        palette::INK,
        1.0,
    );
    label(canvas, diagonal, "DIAGONAL");

    let mut y = box_top + LINE_BOX + 72.0;
    canvas.draw_text(
        "STROKE FONT AT EVERY SIZE THE CHROME USES",
        Point::new(content.x, y),
        TextStyle::new(26.0, palette::INK_SOFT).with_tracking(0.08),
    );
    y += 56.0;
    for size in CHROME_SIZES {
        canvas.draw_text(
            "AaBbCc 0123",
            Point::new(content.x, y),
            TextStyle::new(size, palette::INK).with_weight(0.12),
        );
        canvas.draw_text(
            &format!("{}px", size as u32),
            Point::new(content.right(), y + size / 2.0 - 12.0),
            TextStyle::new(22.0, palette::INK_SOFT).right_aligned(),
        );
        y += size + 40.0;
    }
}

fn label(canvas: &mut Canvas, rect: Rect, text: &str) {
    canvas.draw_text(
        text,
        Point::new(rect.center().x, rect.bottom() + 12.0),
        TextStyle::new(22.0, palette::INK_SOFT).centered(),
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
        assert!(coverage > 0.0, "coverage was {coverage}");
        assert!(coverage < 0.98, "coverage was {coverage}");
    }
}
