//! The surface every screen is drawn on.

use std::fs;
use std::io;
use std::path::Path;

use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Shader, Stroke, Transform,
};

use crate::color::{Color, palette};
use crate::display::DisplayMapping;
use crate::text::{self, TextStyle};
use paper_protocol::{Point, Rect, Size};

/// A CPU-rasterised drawing surface in canvas space.
///
/// CPU on purpose: an e-ink panel is fed a framebuffer, and a software
/// rasteriser produces one on the Mac and on the device from identical code
/// with no GPU, no driver and no windowing system in the way. See ADR-0002.
#[derive(Debug, Clone)]
pub struct Canvas {
    pixmap: Pixmap,
}

impl Canvas {
    /// A new canvas filled with [`palette::PAPER`].
    ///
    /// Returns `None` for a zero or absurdly large size, which is the only way
    /// allocation fails here.
    pub fn new(size: Size) -> Option<Self> {
        let mut pixmap = Pixmap::new(size.width, size.height)?;
        pixmap.fill(palette::PAPER.to_tiny_skia());
        Some(Self { pixmap })
    }

    /// The canvas extent.
    pub fn size(&self) -> Size {
        Size::new(self.pixmap.width(), self.pixmap.height())
    }

    /// The whole canvas as a rectangle.
    pub fn bounds(&self) -> Rect {
        let size = self.size();
        Rect::new(0.0, 0.0, size.width as f32, size.height as f32)
    }

    /// Paints the entire canvas one colour.
    pub fn clear(&mut self, color: Color) {
        self.pixmap.fill(color.to_tiny_skia());
    }

