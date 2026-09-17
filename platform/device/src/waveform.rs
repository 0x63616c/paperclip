//! Which waveform the panel is driven with, and over what area.
//!
//! An e-ink pixel is not a value latched from a framebuffer; it is driven to a
//! state by a sequence of voltage frames. Choosing that sequence is the whole
//! of what the vendor engine does that a raw framebuffer cannot, and it is the
//! single knob with the largest effect on both latency and legibility.
//!
//! **Status of the numbers here: proposed, in the spec's sense.** The mode
//! integers come from WWW-20's reading of the vendor library and from quill's
//! published interface; nothing in this repository has watched them take
//! effect on glass. They are named constants in one place precisely so that
//! the first device session can correct them without touching a call site.

use paper_sdk::{Rect, SCREEN, Size};

/// Whether the engine should drive the panel as a monochrome or a colour
/// surface.
///
/// The panel is E Ink Gallery 3 — colour — but its fast waveforms are
/// monochrome. Live ink wants [`ContentType::Mono`]; a settled UI wants
/// [`ContentType::Color`]. Driving colour content through a mono waveform
/// degrades it to greyscale rather than failing, which makes a wrong choice
/// here quiet rather than loud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ContentType {
    /// Monochrome content, driven by a monochrome waveform.
    Mono = 0,
    /// Colour content.
    Color = 1,
}

/// A waveform selection: a content type and the engine's mode number.
///
/// Constructed through the named constants below in normal code.
/// [`Waveform::custom`] exists because the mode space is the vendor's, not
/// ours, and a device session must be able to try a number this file has never
/// heard of without a code change becoming a prerequisite for an experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Waveform {
    content: ContentType,
    mode: i32,
}

impl Waveform {
    /// Live ink: the fastest monochrome waveform. Mode 0.
    pub const INK: Self = Self {
        content: ContentType::Mono,
        mode: 0,
    };

    /// Settled monochrome: slower, cleaner, less residue. Mode 3.
    pub const MONO_QUALITY: Self = Self {
        content: ContentType::Mono,
        mode: 3,
    };

    /// Colour UI. Mode 4 of the 3/4/5 colour set.
    pub const COLOR: Self = Self {
        content: ContentType::Color,
        mode: 4,
    };

    /// A mode this file does not name, for a device session to try.
    pub const fn custom(content: ContentType, mode: i32) -> Self {
        Self { content, mode }
    }

    /// What kind of content the engine is being told it has.
    pub const fn content(self) -> ContentType {
        self.content
    }

    /// The engine's mode number.
    pub const fn mode(self) -> i32 {
        self.mode
    }
}

/// Whether a swap is a partial update or a whole-panel refresh.
///
/// Default to [`Refresh::Partial`]. The vendor backend escalates a small
/// rectangle to a full-panel refresh whenever the full flag is set, so a
/// carelessly full update turns a fifty-millisecond ink stroke into a
/// whole-screen flash (WWW-20, ADR-0007).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Refresh {
    /// Update only the rectangle given. `full = 0`.
    #[default]
    Partial,
    /// Refresh the whole panel regardless of the rectangle. `full = 1`.
    Full,
}

impl Refresh {
    /// The `full` flag the C ABI takes.
    pub const fn full_flag(self) -> i32 {
        match self {
            Self::Partial => 0,
            Self::Full => 1,
        }
    }
}

/// How aggressively the engine should suppress ghosting.
///
/// `EPFramebuffer::ghostControl` exists, which means residue is the engine's
/// concern and not something to hand-roll on top of it. The variants mirror
/// the vendor's `GhostControlMode`; like the waveform numbers they are
/// proposed until a device session says otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum GhostControl {
    /// Leave it to the engine's default policy.
    #[default]
    Auto = 0,
    /// Suppress ghost handling — fastest, dirtiest.
    Off = 1,
    /// Full ghost handling.
    Full = 2,
}

/// An integer rectangle in panel pixels.
///
/// The SDK's [`Rect`] is `f32` because it is a drawing rectangle. A swap
/// rectangle is neither: it addresses whole pixels, and a swap that misses a
/// pixel leaves a stale one on the glass. [`PixelRect::enclosing`] therefore
/// rounds *outward* — never to nearest — and clamps to the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelRect {
    /// Left edge, in panel pixels.
    pub x: u32,
    /// Top edge, in panel pixels.
    pub y: u32,
    /// Width, in panel pixels. Zero means the rectangle is empty.
    pub width: u32,
    /// Height, in panel pixels. Zero means the rectangle is empty.
    pub height: u32,
}

impl PixelRect {
    /// The whole panel.
    pub const PANEL: Self = Self {
        x: 0,
        y: 0,
        width: SCREEN.width,
        height: SCREEN.height,
    };

    /// A rectangle, unchecked against any particular panel.
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The whole of a surface of `size`.
    pub const fn whole(size: Size) -> Self {
        Self::new(0, 0, size.width, size.height)
    }

