//! Digitizer coordinates to panel coordinates.
//!
//! The pen and the touchscreen report into two different integer spaces, and
//! neither of them is the panel. WWW-20 established that the two are the *same*
//! physical space at a fixed 65/12 ratio:
//!
//! ```text
//! 11180 / 2064 = 5.4166…       15340 / 2832 = 5.4166…       (both 65/12)
//! ```
//!
//! so one transform answers both. These scales are a hardware constant, not a
//! per-device calibration (ADR-0010):
//!
//! ```text
//! touch:  x * 1620 / 2064      y * 2160 / 2832
//! pen:    x * 1620 / 11180     y * 2160 / 15340
//! ```
//!
//! No axis swap and no inversion on the stock firmware path. Corroborated by
//! rmweb's input research and KOReader's verified device table, which give
//! these ratios independently of the 65/12 derivation above.
//!
//! ## The offset is not expected to be needed
//!
//! The digitizer aspect (0.729) differs from the panel's (0.750), but that is
//! absorbed by the two axes carrying *different* scale factors — which is
//! exactly what the formulas express. An earlier reading of this crate's
//! history claimed the sensing area is taller than the glass and therefore
//! needs a measured offset; that claim is withdrawn.
//!
//! `with_offset` **has been removed.** It was left in place for WWW-3 to decide
//! on, and with the claim that motivated it withdrawn it was a field with no
//! caller and no open question behind it — which is the speculative
//! scaffolding §7 forbids. Restoring it is three lines if the device ever
//! disagrees with the formulas above.
//!
//! [`PointerTransform::mirror_y`] stays, off by default, for a real open
//! question: some firmware reportedly inverts Y on a mainline-kernel input
//! path. One tap on a top-left mark rules it out, deferred to WWW-21.

use paper_sdk::{Point, SCREEN, Size};

/// The touchscreen's reported extent: 2064 × 2832.
pub const TOUCH_EXTENT: Size = Size::new(2064, 2832);
/// The pen digitizer's reported extent: 11180 × 15340.
pub const PEN_EXTENT: Size = Size::new(11180, 15340);

/// Maps one digitizer's raw coordinates onto the panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerTransform {
    extent: Size,
    panel: Size,
    mirror_y: bool,
}

impl PointerTransform {
    /// The touchscreen transform, unmirrored.
    pub const fn touch() -> Self {
        Self::new(TOUCH_EXTENT, SCREEN)
    }

    /// The pen transform, unmirrored.
    pub const fn pen() -> Self {
        Self::new(PEN_EXTENT, SCREEN)
    }

    /// A transform from an arbitrary digitizer extent onto an arbitrary panel.
    pub const fn new(extent: Size, panel: Size) -> Self {
        Self {
            extent,
            panel,
            mirror_y: false,
        }
    }

    /// The same transform with the vertical axis flipped.
    ///
    /// Off by default: the stock firmware path has no inversion, but some
    /// firmware reportedly inverts Y on a mainline-kernel input path. One tap
    /// on a top-left mark settles it (WWW-21).
    pub const fn mirror_y(mut self, mirrored: bool) -> Self {
        self.mirror_y = mirrored;
        self
    }

    /// The digitizer extent this transform reads from.
    pub const fn extent(self) -> Size {
        self.extent
    }

    /// The panel extent it writes to.
    pub const fn panel(self) -> Size {
        self.panel
    }

    /// Maps a raw report to panel coordinates.
    ///
    /// The result may fall outside the panel, and that is not an error: a
    /// report can legitimately land on the bezel, and the digitizer reports it
    /// at a real coordinate that is not on screen. Deciding what to do about
    /// it is [`Self::map_on_panel`]'s, or the caller's.
    pub fn map(self, raw_x: i32, raw_y: i32) -> Point {
        if self.extent.is_empty() {
            return Point::new(0.0, 0.0);
        }
        let x = raw_x as f32 * self.panel.width as f32 / self.extent.width as f32;
        let y = raw_y as f32 * self.panel.height as f32 / self.extent.height as f32;
        let y = if self.mirror_y {
            self.panel.height as f32 - y
        } else {
            y
        };
        Point::new(x, y)
    }