    /// Fills a rectangle.
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        let Some(rect) = to_skia_rect(rect) else {
            return;
        };
        self.pixmap
            .fill_rect(rect, &paint(color), Transform::identity(), None);
    }

    /// Strokes a rectangle outline, centred on its edges.
    pub fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32) {
        let Some(rect) = to_skia_rect(rect) else {
            return;
        };
        let path = PathBuilder::from_rect(rect);
        self.pixmap.stroke_path(
            &path,
            &paint(color),
            &stroke(width),
            Transform::identity(),
            None,
        );
    }

    /// Fills a rectangle with rounded corners.
    pub fn fill_round_rect(&mut self, rect: Rect, radius: f32, color: Color) {
        let Some(path) = round_rect_path(rect, radius) else {
            return;
        };
        self.pixmap.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    /// Strokes a rounded rectangle outline.
    pub fn stroke_round_rect(&mut self, rect: Rect, radius: f32, color: Color, width: f32) {
        let Some(path) = round_rect_path(rect, radius) else {
            return;
        };
        self.pixmap.stroke_path(
            &path,
            &paint(color),
            &stroke(width),
            Transform::identity(),
            None,
        );
    }

    /// Fills a circle.
    pub fn fill_circle(&mut self, center: Point, radius: f32, color: Color) {
        let mut builder = PathBuilder::new();
        builder.push_circle(center.x, center.y, radius);
        let Some(path) = builder.finish() else {
            return;
        };
        self.pixmap.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    /// Fills a closed polygon.
    pub fn fill_polygon(&mut self, points: &[Point], color: Color) {
        let Some(path) = polygon_path(points, true) else {
            return;
        };
        self.pixmap.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    /// Strokes an open polyline with round caps and joins.
    pub fn stroke_polyline(&mut self, points: &[Point], color: Color, width: f32) {
        let Some(path) = polygon_path(points, false) else {
            return;
        };
        self.pixmap.stroke_path(
            &path,
            &paint(color),
            &stroke(width),
            Transform::identity(),
            None,
        );
    }

    /// Draws a horizontal rule across `length`, two canvas pixels thick.
    ///
    /// Two rather than one: at 228 dpi a single-pixel rule is close to
    /// invisible, and the preview scales down far enough that box filtering
    /// would grey it away entirely.
    pub fn hairline(&mut self, from: Point, length: f32, color: Color) {
        self.fill_rect(Rect::new(from.x, from.y, length, 2.0), color);
    }

    /// Draws a run of text, anchored per [`TextStyle::align`].
    ///
    /// `at` is the top of the glyph box, not a baseline.
    pub fn draw_text(&mut self, content: &str, at: Point, style: TextStyle) {
        let unit = text::unit_scale(style);
        let origin_x = at.x + text::align_offset(content, style);
        let width = style.stroke_width();

        for (pen, glyph) in text::layout(content, style) {
            for polyline in glyph {
                let points: Vec<Point> = polyline
                    .iter()
                    .map(|&(x, y)| Point::new(origin_x + pen + x * unit, at.y + y * unit))
                    .collect();
                if points.len() == 1 {
                    self.fill_circle(points[0], width / 2.0, style.color);
                } else {
                    self.stroke_polyline(&points, style.color, width);
                }
            }
        }
    }

    /// Fraction of pixels that are not the background colour.
    ///
    /// Cheap evidence for tests: a screen that renders nothing scores zero,
    /// and a screen that has gone solid black scores one. Both are bugs a
    /// screenshot would catch by eye and a test otherwise would not.
    pub fn ink_coverage(&self) -> f32 {
        let background = palette::PAPER;
        let total = self.pixmap.pixels().len();
        if total == 0 {
            return 0.0;
        }
        let inked = self
            .pixmap
            .pixels()
            .iter()
            .filter(|pixel| {
                let pixel = pixel.demultiply();
                pixel.red() != background.r
                    || pixel.green() != background.g
                    || pixel.blue() != background.b
            })
            .count();
        inked as f32 / total as f32
    }

    /// The colour at a canvas pixel, or `None` outside the canvas.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Color> {
        // Bounds-checked here rather than left to the rasteriser: a stray `x`
        // past the right edge is a valid index into the next row, so an
        // unchecked read answers a question nobody asked.
        let size = self.size();
        if x >= size.width || y >= size.height {
            return None;
        }
        let pixel = self.pixmap.pixel(x, y)?.demultiply();
        Some(Color::rgb(pixel.red(), pixel.green(), pixel.blue()))
    }

    /// Presents this canvas into a larger surface through `mapping`, filling
    /// the letterbox with [`palette::LETTERBOX`].
    ///
    /// Box-filtered rather than nearest-neighbour: the preview is normally
    /// scaled well below 1:1 on a laptop screen, and point sampling a stroke
    /// font at 0.35× drops whole strokes, which would make the preview lie
    /// about legibility.
    pub fn present_into(&self, surface: &mut Canvas, mapping: DisplayMapping) {
        surface.clear(palette::LETTERBOX);
        let viewport = mapping.viewport();
        let scale = mapping.scale();
        if scale <= 0.0 {
            return;
        }

        let source = self.size();
        let target = surface.size();
        let step = 1.0 / scale;

        let x_start = viewport.x.round().max(0.0) as u32;
        let y_start = viewport.y.round().max(0.0) as u32;
        let x_end = (viewport.right().round().max(0.0) as u32).min(target.width);
        let y_end = (viewport.bottom().round().max(0.0) as u32).min(target.height);

        let stride = target.width;
        for y in y_start..y_end {
            let sy0 = ((y as f32 - viewport.y) * step).max(0.0);
            let sy1 = (sy0 + step).min(source.height as f32);
            for x in x_start..x_end {
                let sx0 = ((x as f32 - viewport.x) * step).max(0.0);
                let sx1 = (sx0 + step).min(source.width as f32);
                let color = self.box_sample(sx0, sy0, sx1, sy1);
                // Written straight into the buffer: a per-pixel `fill_rect`
                // would rasterise 600k paths a frame and make the preview feel
                // like the renderer is the bottleneck when it is not.
                let index = (y * stride + x) as usize;
                if let Some(slot) = surface.pixmap.pixels_mut().get_mut(index)
                    && let Some(premultiplied) =
                        tiny_skia::PremultipliedColorU8::from_rgba(color.r, color.g, color.b, 255)
                {
                    *slot = premultiplied;
                }
            }
        }
    }

    /// Averages a source rectangle, clamped to the canvas.
    fn box_sample(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Color {
        let size = self.size();
        let lo_x = (x0.floor().max(0.0) as u32).min(size.width.saturating_sub(1));
        let lo_y = (y0.floor().max(0.0) as u32).min(size.height.saturating_sub(1));
        let hi_x = (x1.ceil().max(1.0) as u32).clamp(lo_x + 1, size.width);
        let hi_y = (y1.ceil().max(1.0) as u32).clamp(lo_y + 1, size.height);

        let (mut r, mut g, mut b, mut count) = (0u32, 0u32, 0u32, 0u32);
        for y in lo_y..hi_y {
            for x in lo_x..hi_x {
                if let Some(pixel) = self.pixmap.pixel(x, y) {
                    let pixel = pixel.demultiply();
                    r += u32::from(pixel.red());
                    g += u32::from(pixel.green());
                    b += u32::from(pixel.blue());
                    count += 1;
                }
            }
        }
        if count == 0 {
            return palette::PAPER;
        }
        Color::rgb((r / count) as u8, (g / count) as u8, (b / count) as u8)
    }

    /// Copies the canvas into a `0x00RRGGBB` buffer, as window systems want it.
    ///
    /// Returns `false` and leaves the buffer alone when it is the wrong length,
    /// which happens for one frame during a live resize.
    pub fn fill_argb_buffer(&self, buffer: &mut [u32]) -> bool {
        let pixels = self.pixmap.pixels();
        if buffer.len() != pixels.len() {
            return false;
        }
        for (slot, pixel) in buffer.iter_mut().zip(pixels) {
            let pixel = pixel.demultiply();
            *slot = ((pixel.red() as u32) << 16)
                | ((pixel.green() as u32) << 8)
                | (pixel.blue() as u32);
        }
        true
    }

    /// Copies the canvas into an opaque ARGB8888 buffer — the format the
    /// tablet's waveform engine takes.
    ///
    /// Identical to [`Self::fill_argb_buffer`] except that the alpha byte is
    /// `0xFF` rather than zero. The two exist separately because the desktop
    /// preview's `softbuffer` surface wants `0RGB` with the top byte clear,
    /// while the device wants a genuinely opaque pixel. The rasterisation
    /// above them is the same code either way, which is the point (§9).
    ///
    /// On the little-endian aarch64 the tablet runs, a `u32` of `0xFFRRGGBB`
    /// lands in memory as the bytes `B, G, R, 0xFF` — the order the vendor
    /// engine's auxiliary buffer is documented to use (ADR-0007).
    ///
    /// Returns `false` if `buffer` is not exactly the canvas's pixel count,
    /// which is the only way to get this wrong silently.
    pub fn fill_argb8888(&self, buffer: &mut [u32]) -> bool {
        let size = self.size();
        self.blit_argb8888(buffer, size.width as usize, 0, 0, size.width, size.height)
    }

    /// Copies one rectangle of the canvas into an ARGB8888 buffer of
    /// `target_stride` pixels per row, at the same coordinates.
    ///
    /// The mapping is the identity: canvas pixel `(x, y)` lands at `(x, y)` in
    /// the target. That is what the device is — a 1:1 surface — and a partial
    /// copy is how a stroke reaches the panel without repainting 3.5 million
    /// pixels first.
    ///
    /// Returns `false` and copies nothing if the rectangle leaves either the
    /// canvas or the target, rather than clipping. A caller that asked for the
    /// wrong rectangle has a bug, and a silently smaller update shows up as a
    /// stale strip on the glass rather than as an error.
    pub fn blit_argb8888(
        &self,
        target: &mut [u32],
        target_stride: usize,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> bool {
        let size = self.size();
        // Saturating throughout: a caller passing `u32::MAX` should get
        // `false`, not a debug-build panic in the bounds check itself.
        let right = x.saturating_add(width);
        let bottom = y.saturating_add(height);
        if right > size.width || bottom > size.height || target_stride < right as usize {
            return false;
        }
        if width == 0 || height == 0 {
            return true;
        }
        let last_row_end = (bottom as usize - 1) * target_stride + right as usize;
        if last_row_end > target.len() {
            return false;
        }

        let pixels = self.pixmap.pixels();
        let source_stride = size.width as usize;
        for row in 0..height as usize {
            let source_row = (y as usize + row) * source_stride + x as usize;
            let target_row = (y as usize + row) * target_stride + x as usize;
            let source = &pixels[source_row..source_row + width as usize];
            let target = &mut target[target_row..target_row + width as usize];
            for (slot, pixel) in target.iter_mut().zip(source) {
                let pixel = pixel.demultiply();
                *slot = 0xFF00_0000
                    | ((pixel.red() as u32) << 16)
                    | ((pixel.green() as u32) << 8)
                    | (pixel.blue() as u32);
            }
        }
        true
    }

    /// Encodes the canvas as a PNG at full resolution.
    pub fn to_png(&self) -> io::Result<Vec<u8>> {
        self.pixmap
            .encode_png()
            .map_err(|error| io::Error::other(error.to_string()))
    }

    /// Writes the canvas to a PNG file, creating parent directories.
    pub fn write_png(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, self.to_png()?)
    }
}

fn paint(color: Color) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(color.to_tiny_skia()),
        anti_alias: true,
        ..Paint::default()
    }
}

