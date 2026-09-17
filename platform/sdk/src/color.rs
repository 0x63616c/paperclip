//! Colour, and the deliberately small palette Stage 1 draws with.

/// An opaque-by-default 8-bit sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel; `255` is opaque.
    pub a: u8,
}

impl Color {
    /// An opaque colour.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// An opaque grey.
    pub const fn grey(value: u8) -> Self {
        Self::rgb(value, value, value)
    }

    /// This colour at a different opacity.
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Packed `0x00RRGGBB`, the layout `softbuffer` presents.
    pub const fn to_argb(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Relative luminance, used to check contrast in tests.
    pub fn luminance(self) -> f32 {
        let channel = |value: u8| {
            let v = value as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    /// WCAG contrast ratio against `other`, between `1.0` and `21.0`.
    pub fn contrast_ratio(self, other: Color) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
        (lighter + 0.05) / (darker + 0.05)
    }

    pub(crate) fn to_tiny_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }
}

/// The Stage 1 palette.
///
/// Greyscale on purpose. The Paper Pro's colour behaviour is a WWW-1 hardware
/// gate, and a UI that only reads correctly in colour would be a bet on the
/// answer. Every value here is chosen so the screens stay legible if the panel
/// turns out to render them as plain greys — which, on e-ink, is the outcome
/// to design for first. See `docs/assumptions.md`.
pub mod palette {
    use super::Color;

    /// Background. Off-white rather than pure white, which is closer to how
    /// e-ink actually looks and keeps the paper metaphor honest on a Mac.
    pub const PAPER: Color = Color::rgb(0xF7, 0xF6, 0xF2);
    /// Primary foreground: text, borders, dark pieces.
    pub const INK: Color = Color::rgb(0x16, 0x16, 0x18);
    /// Secondary text. Still 7:1 against [`PAPER`].
    pub const INK_SOFT: Color = Color::rgb(0x4B, 0x4B, 0x4F);
    /// Tertiary text and disabled states.
    pub const INK_FAINT: Color = Color::rgb(0x8A, 0x8A, 0x8D);
    /// Separator rules.
    pub const HAIRLINE: Color = Color::rgb(0xC4, 0xC3, 0xBE);
    /// Home shelf tile fill.
    pub const TILE: Color = Color::rgb(0xEC, 0xEB, 0xE6);
    /// Light chess squares.
    pub const BOARD_LIGHT: Color = Color::rgb(0xE4, 0xE2, 0xDB);
    /// Dark chess squares.
    ///
    /// Sits at the luminance that balances contrast against [`INK`] and
    /// [`PAPER`] — roughly 4:1 in both directions. No single mid grey can be
    /// 4.5:1 from both ends at once (the product would need more range than
    /// the scale has), which is exactly why every real piece set outlines its
    /// pieces in the opposite colour instead of relying on square contrast.
    /// Paperclip does the same.
    pub const BOARD_DARK: Color = Color::rgb(0x79, 0x77, 0x71);
    /// Emphasis fill, for the one thing on a screen that should be loudest.
    pub const EMPHASIS: Color = Color::rgb(0x2E, 0x2E, 0x32);
    /// Letterbox bars around the canvas in the desktop preview. Never part of
    /// a screen's own design — it exists only to make the canvas edge obvious.
    pub const LETTERBOX: Color = Color::rgb(0x2A, 0x2A, 0x2C);
}

#[cfg(test)]
mod tests {
    use super::{Color, palette};

    #[test]
    fn packs_to_argb_for_the_window_buffer() {
        assert_eq!(Color::rgb(0x12, 0x34, 0x56).to_argb(), 0x0012_3456);
        assert_eq!(Color::grey(0xFF).to_argb(), 0x00FF_FFFF);
    }

    #[test]
    fn body_text_clears_the_contrast_bar_for_e_ink() {
        // 7:1 is WCAG AAA for body text. On a reflective display it is the
        // floor, not the target.
        assert!(palette::INK.contrast_ratio(palette::PAPER) > 14.0);
        assert!(palette::INK_SOFT.contrast_ratio(palette::PAPER) > 7.0);
        assert!(palette::INK.contrast_ratio(palette::TILE) > 12.0);
        // A piece must stay visible on either square colour. The dark square
        // is deliberately balanced rather than pushed to one end; the piece
        // outline carries the rest.
        assert!(palette::INK.contrast_ratio(palette::BOARD_DARK) > 3.8);
        assert!(palette::PAPER.contrast_ratio(palette::BOARD_DARK) > 3.8);
        assert!(palette::INK.contrast_ratio(palette::BOARD_LIGHT) > 12.0);
        assert!(palette::BOARD_LIGHT.contrast_ratio(palette::BOARD_DARK) > 2.5);
    }

    #[test]
    fn contrast_is_symmetric_and_bounded() {
        let ratio = palette::INK.contrast_ratio(palette::PAPER);
        assert!((ratio - palette::PAPER.contrast_ratio(palette::INK)).abs() < 1e-5);
        assert!((1.0..=21.0).contains(&ratio));
        assert!((palette::INK.contrast_ratio(palette::INK) - 1.0).abs() < 1e-5);
    }
}
