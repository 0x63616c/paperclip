//! Which waveform the panel is driven with, and over what area.
//!
//! An e-ink pixel is not a value latched from a framebuffer; it is driven to a
//! state by a sequence of voltage frames. Choosing that sequence is the whole
//! of what the vendor engine does that a raw framebuffer cannot, and it is the
//! single knob with the largest effect on both latency and legibility.
//!
//! **Status of the mode numbers: measured.** WWW-35's device session
//! confirmed the vendor's own mode enum against the 3.28 ABI (two
//! independent reverse-engineering efforts agree) and measured `sync()`
//! completion for each: `Pen` 372ms, `Mono` 634ms, `Animation` 304ms
//! (fastest), `Ui` 630ms, `Content` 1086ms (slowest). What WWW-35 corrected
//! was the *names* this file gave modes 3 and 4 — `MONO_QUALITY` and
//! `COLOR` — which described what this file used them for rather than what
//! the vendor calls them, and so read as an implementation detail (a
//! mono-specific quality table) that the vendor's own naming denies.
//!
//! Named constants stay in one place so a future device session can correct
//! a mode number, or reach for [`EngineMode::Animation`] — measured fastest,
//! and not yet wired into any [`Waveform`] constant — without touching a
//! call site.

use paper_sdk::{Rect, SCREEN, Size};

/// The vendor engine's own waveform-mode enum, as `EPScreenMode` — not a
/// name this repository invented. Established against the 3.28 ABI by two
/// independent reverse-engineering efforts (WWW-35); see the project's
/// `remarkable-device-session` skill for the citations.
///
/// This is *not* the waveform-file index `swapBuffers_impl` ultimately
/// selects — `EPScreenMode` is the argument the engine's public API takes,
/// and the engine picks the underlying waveform-file table internally
/// (ADR-0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum EngineMode {
    /// Live pen ink. Measured `sync()`: 372ms.
    Pen = 0,
    /// Generic monochrome, distinct from [`Self::Ui`]. Measured `sync()`:
    /// 634ms.
    Mono = 1,
    /// The fastest mode measured — 304ms — and not yet used by any
    /// [`Waveform`] constant.
    Animation = 2,
    /// Settled UI. What this file called `MONO_QUALITY` before WWW-35:
    /// nothing about it is mono-specific, and driving mono content through
    /// it is this crate's choice, not the vendor's constraint. Measured
    /// `sync()`: 630ms.
    Ui = 3,
    /// Settled colour content. Measured `sync()`: 1086ms, the slowest.
    Content = 4,
    /// The engine's sleep/screensaver mode. Not used by this crate.
    Sleep = 5,
}

impl EngineMode {
    /// The `EPScreenMode` integer the C ABI takes.
    pub const fn mode(self) -> i32 {
        self as i32
    }
}

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
    /// Live ink: the fastest monochrome waveform in current use.
    /// [`EngineMode::Pen`].
    pub const INK: Self = Self {
        content: ContentType::Mono,
        mode: EngineMode::Pen.mode(),
    };

    /// The vendor's fastest mode of all, measured 304ms — not yet driven by
    /// any call site. [`EngineMode::Animation`], paired with
    /// [`ContentType::Mono`] as the closer match until a device session
    /// exercises it and says otherwise.
    pub const ANIMATION: Self = Self {
        content: ContentType::Mono,
        mode: EngineMode::Animation.mode(),
    };

    /// Settled UI: slower, cleaner, less residue. [`EngineMode::Ui`] — named
    /// `MONO_QUALITY` before WWW-35 corrected it; see the module docs.
    pub const UI: Self = Self {
        content: ContentType::Mono,
        mode: EngineMode::Ui.mode(),
    };

    /// Settled colour content. [`EngineMode::Content`] — named `COLOR`
    /// before WWW-35 corrected it; see the module docs.
    pub const CONTENT: Self = Self {
        content: ContentType::Color,
        mode: EngineMode::Content.mode(),
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
    use super::{ContentType, EngineMode, GhostControl, PixelRect, Refresh, Waveform};
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
        assert_eq!(Waveform::ANIMATION.mode(), 2);
        assert_eq!(Waveform::UI.mode(), 3);
        assert_eq!(Waveform::CONTENT.content(), ContentType::Color);
        assert_eq!(Waveform::CONTENT.mode(), 4);
        assert_eq!(Waveform::custom(ContentType::Color, 5).mode(), 5);
    }

    #[test]
    fn the_engine_mode_integers_match_the_vendor_enum_www_35_confirmed() {
        assert_eq!(EngineMode::Pen.mode(), 0);
        assert_eq!(EngineMode::Mono.mode(), 1);
        assert_eq!(EngineMode::Animation.mode(), 2);
        assert_eq!(EngineMode::Ui.mode(), 3);
        assert_eq!(EngineMode::Content.mode(), 4);
        assert_eq!(EngineMode::Sleep.mode(), 5);
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