fn stroke(width: f32) -> Stroke {
    Stroke {
        width: width.max(0.1),
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    }
}

fn to_skia_rect(rect: Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)
}

fn polygon_path(points: &[Point], close: bool) -> Option<tiny_skia::Path> {
    let (first, rest) = points.split_first()?;
    let mut builder = PathBuilder::new();
    builder.move_to(first.x, first.y);
    for point in rest {
        builder.line_to(point.x, point.y);
    }
    if close {
        builder.close();
    }
    builder.finish()
}

fn round_rect_path(rect: Rect, radius: f32) -> Option<tiny_skia::Path> {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let r = radius.min(rect.width / 2.0).min(rect.height / 2.0).max(0.0);
    let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);
    let mut builder = PathBuilder::new();
    builder.move_to(x + r, y);
    builder.line_to(x + w - r, y);
    builder.quad_to(x + w, y, x + w, y + r);
    builder.line_to(x + w, y + h - r);
    builder.quad_to(x + w, y + h, x + w - r, y + h);
    builder.line_to(x + r, y + h);
    builder.quad_to(x, y + h, x, y + h - r);
    builder.line_to(x, y + r);
    builder.quad_to(x, y, x + r, y);
    builder.close();
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::Canvas;
    use crate::color::palette;
    use crate::display::DisplayMapping;
    use crate::text::TextStyle;
    use paper_protocol::{Point, Rect, Size};

    fn canvas(width: u32, height: u32) -> Canvas {
        Canvas::new(Size::new(width, height)).expect("canvas allocates")
    }

    #[test]
    fn starts_as_blank_paper() {
        let canvas = canvas(64, 64);
        assert_eq!(canvas.ink_coverage(), 0.0);
        assert_eq!(canvas.pixel(0, 0), Some(palette::PAPER));
        assert_eq!(canvas.pixel(64, 0), None);
    }

    #[test]
    fn a_zero_sized_canvas_is_refused_rather_than_faked() {
        assert!(Canvas::new(Size::new(0, 10)).is_none());
    }

    #[test]
    fn filling_marks_the_pixels_it_covers_and_no_others() {
        let mut canvas = canvas(64, 64);
        canvas.fill_rect(Rect::new(16.0, 16.0, 32.0, 32.0), palette::INK);

        assert_eq!(canvas.pixel(32, 32), Some(palette::INK));
        assert_eq!(canvas.pixel(4, 4), Some(palette::PAPER));
        let coverage = canvas.ink_coverage();
        assert!((coverage - 0.25).abs() < 0.01, "coverage was {coverage}");
    }

    #[test]
    fn text_puts_ink_on_the_page() {
        let mut canvas = canvas(400, 120);
        canvas.draw_text(
            "CHESS",
            Point::new(20.0, 20.0),
            TextStyle::new(80.0, palette::INK),
        );
        assert!(canvas.ink_coverage() > 0.01);
    }

    #[test]
    fn centered_text_is_actually_centered() {
        let mut left = canvas(400, 120);
        let mut right = canvas(400, 120);
        let style = TextStyle::new(60.0, palette::INK).centered();
        left.draw_text("HOME", Point::new(200.0, 30.0), style);
        right.draw_text("HOME", Point::new(200.0, 30.0), style);

        let inked_columns = |canvas: &Canvas| -> (u32, u32) {
            let mut min = u32::MAX;
            let mut max = 0;
            for x in 0..400 {
                for y in 0..120 {
                    if canvas.pixel(x, y) != Some(palette::PAPER) {
                        min = min.min(x);
                        max = max.max(x);
                        break;
                    }
                }
            }
            (min, max)
        };
        let (min, max) = inked_columns(&left);
        let midpoint = (min + max) / 2;
        assert!(midpoint.abs_diff(200) < 6, "midpoint was {midpoint}");
        assert_eq!(inked_columns(&left), inked_columns(&right));
    }

    #[test]
    fn presenting_letterboxes_and_scales_without_stretching() {
        let mut source = canvas(100, 200);
        source.fill_rect(Rect::new(0.0, 0.0, 100.0, 200.0), palette::INK);

        let mapping = DisplayMapping::fit(Size::new(100, 200), Size::new(200, 200));
        let mut surface = canvas(200, 200);
        source.present_into(&mut surface, mapping);

        // Bars left and right, image in the middle, aspect preserved.
        assert_eq!(surface.pixel(2, 100), Some(palette::LETTERBOX));
        assert_eq!(surface.pixel(197, 100), Some(palette::LETTERBOX));
        assert_eq!(surface.pixel(100, 100), Some(palette::INK));
    }

    #[test]
    fn box_filtering_keeps_a_thin_line_visible_when_scaled_down() {
        // Point sampling would drop this line entirely at 0.25x.
        let mut source = canvas(400, 400);
        source.fill_rect(Rect::new(199.0, 0.0, 2.0, 400.0), palette::INK);

        let mapping = DisplayMapping::fit(Size::new(400, 400), Size::new(100, 100));
        let mut surface = canvas(100, 100);
        source.present_into(&mut surface, mapping);

        let column = surface.pixel(50, 50).expect("inside the surface");
        assert!(
            column.luminance() < palette::PAPER.luminance(),
            "the line survived as {column:?}"
        );
    }

    #[test]
    fn argb_buffer_fill_refuses_a_mismatched_length() {
        let canvas = canvas(4, 4);
        let mut correct = vec![0u32; 16];
        let mut wrong = vec![0u32; 15];
        assert!(canvas.fill_argb_buffer(&mut correct));
        assert!(!canvas.fill_argb_buffer(&mut wrong));
        assert_eq!(correct[0], palette::PAPER.to_argb());
    }

    #[test]
    fn the_device_fill_is_opaque_where_the_desktop_fill_is_not() {
        let canvas = canvas(4, 4);
        let mut desktop = vec![0u32; 16];
        let mut device = vec![0u32; 16];
        assert!(canvas.fill_argb_buffer(&mut desktop));
        assert!(canvas.fill_argb8888(&mut device));

        assert_eq!(desktop[0] >> 24, 0x00);
        assert_eq!(device[0] >> 24, 0xFF);
        // Same rasterisation underneath: only the alpha byte differs.
        assert_eq!(device[0] & 0x00FF_FFFF, desktop[0] & 0x00FF_FFFF);
    }

    #[test]
    fn a_partial_blit_touches_only_its_own_rectangle() {
        let mut canvas = canvas(8, 8);
        canvas.fill_rect(Rect::new(2.0, 2.0, 2.0, 2.0), palette::INK);

        let mut target = vec![0u32; 64];
        assert!(canvas.blit_argb8888(&mut target, 8, 2, 2, 2, 2));

        for y in 0..8u32 {
            for x in 0..8u32 {
                let slot = target[(y * 8 + x) as usize];
                let inside = (2..4).contains(&x) && (2..4).contains(&y);
                assert_eq!(slot != 0, inside, "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn a_blit_into_a_wider_target_lands_at_the_same_coordinates() {
        let mut canvas = canvas(4, 4);
        canvas.fill_rect(Rect::new(1.0, 1.0, 1.0, 1.0), palette::INK);

        let stride = 16;
        let mut target = vec![0u32; stride * 4];
        assert!(canvas.blit_argb8888(&mut target, stride, 1, 1, 1, 1));
        assert_ne!(target[stride + 1], 0);
        assert_eq!(target[stride], 0);
    }

    #[test]
    fn an_out_of_bounds_blit_refuses_rather_than_clipping() {
        let canvas = canvas(4, 4);
        let mut target = vec![0u32; 16];
        assert!(!canvas.blit_argb8888(&mut target, 4, 2, 2, 4, 4));
        assert!(!canvas.blit_argb8888(&mut target, 4, 0, 0, u32::MAX, 1));
        // A stride narrower than the rectangle would wrap rows silently.
        assert!(!canvas.blit_argb8888(&mut target, 2, 0, 0, 4, 4));
        // A correct rectangle into too small a target is still refused.
        let mut small = vec![0u32; 8];
        assert!(!canvas.blit_argb8888(&mut small, 4, 0, 0, 4, 4));
        assert!(target.iter().all(|slot| *slot == 0));
    }

    #[test]
    fn an_empty_blit_succeeds_and_does_nothing() {
        let canvas = canvas(4, 4);
        let mut target = vec![0u32; 16];
        assert!(canvas.blit_argb8888(&mut target, 4, 0, 0, 0, 0));
        assert!(target.iter().all(|slot| *slot == 0));
    }

    #[test]
    fn png_encoding_produces_a_real_png() {
        let canvas = canvas(8, 8);
        let bytes = canvas.to_png().expect("encodes");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
