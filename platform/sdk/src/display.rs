//! Mapping the fixed canvas onto whatever surface is presenting it.

use crate::geometry::{Point, Rect, Size};

/// The screen every Paperclip app draws for: 1620 × 2160, portrait.
///
/// **Confirmed** by the WWW-1 device survey, read from the device tree the
/// vendor's own display stack reads: `display-width` = 1620,
/// `display-height` = 2160, `display-dpi` = 228. Nothing else in the codebase
/// hard-codes these numbers; changing this constant changes the target
/// everywhere, which is the point of it being a constant.
///
/// The pixel format is settled too, and it is the one this canvas already
/// produces: **1620 × 2160 ARGB8888**. An earlier reading of the DRM mode —
/// 1620 bytes × 1084 rows, therefore 4bpp with two panel rows packed per
/// buffer row — was refuted on hardware. That mode is a *proprietary packed
/// transport*, not a pixel format anybody writes into, and no third party has
/// reverse-engineered it. Presentation goes through the vendor waveform
/// engine instead (ADR-0007), so there is no packing or quantisation step and
/// none should be written.
///
/// What is still not settled is whether a Paperclip surface is legible on the
/// glass. Nothing in this repository has presented a pixel on the tablet.
pub const SCREEN: Size = Size::new(1620, 2160);

/// How canvas space sits inside a physical surface.
///
/// The canvas is scaled uniformly — never stretched — and centred, leaving
/// letterbox bars on whichever axis has spare room. On a Mac that is almost
/// always the horizontal axis, because a 3:4 portrait canvas is taller than
/// any laptop screen. On the tablet the mapping is the identity, and
/// `paper_device::present` refuses a canvas of any other size rather than
/// scaling one — a blurred UI on e-ink is worse than an error.
///
/// All physical coordinates are **physical** pixels. Window systems report
/// logical points on HiDPI displays; convert with the scale factor before
/// calling anything here. [`Self::fit_logical`] does that for you.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayMapping {
    canvas: Size,
    viewport: Rect,
    scale: f32,
}

impl DisplayMapping {
    /// Fits `canvas` inside `surface`, centred, preserving aspect ratio.
    pub fn fit(canvas: Size, surface: Size) -> Self {
        if canvas.is_empty() || surface.is_empty() {
            return Self {
                canvas,
                viewport: Rect::new(0.0, 0.0, 0.0, 0.0),
                scale: 0.0,
            };
        }
        let scale = (surface.width as f32 / canvas.width as f32)
            .min(surface.height as f32 / canvas.height as f32);
        let width = canvas.width as f32 * scale;
        let height = canvas.height as f32 * scale;
        let viewport = Rect::new(
            (surface.width as f32 - width) / 2.0,
            (surface.height as f32 - height) / 2.0,
            width,
            height,
        );
        Self {
            canvas,
            viewport,
            scale,
        }
    }

    /// Fits `canvas` inside a surface described in logical points plus a HiDPI
    /// scale factor, as window systems report it.
    pub fn fit_logical(canvas: Size, logical: (f64, f64), scale_factor: f64) -> Self {
        let physical = Size::new(
            (logical.0 * scale_factor).round().max(0.0) as u32,
            (logical.1 * scale_factor).round().max(0.0) as u32,
        );
        Self::fit(canvas, physical)
    }

    /// The canvas extent this mapping targets.
    pub fn canvas(self) -> Size {
        self.canvas
    }

    /// Where the canvas lands in the physical surface.
    pub fn viewport(self) -> Rect {
        self.viewport
    }

    /// Physical pixels per canvas pixel.
    pub fn scale(self) -> f32 {
        self.scale
    }

    /// Whether the canvas is presented one-for-one, as it should be on the device.
    pub fn is_identity(self) -> bool {
        (self.scale - 1.0).abs() < f32::EPSILON
            && self.viewport.x.abs() < f32::EPSILON
            && self.viewport.y.abs() < f32::EPSILON
    }

    /// Where a physical surface point lands on the canvas.
    ///
    /// `None` when the point is in a letterbox bar — a touch there belongs to
    /// nothing, and silently clamping it to the nearest edge would hand the
    /// app a press it never received.
    pub fn to_canvas(self, physical: Point) -> Option<Point> {
        if self.scale <= 0.0 || !self.viewport.contains(physical) {
            return None;
        }
        Some(Point::new(
            (physical.x - self.viewport.x) / self.scale,
            (physical.y - self.viewport.y) / self.scale,
        ))
    }

