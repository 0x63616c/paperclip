//! Section: the waveform x content-type matrix.
//!
//! `quill` documents two update classes — mode 0 fast mono dirty-rects for
//! live ink, and modes 3/4/5 for settled/colour content — and WWW-26 found a
//! 4-argument `swapBuffers` taking `EPContentType` that Paperclip discarded
//! from WWW-3 onward. `platform/device/src/waveform.rs` names the four modes
//! this app actually knows about (`Waveform::INK`, `ANIMATION`, `UI`,
//! `CONTENT`); this section draws the identical patch under each label so the
//! right pairing for a purpose is read off the glass rather than assumed.
//!
//! This app does not depend on `paper-device` — the labels below are copied
//! from its constants, not derived from them, because an app has no reason to
//! link the device crate to draw four rectangles. What actually presents each
//! cell under its named waveform is outside this app: see the WWW-47 report
//! for what exercises that axis today and what does not yet.

use paper_sdk::{Canvas, Point, Rect, TextStyle, palette};

/// A pairing this app can name. Mirrors `platform/device/src/waveform.rs`'s
/// four named `Waveform` constants.
struct Pairing {
    name: &'static str,
    detail: &'static str,
}

const PAIRINGS: &[Pairing] = &[
    Pairing {
        name: "INK",
        detail: "mode 0 \u{00b7} mono \u{00b7} fast, live ink",
    },
    Pairing {
        name: "ANIMATION",
        detail: "mode 2 \u{00b7} mono \u{00b7} fast, unused today",
    },
    Pairing {
        name: "UI",
        detail: "mode 3 \u{00b7} mono \u{00b7} settled UI",
    },
    Pairing {
        name: "CONTENT",
        detail: "mode 4 \u{00b7} colour \u{00b7} slow, full colour",
    },
];

const COLUMNS: usize = 2;
const CELL_HEIGHT: f32 = 340.0;
const CELL_GAP: f32 = 24.0;
const PATCH_INSET: f32 = 24.0;

/// Draws the identical test patch — a saturated tint, a black/white hairline
/// pair and a small text sample — into `rect`.
fn draw_patch(canvas: &mut Canvas, rect: Rect) {
    canvas.fill_rect(rect, palette::PAPER);
    canvas.stroke_rect(rect, palette::HAIRLINE, 2.0);
    let swatch = Rect::new(
        rect.x + PATCH_INSET,
        rect.y + PATCH_INSET,
        rect.width - PATCH_INSET * 2.0,
        rect.height * 0.45,
    );
    canvas.fill_rect(swatch, paper_sdk::Color::rgb(0x3B, 0x6E, 0xA5));
    canvas.fill_rect(
        Rect::new(swatch.x, swatch.bottom() + 12.0, swatch.width, 4.0),
        palette::INK,
    );
    canvas.fill_rect(
        Rect::new(swatch.x, swatch.bottom() + 20.0, swatch.width, 4.0),
        palette::PAPER,
    );
    canvas.stroke_rect(
        Rect::new(swatch.x, swatch.bottom() + 20.0, swatch.width, 4.0),
        palette::HAIRLINE,
        1.0,
    );
    canvas.draw_text(
        "AaBb 123",
        Point::new(swatch.x, swatch.bottom() + 36.0),
        TextStyle::new(34.0, palette::INK).with_weight(0.12),
    );
}

pub(crate) fn render(canvas: &mut Canvas, content: Rect) {
    let column_width = (content.width - CELL_GAP * (COLUMNS as f32 - 1.0)) / COLUMNS as f32;
    for (index, pairing) in PAIRINGS.iter().enumerate() {
        let column = index % COLUMNS;
        let row = index / COLUMNS;
        let cell = Rect::new(
            content.x + column as f32 * (column_width + CELL_GAP),
            content.y + row as f32 * (CELL_HEIGHT + CELL_GAP + 40.0),
            column_width,
            CELL_HEIGHT,
        );
        draw_patch(canvas, cell);
        canvas.draw_text(
            pairing.name,
            Point::new(cell.x, cell.bottom() + 8.0),
            TextStyle::new(30.0, palette::INK)
                .with_weight(0.14)
                .with_tracking(0.06),
        );
        canvas.draw_text(
            pairing.detail,
            Point::new(cell.x, cell.bottom() + 40.0),
            TextStyle::new(22.0, palette::INK_SOFT),
        );
    }

    let caption_y = content.y + 2.0 * (CELL_HEIGHT + CELL_GAP + 40.0) + 8.0;
    canvas.draw_text(
        "SAME PATCH, FOUR PAIRINGS, SEE THE WWW-47 REPORT FOR HOW EACH IS PRESENTED",
        Point::new(content.x, caption_y),
        TextStyle::new(22.0, palette::INK_FAINT),
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
        assert!(coverage > 0.02, "coverage was {coverage}");
        assert!(coverage < 0.98, "coverage was {coverage}");
    }
}
