//! Screen furniture every Paperclip screen shares.
//!
//! Kept here rather than in each app so the status bar is in the same place,
//! at the same height, on the home screen and inside Chess. A user who has to
//! re-find the back affordance per app is being charged for our convenience.

use crate::canvas::Canvas;
use crate::color::palette;
use crate::geometry::{Point, Rect};
use crate::text::{TextStyle, measure_text};

/// Height of the bar at the top of every screen.
pub const STATUS_BAR_HEIGHT: f32 = 132.0;

/// Height of the action row at the bottom of a screen that has one.
pub const FOOTER_HEIGHT: f32 = 180.0;

/// Side margin for content.
pub const MARGIN: f32 = 56.0;

/// Smallest side any tappable thing may have, in canvas pixels.
///
/// The Paper Pro's 1620 px width across roughly 179 mm of glass works out
/// near 230 px/inch, which puts 120 px at about 13 mm — comfortably past the
/// ~9 mm that finger targets want, with room to lose some to a bezel-adjacent
/// press. The DPI figure is from published panel dimensions, not measured, so
/// treat the millimetre conversion as provisional (`docs/assumptions.md`); the
/// pixel floor itself is what the code enforces.
pub const MIN_TOUCH_TARGET: f32 = 120.0;

/// Draws the top bar and returns the content rectangle below it.
pub fn draw_status_bar(canvas: &mut Canvas, title: &str, trailing: &str) -> Rect {
    let bounds = canvas.bounds();
    canvas.fill_rect(
        Rect::new(0.0, 0.0, bounds.width, STATUS_BAR_HEIGHT),
        palette::PAPER,
    );
    canvas.draw_text(
        title,
        Point::new(MARGIN, 46.0),
        TextStyle::new(40.0, palette::INK)
            .with_weight(0.13)
            .with_tracking(0.14),
    );
    if !trailing.is_empty() {
        canvas.draw_text(
            trailing,
            Point::new(bounds.width - MARGIN, 48.0),
            TextStyle::new(34.0, palette::INK_SOFT)
                .with_tracking(0.1)
                .right_aligned(),
        );
    }
    canvas.hairline(
        Point::new(0.0, STATUS_BAR_HEIGHT - 2.0),
        bounds.width,
        palette::HAIRLINE,
    );

    Rect::new(
        0.0,
        STATUS_BAR_HEIGHT,
        bounds.width,
        bounds.height - STATUS_BAR_HEIGHT,
    )
}

/// Draws a row of equal-width actions across the bottom of the screen and
/// returns their rectangles, in order, for hit-testing.
///
/// Returns an empty vector for an empty label list rather than dividing by
/// zero to draw nothing.
pub fn draw_footer_actions(canvas: &mut Canvas, labels: &[&str]) -> Vec<Rect> {
    if labels.is_empty() {
        return Vec::new();
    }
    let bounds = canvas.bounds();
    let top = bounds.height - FOOTER_HEIGHT;
    canvas.hairline(Point::new(0.0, top), bounds.width, palette::HAIRLINE);

    let usable = bounds.width - MARGIN * 2.0;
    let gap = 24.0;
    let width = (usable - gap * (labels.len() - 1) as f32) / labels.len() as f32;
    let height = FOOTER_HEIGHT - 48.0;

    labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let rect = Rect::new(
                MARGIN + index as f32 * (width + gap),
                top + 24.0,
                width,
                height,
            );
            draw_action(canvas, rect, label, index == 0);
            rect
        })
        .collect()
}

/// Draws one action button.
pub fn draw_action(canvas: &mut Canvas, rect: Rect, label: &str, emphasised: bool) {
    let radius = 20.0;
    if emphasised {
        canvas.fill_round_rect(rect, radius, palette::EMPHASIS);
    } else {
        canvas.fill_round_rect(rect, radius, palette::PAPER);
        canvas.stroke_round_rect(rect, radius, palette::INK, 3.0);
    }

    let color = if emphasised {
        palette::PAPER
    } else {
        palette::INK
    };
    let style = fit_text(
        label,
        TextStyle::new(34.0, color)
            .with_weight(0.12)
            .with_tracking(0.16)
            .centered(),
        rect.width - 32.0,
        18.0,
    );
    let center = rect.center();
    canvas.draw_text(
        label,
        Point::new(center.x, center.y - style.size / 2.0),
        style,
    );
}