    /// Maps a raw report, or `None` when it does not land on the glass.
    ///
    /// `None` rather than a clamp, for the same reason
    /// [`paper_sdk::DisplayMapping::to_canvas`] returns `None` in the
    /// letterbox: clamping hands an app a press at a coordinate nobody
    /// touched.
    pub fn map_on_panel(self, raw_x: i32, raw_y: i32) -> Option<Point> {
        let point = self.map(raw_x, raw_y);
        let on_panel = point.x >= 0.0
            && point.y >= 0.0
            && point.x < self.panel.width as f32
            && point.y < self.panel.height as f32;
        on_panel.then_some(point)
    }
}

#[cfg(test)]
mod tests {
    use super::{PEN_EXTENT, PointerTransform, TOUCH_EXTENT};
    use paper_sdk::{Point, SCREEN, Size};

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn the_scales_are_the_ones_this_stage_was_handed() {
        let touch = PointerTransform::touch();
        let mapped = touch.map(1032, 1416);
        assert!(close(mapped.x, 1032.0 * 1620.0 / 2064.0), "{mapped:?}");
        assert!(close(mapped.y, 1416.0 * 2160.0 / 2832.0), "{mapped:?}");

        let pen = PointerTransform::pen();
        let mapped = pen.map(5590, 7670);
        assert!(close(mapped.x, 5590.0 * 1620.0 / 11180.0), "{mapped:?}");
        assert!(close(mapped.y, 7670.0 * 2160.0 / 15340.0), "{mapped:?}");
    }

    #[test]
    fn the_origin_maps_to_the_origin_and_the_far_corner_to_the_far_corner() {
        for transform in [PointerTransform::touch(), PointerTransform::pen()] {
            let origin = transform.map(0, 0);
            assert!(close(origin.x, 0.0) && close(origin.y, 0.0), "{origin:?}");

            let extent = transform.extent();
            let corner = transform.map(extent.width as i32, extent.height as i32);
            assert!(close(corner.x, SCREEN.width as f32), "{corner:?}");
            assert!(close(corner.y, SCREEN.height as f32), "{corner:?}");
        }
    }

    #[test]
    fn pen_and_touch_are_the_same_physical_space_at_65_over_12() {
        // The property WWW-20 established. If a firmware update changes one
        // extent and not the other, this fails rather than quietly skewing
        // every pen coordinate against every touch coordinate.
        let ratio_x = PEN_EXTENT.width as f64 / TOUCH_EXTENT.width as f64;
        let ratio_y = PEN_EXTENT.height as f64 / TOUCH_EXTENT.height as f64;
        assert!((ratio_x - 65.0 / 12.0).abs() < 1e-9, "{ratio_x}");
        assert!((ratio_y - 65.0 / 12.0).abs() < 1e-9, "{ratio_y}");

        // And so the same physical point maps to the same panel pixel. The
        // raw touch coordinates are multiples of 12 so that scaling by 65/12
        // stays exact in integers — the digitizers report integers, and a
        // rounding artefact here would hide a real skew.
        let touch = PointerTransform::touch().map(480, 720);
        let pen = PointerTransform::pen().map(480 * 65 / 12, 720 * 65 / 12);
        assert!(
            close(touch.x, pen.x) && close(touch.y, pen.y),
            "{touch:?} {pen:?}"
        );
    }

    #[test]
    fn mirroring_is_off_by_default_and_flips_about_the_panel_centre_when_on() {
        let plain = PointerTransform::touch();
        let mirrored = PointerTransform::touch().mirror_y(true);

        let raw_y = 708; // a quarter of the way down the digitizer
        assert!(close(plain.map(0, raw_y).y, 540.0));
        assert!(close(mirrored.map(0, raw_y).y, 2160.0 - 540.0));
        // x is untouched either way.
        assert!(close(mirrored.map(1032, raw_y).x, plain.map(1032, raw_y).x));
    }

    #[test]
    fn a_report_off_the_glass_is_none_rather_than_clamped_to_the_edge() {
        let touch = PointerTransform::touch();
        assert_eq!(touch.map_on_panel(-5, 100), None);
        assert_eq!(touch.map_on_panel(100, -5), None);
        // The far corner is exactly SCREEN, which is one past the last pixel.
        assert_eq!(touch.map_on_panel(2064, 2832), None);
        assert!(touch.map_on_panel(1032, 1416).is_some());
    }

    #[test]
    fn a_degenerate_extent_maps_to_the_origin_rather_than_dividing_by_zero() {
        let broken = PointerTransform::new(Size::new(0, 0), SCREEN);
        assert_eq!(broken.map(100, 100), Point::new(0.0, 0.0));
        assert_eq!(broken.map_on_panel(100, 100), Some(Point::new(0.0, 0.0)));
    }
}
