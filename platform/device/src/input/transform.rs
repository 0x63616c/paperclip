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
//! so one calibration answers both. The scales below are the ones WWW-3 was
//! handed as settled:
//!
//! ```text
//! touch:  x * 1620 / 2064      y * 2160 / 2832
//! pen:    x * 1620 / 11180     y * 2160 / 15340
//! ```
//!
//! ## The offset, and why it is a field rather than a zero
//!
//! Neither digitizer space matches the panel's aspect ratio — 0.729 against
//! 0.750 — so the active sensing area is taller than the glass and a pure
//! scale cannot be exactly right everywhere. The scales above are what this
//! stage was told to implement, and they are the defaults. The offset exists,
//! zeroed, so that the measured correction WWW-21 produces is a number to set
//! rather than a type to redesign.
//!
//! WWW-20's second calibration attempt narrows how large that correction can
//! be: real touches reached x=33 and x=2003 against an axis maximum of 2064,
//! and y=0 and y=2726 against 2832. So the reported range is physically
//! reachable and there is no large dead margin between digitizer and glass —
//! which is why a zeroed offset is a reasonable default rather than a
//! placeholder. It is still not a measured transform: nine contacts whose
//! intended targets are unknown do not determine one.
//!
//! [`PointerTransform::mirror_y`] is likewise present and off: the single
//! orientation check is deferred to WWW-21, and a flag defaulted to the
//! unmirrored reading is the honest way to carry an unanswered question.

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
    offset: Point,
    mirror_y: bool,
}

impl PointerTransform {
    /// The touchscreen transform, unmirrored and unoffset.
    pub const fn touch() -> Self {
        Self::new(TOUCH_EXTENT, SCREEN)
    }

    /// The pen transform, unmirrored and unoffset.
    pub const fn pen() -> Self {
        Self::new(PEN_EXTENT, SCREEN)
    }

    /// A transform from an arbitrary digitizer extent onto an arbitrary panel.
    pub const fn new(extent: Size, panel: Size) -> Self {
        Self {
            extent,
            panel,
            offset: Point::new(0.0, 0.0),
            mirror_y: false,
        }
    }

    /// The same transform with a measured offset in panel pixels, applied
    /// after scaling.
    pub const fn with_offset(mut self, offset: Point) -> Self {
        self.offset = offset;
        self
    }

    /// The same transform with the vertical axis flipped.
    ///
    /// Off by default and deliberately not exercised: nothing has checked the
    /// tablet's orientation against rendered content yet (WWW-21).
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
    /// The result may fall outside the panel, and that is not an error: the
    /// sensing area is taller than the glass, so a touch on the bezel is a
    /// real report at a real coordinate that is not on screen. Deciding what
    /// to do about it is [`Self::map_on_panel`]'s, or the caller's.
    pub fn map(self, raw_x: i32, raw_y: i32) -> Point {
        if self.extent.is_empty() {
            return Point::new(0.0, 0.0);
        }
        let x = raw_x as f32 * self.panel.width as f32 / self.extent.width as f32 + self.offset.x;
        let y = raw_y as f32 * self.panel.height as f32 / self.extent.height as f32 + self.offset.y;
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
    fn an_offset_moves_the_result_in_panel_pixels() {
        let shifted = PointerTransform::touch().with_offset(Point::new(-10.0, 25.0));
        let plain = PointerTransform::touch().map(1000, 1000);
        let moved = shifted.map(1000, 1000);
        assert!(close(moved.x, plain.x - 10.0), "{moved:?}");
        assert!(close(moved.y, plain.y + 25.0), "{moved:?}");
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