/// Shrinks `style` until `label` fits inside `max_width`, down to `min_size`.
///
/// Shrink rather than clip: a label the user can still read at a smaller size
/// is worth more than half a word at the intended one. Below `min_size` it
/// stops, because past that the answer is a shorter label, not a smaller font.
pub fn fit_text(label: &str, style: TextStyle, max_width: f32, min_size: f32) -> TextStyle {
    let mut style = style;
    while style.size > min_size && measure_text(label, style) > max_width {
        style.size -= 1.0;
    }
    style
}

/// Draws a section heading with a rule under it, returning the `y` below it.
pub fn draw_section_heading(canvas: &mut Canvas, label: &str, at: Point, width: f32) -> f32 {
    canvas.draw_text(
        label,
        at,
        TextStyle::new(30.0, palette::INK_SOFT)
            .with_weight(0.12)
            .with_tracking(0.26),
    );
    let baseline = at.y + 52.0;
    canvas.hairline(Point::new(at.x, baseline), width, palette::HAIRLINE);
    baseline + 2.0
}

#[cfg(test)]
mod tests {
    use super::{
        FOOTER_HEIGHT, MARGIN, MIN_TOUCH_TARGET, STATUS_BAR_HEIGHT, draw_footer_actions,
        draw_status_bar,
    };
    use crate::canvas::Canvas;
    use crate::display::SCREEN;

    fn screen() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    #[test]
    fn the_status_bar_hands_back_everything_below_it() {
        let mut canvas = screen();
        let content = draw_status_bar(&mut canvas, "PAPERCLIP", "100%");
        assert_eq!(content.y, STATUS_BAR_HEIGHT);
        assert_eq!(content.bottom(), SCREEN.height as f32);
        assert!(canvas.ink_coverage() > 0.0);
    }

    #[test]
    fn footer_actions_tile_the_row_without_overlapping() {
        let mut canvas = screen();
        let rects = draw_footer_actions(&mut canvas, &["NEW GAME", "FLIP BOARD", "HOME"]);

        assert_eq!(rects.len(), 3);
        assert!((rects[0].x - MARGIN).abs() < 1e-3);
        assert!((rects[2].right() - (SCREEN.width as f32 - MARGIN)).abs() < 1e-3);
        for pair in rects.windows(2) {
            assert!(pair[0].right() <= pair[1].x + 1e-3, "actions overlap");
        }
        for rect in &rects {
            assert!(rect.bottom() <= SCREEN.height as f32);
            assert!(rect.y >= SCREEN.height as f32 - FOOTER_HEIGHT);
        }
    }

    #[test]
    fn footer_actions_are_big_enough_to_hit() {
        let mut canvas = screen();
        for labels in [
            vec!["HOME"],
            vec!["NEW GAME", "HOME"],
            vec!["NEW GAME", "FLIP BOARD", "HOME"],
        ] {
            for rect in draw_footer_actions(&mut canvas, &labels) {
                assert!(
                    rect.shortest_side() >= MIN_TOUCH_TARGET,
                    "{labels:?} produced {rect:?}"
                );
            }
        }
    }

    #[test]
    fn a_long_label_is_shrunk_rather_than_clipped() {
        use super::fit_text;
        use crate::color::palette;
        use crate::text::{TextStyle, measure_text};

        let style = TextStyle::new(52.0, palette::INK);
        let fitted = fit_text("RETURN TO STOCK", style, 600.0, 20.0);
        assert!(fitted.size < style.size);
        assert!(measure_text("RETURN TO STOCK", fitted) <= 600.0);

        // Something that already fits is left alone.
        assert_eq!(fit_text("HOME", style, 600.0, 20.0).size, style.size);

        // And the floor holds even when nothing can fit.
        assert_eq!(fit_text("RETURN TO STOCK", style, 1.0, 20.0).size, 20.0);
    }

    #[test]
    fn an_empty_action_row_draws_nothing_rather_than_dividing_by_zero() {
        let mut canvas = screen();
        assert!(draw_footer_actions(&mut canvas, &[]).is_empty());
        assert_eq!(canvas.ink_coverage(), 0.0);
    }
}
