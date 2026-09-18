//! Rendering the "`<label> stopped`" frame the compositor shows in place of
//! a client that died or was killed while foreground (WWW-78).
//!
//! A synthesised frame, not the dead client's last committed pixels: those
//! pixels are exactly what a crashed or hostile client cannot be trusted to
//! have left in a legible state, which is the whole reason this exists
//! rather than just leaving the panel showing whatever was there.

use paper_device::{DeviceError, Panel, PixelRect, Refresh, Waveform, present};
use paper_sdk::{Canvas, Point, TextStyle, palette};

/// Cap height of the stopped-frame message, in canvas pixels.
const MESSAGE_SIZE: f32 = 48.0;

/// Draws "`<label> stopped`" centred on `panel` and presents it with a full
/// refresh.
///
/// Full, not partial: the client that owned this rectangle is gone, so there
/// is no trusted prior frame for the waveform engine to diff a partial
/// update against.
pub fn render_stopped_frame(panel: &mut dyn Panel, label: &str) -> Result<(), DeviceError> {
    let size = panel.size();
    let mut canvas = Canvas::new(size)
        .ok_or_else(|| DeviceError::unexpected(format!("no canvas for a {size:?} panel")))?;
    canvas.clear(palette::PAPER);
    let message = format!("{label} stopped");
    let at = Point::new(
        size.width as f32 / 2.0,
        size.height as f32 / 2.0 - MESSAGE_SIZE / 2.0,
    );
    canvas.draw_text(
        &message,
        at,
        TextStyle::new(MESSAGE_SIZE, palette::INK).centered(),
    );
    present(
        panel,
        &canvas,
        PixelRect::whole(size),
        Waveform::UI,
        Refresh::Full,
    )
}

#[cfg(test)]
mod tests {
    use super::render_stopped_frame;
    use paper_device::{MemoryPanel, Refresh};
    use paper_sdk::Size;

    #[test]
    fn the_stopped_frame_paints_something_other_than_a_blank_panel() {
        let mut panel = MemoryPanel::new(Size::new(64, 64));
        render_stopped_frame(&mut panel, "Chess").unwrap();

        assert_eq!(panel.swaps().len(), 1);
        assert_eq!(panel.swaps()[0].refresh, Refresh::Full);
        assert!(
            (0..64)
                .flat_map(|y| (0..64).map(move |x| (x, y)))
                .any(|(x, y)| panel.pixel(x, y) != Some(0xFFFF_FFFF)),
            "expected at least one non-blank pixel from the rendered label"
        );
    }

    #[test]
    fn a_zero_sized_panel_is_refused_rather_than_panicking() {
        let mut panel = MemoryPanel::new(Size::new(0, 0));
        assert!(render_stopped_frame(&mut panel, "Chess").is_err());
    }
}