    /// Whether this rectangle covers no pixels.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The smallest pixel rectangle containing `rect`, clamped to `bounds`.
    ///
    /// Rounds outward on every edge. A drawing rectangle that covers half of
    /// pixel 10 has changed pixel 10, and a swap that excludes it leaves the
    /// old ink there — on e-ink, visibly and until something else repaints.
    pub fn enclosing(rect: Rect, bounds: Size) -> Self {
        if bounds.is_empty() || rect.width <= 0.0 || rect.height <= 0.0 {
            return Self::new(0, 0, 0, 0);
        }
        let left = rect.x.floor().max(0.0) as u32;
        let top = rect.y.floor().max(0.0) as u32;
        let right = (rect.x + rect.width).ceil().max(0.0) as u32;
        let bottom = (rect.y + rect.height).ceil().max(0.0) as u32;

        let left = left.min(bounds.width);
        let top = top.min(bounds.height);
        let right = right.min(bounds.width);
        let bottom = bottom.min(bounds.height);

        Self::new(
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        )
    }

    /// The smallest rectangle containing both, for accumulating damage between
    /// swaps.
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Self::new(left, top, right - left, bottom - top)
    }

    /// Whether this rectangle fits inside a surface of `size`.
    ///
    /// Saturating, so a nonsense rectangle answers `false` rather than
    /// overflowing the check that was meant to catch it.
    pub const fn fits_in(self, size: Size) -> bool {
        self.x.saturating_add(self.width) <= size.width
            && self.y.saturating_add(self.height) <= size.height
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentType, GhostControl, PixelRect, Refresh, Waveform};
    use paper_sdk::{Rect, SCREEN, Size};

    #[test]
    fn partial_is_the_default_because_full_escalates_to_the_whole_panel() {
        assert_eq!(Refresh::default(), Refresh::Partial);
        assert_eq!(Refresh::Partial.full_flag(), 0);
        assert_eq!(Refresh::Full.full_flag(), 1);
    }

    #[test]
    fn the_named_waveforms_are_the_ones_the_notes_describe() {
        assert_eq!(Waveform::INK.content(), ContentType::Mono);
        assert_eq!(Waveform::INK.mode(), 0);
        assert_eq!(Waveform::MONO_QUALITY.mode(), 3);
        assert_eq!(Waveform::COLOR.content(), ContentType::Color);
        assert_eq!(Waveform::COLOR.mode(), 4);
        assert_eq!(Waveform::custom(ContentType::Color, 5).mode(), 5);
    }

    #[test]
    fn ghost_control_defaults_to_leaving_it_to_the_engine() {
        assert_eq!(GhostControl::default(), GhostControl::Auto);
    }

    #[test]
    fn enclosing_rounds_outward_so_a_half_covered_pixel_is_still_swapped() {
        // x spans 10.4..15.5, so pixels 10..=15 — six of them, not five.
        let rect = Rect::new(10.4, 20.6, 5.1, 5.1);
        let pixels = PixelRect::enclosing(rect, SCREEN);
        assert_eq!(pixels, PixelRect::new(10, 20, 6, 6));

        // A rectangle that ends exactly on a pixel boundary must not gain one.
        let exact = PixelRect::enclosing(Rect::new(10.0, 20.0, 5.0, 5.0), SCREEN);
        assert_eq!(exact, PixelRect::new(10, 20, 5, 5));

        // A sub-pixel rectangle still covers the pixel it sits in.
        let sliver = PixelRect::enclosing(Rect::new(10.2, 20.2, 0.1, 0.1), SCREEN);
        assert_eq!(sliver, PixelRect::new(10, 20, 1, 1));
    }

    #[test]
    fn enclosing_clamps_to_the_panel_instead_of_addressing_past_it() {
        let rect = Rect::new(-50.0, -50.0, 5000.0, 5000.0);
        let pixels = PixelRect::enclosing(rect, SCREEN);
        assert_eq!(pixels, PixelRect::PANEL);
        assert!(pixels.fits_in(SCREEN));
    }

    #[test]
    fn a_rectangle_entirely_off_the_panel_is_empty_not_wrapped() {
        let pixels = PixelRect::enclosing(Rect::new(9000.0, 9000.0, 10.0, 10.0), SCREEN);
        assert!(pixels.is_empty());
        assert!(pixels.fits_in(SCREEN));
    }

    #[test]
    fn degenerate_rectangles_and_surfaces_produce_nothing() {
        assert!(PixelRect::enclosing(Rect::new(0.0, 0.0, 0.0, 10.0), SCREEN).is_empty());
        assert!(PixelRect::enclosing(Rect::new(0.0, 0.0, 10.0, 10.0), Size::new(0, 0)).is_empty());
    }

    #[test]
    fn union_accumulates_damage_and_ignores_empty_rectangles() {
        let a = PixelRect::new(10, 10, 10, 10);
        let b = PixelRect::new(100, 5, 10, 10);
        assert_eq!(a.union(b), PixelRect::new(10, 5, 100, 15));
        assert_eq!(a.union(PixelRect::new(0, 0, 0, 0)), a);
        assert_eq!(PixelRect::new(0, 0, 0, 0).union(b), b);
    }
}
