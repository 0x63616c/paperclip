//! Section: colour on a panel ADR-0005 chose to design greyscale-first for.
//!
//! `rmweb`'s device profile (cited in WWW-26) names this panel E Ink Gallery
//! 3 — colour, ARGB8888. Nobody has looked at what that colour actually does
//! on this glass. Three groups, in saturation order: the six primaries and
//! secondaries at full strength, the same six pulled halfway to white, and a
//! small set of muted tones representative of what a real UI accent would
//! use rather than a synthetic hue nobody would ship.

use paper_sdk::{Canvas, Color, Point, Rect, TextStyle, palette};

struct Group {
    title: &'static str,
    swatches: &'static [(&'static str, Color)],
}

const GROUPS: &[Group] = &[
    Group {
        title: "PRIMARY / SECONDARY - FULL SATURATION",
        swatches: &[
            ("RED", Color::rgb(0xFF, 0x00, 0x00)),
            ("GREEN", Color::rgb(0x00, 0xFF, 0x00)),
            ("BLUE", Color::rgb(0x00, 0x00, 0xFF)),
            ("YELLOW", Color::rgb(0xFF, 0xFF, 0x00)),
            ("CYAN", Color::rgb(0x00, 0xFF, 0xFF)),
            ("MAGENTA", Color::rgb(0xFF, 0x00, 0xFF)),
        ],
    },
    Group {
        title: "PRIMARY / SECONDARY - HALFWAY TO WHITE",
        swatches: &[
            ("RED SOFT", Color::rgb(0xFF, 0x80, 0x80)),
            ("GREEN SOFT", Color::rgb(0x80, 0xFF, 0x80)),
            ("BLUE SOFT", Color::rgb(0x80, 0x80, 0xFF)),
            ("YELLOW SOFT", Color::rgb(0xFF, 0xFF, 0x80)),
            ("CYAN SOFT", Color::rgb(0x80, 0xFF, 0xFF)),
            ("MAGENTA SOFT", Color::rgb(0xFF, 0x80, 0xFF)),
        ],
    },
    Group {
        title: "REALISTIC UI TINTS",
        swatches: &[
            ("ACCENT BLUE", Color::rgb(0x3B, 0x6E, 0xA5)),
            ("SUCCESS GREEN", Color::rgb(0x3E, 0x7C, 0x4A)),
            ("WARNING AMBER", Color::rgb(0xB5, 0x79, 0x3A)),
            ("DANGER RED", Color::rgb(0xB5, 0x44, 0x3A)),
            ("HIGHLIGHT TEAL", Color::rgb(0x3E, 0x8E, 0x86)),
        ],
    },
];

const TITLE_HEIGHT: f32 = 32.0;
const SWATCH_HEIGHT: f32 = 130.0;
const LABEL_HEIGHT: f32 = 60.0;
const GROUP_GAP: f32 = 28.0;
const SWATCH_GAP: f32 = 20.0;

pub(crate) fn render(canvas: &mut Canvas, content: Rect) {
    let mut y = content.y;
    for group in GROUPS {
        canvas.draw_text(
            group.title,
            Point::new(content.x, y),
            TextStyle::new(26.0, palette::INK_SOFT)
                .with_weight(0.12)
                .with_tracking(0.1),
        );
        y += TITLE_HEIGHT;

        let count = group.swatches.len() as f32;
        let width = (content.width - SWATCH_GAP * (count - 1.0)) / count;
        for (index, (name, color)) in group.swatches.iter().enumerate() {
            let swatch = Rect::new(
                content.x + index as f32 * (width + SWATCH_GAP),
                y,
                width,
                SWATCH_HEIGHT,
            );
            canvas.fill_rect(swatch, *color);
            canvas.stroke_rect(swatch, palette::HAIRLINE, 2.0);
            // No leading `#`: the stroke font (`platform/sdk/src/text.rs`)
            // has no glyph for it.
            let hex = format!("{:02X}{:02X}{:02X}", color.r, color.g, color.b);
            canvas.draw_text(
                name,
                Point::new(swatch.center().x, swatch.bottom() + 8.0),
                paper_sdk::chrome::fit_text(
                    name,
                    TextStyle::new(22.0, palette::INK).centered(),
                    width - 8.0,
                    14.0,
                ),
            );
            canvas.draw_text(
                &hex,
                Point::new(swatch.center().x, swatch.bottom() + 32.0),
                TextStyle::new(20.0, palette::INK_SOFT).centered(),
            );
        }
        y += SWATCH_HEIGHT + LABEL_HEIGHT + GROUP_GAP;
    }
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
}