    /// Where a canvas point lands in the physical surface.
    pub fn to_physical(self, canvas: Point) -> Point {
        Point::new(
            self.viewport.x + canvas.x * self.scale,
            self.viewport.y + canvas.y * self.scale,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayMapping, SCREEN};
    use crate::geometry::{Point, Size};

    #[test]
    fn pillarboxes_a_portrait_canvas_in_a_landscape_window() {
        // A 3:4 canvas in a 16:10 window: bars on the left and right.
        let mapping = DisplayMapping::fit(SCREEN, Size::new(1600, 1000));
        let viewport = mapping.viewport();

        assert!((mapping.scale() - 1000.0 / 2160.0).abs() < 1e-6);
        assert!((viewport.height - 1000.0).abs() < 1e-3);
        assert!((viewport.width - 750.0).abs() < 1e-3);
        assert!((viewport.x - 425.0).abs() < 1e-3);
        assert!((viewport.y - 0.0).abs() < 1e-3);
    }

    #[test]
    fn letterboxes_when_the_window_is_narrower_than_the_canvas_ratio() {
        // A 1:2 window is narrower than 3:4, so the limit is width and the
        // bars go top and bottom.
        let mapping = DisplayMapping::fit(SCREEN, Size::new(810, 2160));
        let viewport = mapping.viewport();

        assert!((mapping.scale() - 0.5).abs() < 1e-6);
        assert!((viewport.width - 810.0).abs() < 1e-3);
        assert!((viewport.height - 1080.0).abs() < 1e-3);
        assert!((viewport.x - 0.0).abs() < 1e-3);
        assert!((viewport.y - 540.0).abs() < 1e-3);
    }

    #[test]
    fn presses_in_the_letterbox_belong_to_nobody() {
        let mapping = DisplayMapping::fit(SCREEN, Size::new(1600, 1000));
        assert_eq!(mapping.to_canvas(Point::new(10.0, 500.0)), None);
        assert_eq!(mapping.to_canvas(Point::new(1590.0, 500.0)), None);
        assert!(mapping.to_canvas(Point::new(800.0, 500.0)).is_some());
    }

    #[test]
    fn canvas_and_physical_round_trip() {
        let mapping = DisplayMapping::fit(SCREEN, Size::new(1600, 1000));
        for canvas in [
            Point::new(0.0, 0.0),
            Point::new(810.0, 1080.0),
            Point::new(1619.0, 2159.0),
        ] {
            let physical = mapping.to_physical(canvas);
            let back = mapping.to_canvas(physical).expect("inside the viewport");
            assert!((back.x - canvas.x).abs() < 0.01, "{back:?} vs {canvas:?}");
            assert!((back.y - canvas.y).abs() < 0.01, "{back:?} vs {canvas:?}");
        }
    }

    #[test]
    fn corners_map_to_the_viewport_corners() {
        let mapping = DisplayMapping::fit(SCREEN, Size::new(1600, 1000));
        let top_left = mapping.to_physical(Point::new(0.0, 0.0));
        let bottom_right =
            mapping.to_physical(Point::new(SCREEN.width as f32, SCREEN.height as f32));
        assert!((top_left.x - mapping.viewport().x).abs() < 1e-3);
        assert!((bottom_right.x - mapping.viewport().right()).abs() < 1e-3);
        assert!((bottom_right.y - mapping.viewport().bottom()).abs() < 1e-3);
    }

    #[test]
    fn hidpi_scale_factor_does_not_change_where_a_press_lands() {
        // The same window, drawn at 1x and at 2x. A press at the same logical
        // point must reach the same canvas pixel, or every touch target is
        // wrong on a Retina display.
        let one_x = DisplayMapping::fit_logical(SCREEN, (800.0, 1000.0), 1.0);
        let two_x = DisplayMapping::fit_logical(SCREEN, (800.0, 1000.0), 2.0);

        assert!((two_x.scale() - one_x.scale() * 2.0).abs() < 1e-4);

        let logical_press = Point::new(420.0, 610.0);
        let at_one_x = one_x
            .to_canvas(logical_press)
            .expect("inside the viewport at 1x");
        let at_two_x = two_x
            .to_canvas(Point::new(logical_press.x * 2.0, logical_press.y * 2.0))
            .expect("inside the viewport at 2x");

        assert!((at_one_x.x - at_two_x.x).abs() < 0.01);
        assert!((at_one_x.y - at_two_x.y).abs() < 0.01);
    }

    #[test]
    fn an_exact_fit_is_the_identity_the_device_should_see() {
        let mapping = DisplayMapping::fit(SCREEN, SCREEN);
        assert!(mapping.is_identity());
        assert_eq!(
            mapping.to_canvas(Point::new(3.0, 4.0)),
            Some(Point::new(3.0, 4.0))
        );
    }

    #[test]
    fn a_zero_sized_surface_maps_nothing_instead_of_dividing_by_zero() {
        let mapping = DisplayMapping::fit(SCREEN, Size::new(0, 0));
        assert_eq!(mapping.scale(), 0.0);
        assert_eq!(mapping.to_canvas(Point::new(0.0, 0.0)), None);
    }
}
