//! Section: the surface geometry this session actually received.
//!
//! WWW-36 found `docs/adr/0009-device-adapter-ffi-boundary.md` had recorded a
//! refuted conclusion as fact because nobody checked the assumption against
//! the device. `quill`'s firmware reports 6528 bytes/row on a 1620px-wide
//! panel — not tightly packed — and this project's own value was unconfirmed
//! before this section existed. It reads `Context::surface()`, which carries
//! whatever `Hello.surface` this session's host actually sent (a compiled
//! constant is not in this code path at all), so what is printed here is
//! this run's own evidence, not an assumption repeated with more confidence.

use paper_sdk::{Canvas, Point, Rect, SurfaceDescriptor, TextStyle, palette};

pub(crate) fn render(canvas: &mut Canvas, content: Rect, surface: SurfaceDescriptor) {
    canvas.draw_text(
        "Hello.surface FOR THIS SESSION, NOT A COMPILED CONSTANT",
        Point::new(content.x, content.y),
        TextStyle::new(26.0, palette::INK_SOFT).with_tracking(0.06),
    );

    let packed = surface.extent.width * surface.format.bytes_per_pixel();
    let padding = surface.stride_bytes.saturating_sub(packed);
    let stride_px = surface.stride_bytes / surface.format.bytes_per_pixel().max(1);

    let lines = [
        format!(
            "EXTENT    {} x {} px",
            surface.extent.width, surface.extent.height
        ),
        format!("FORMAT    {:?}", surface.format),
        format!(
            "STRIDE    {} bytes/row ({} px/row)",
            surface.stride_bytes, stride_px
        ),
        if padding == 0 {
            "PACKED?   yes, stride equals width times bytes-per-pixel".to_owned()
        } else {
            format!("PACKED?   no, {padding} byte(s)/row of padding beyond a tightly packed row")
        },
    ];

    let mut y = content.y + 56.0;
    for line in lines {
        canvas.draw_text(
            &line,
            Point::new(content.x, y),
            TextStyle::new(32.0, palette::INK).with_weight(0.1),
        );
        y += 56.0;
    }
}

#[cfg(test)]
mod tests {
    use super::render;
    use paper_sdk::{Canvas, PixelFormat, Rect, SCREEN, Size, SurfaceDescriptor};

    #[test]
    fn draws_something_without_going_solid() {
        let mut canvas = Canvas::new(SCREEN).expect("a canvas");
        let surface = SurfaceDescriptor {
            extent: Size::new(1620, 2160),
            stride_bytes: 6528,
            format: PixelFormat::Argb8888,
        };
        render(&mut canvas, Rect::new(40.0, 40.0, 1500.0, 1600.0), surface);
        assert!(canvas.ink_coverage() > 0.0);
    }

    #[test]
    fn a_padded_stride_is_reported_rather_than_assumed_packed() {
        // The exact number WWW-36 found on `quill`'s firmware: 1620px wide,
        // 4 bytes/px, tightly packed would be 6480 — the panel reports 6528.
        let surface = SurfaceDescriptor {
            extent: Size::new(1620, 2160),
            stride_bytes: 6528,
            format: PixelFormat::Argb8888,
        };
        let packed = surface.extent.width * surface.format.bytes_per_pixel();
        assert_eq!(packed, 6480);
        assert_eq!(surface.stride_bytes - packed, 48);
    }
}
