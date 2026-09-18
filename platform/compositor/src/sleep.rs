//! The sleep cover and a basic lock screen: a synthesised frame the
//! compositor shows in place of whatever is foreground, until woken (WWW-52,
//! WWW-82).
//!
//! Like [`crate::stopped`]'s "`<app> stopped`" frame, [`render_lock_frame`]
//! never reads a client's committed pixels or [`crate::chrome`]'s status
//! bar — it paints straight onto a fresh panel-sized canvas, so nothing
//! drawn underneath survives being shown. `Compositor::sleep`/`Compositor::wake`
//! (`server.rs`) are what call this and what stop presenting a client's
//! frames while it is showing; this module is only the drawing.
//!
//! Deliberately styled the opposite way round from `stopped`'s
//! paper-on-ink frame — ink background, paper text — so the two are
//! distinguishable from across a room, not just by their words.
//!
//! ## What this does not do
//!
//! Nothing here decides *when* to sleep. There is no idle timer, no power
//! button, and no real device suspend signal wired to `Compositor::sleep` —
//! same split ADR-0033 made for `Surface` ahead of WWW-78's event loop, and
//! ADR-0035 made for `chrome` ahead of a real client: this ticket builds the
//! layer later work drives, not the driving itself. There is also no PIN or
//! any other authentication clearing the lock — WWW-21 dropped that scope
//! rather than ship it as an interim path, and WWW-52 asks for "basic" here,
//! "enough to see it working on the glass, not a designed product."

use paper_device::{DeviceError, Panel, PixelRect, Refresh, Waveform, present};
use paper_sdk::{Canvas, Point, TextStyle, palette};

/// Cap height of the lock screen's message, in canvas pixels.
const MESSAGE_SIZE: f32 = 48.0;

/// Draws the basic lock screen centred on `panel` and presents it with a
/// full refresh.
///
/// Full, not partial: like [`crate::stopped::render_stopped_frame`], there is
/// no prior frame this layer trusts enough to diff a partial update against
/// — the whole point of a lock screen is that it does not depend on what was
/// on the glass a moment ago.
pub fn render_lock_frame(panel: &mut dyn Panel) -> Result<(), DeviceError> {
    let size = panel.size();
    let mut canvas = Canvas::new(size)
        .ok_or_else(|| DeviceError::unexpected(format!("no canvas for a {size:?} panel")))?;
    canvas.clear(palette::INK);
    let at = Point::new(
        size.width as f32 / 2.0,
        size.height as f32 / 2.0 - MESSAGE_SIZE / 2.0,
    );
    canvas.draw_text(
        "Locked",
        at,
        TextStyle::new(MESSAGE_SIZE, palette::PAPER).centered(),
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
    use super::render_lock_frame;
    use paper_device::{MemoryPanel, Refresh};
    use paper_sdk::{Size, palette};

    #[test]
    fn the_lock_frame_paints_something_other_than_a_blank_panel() {
        let mut panel = MemoryPanel::new(Size::new(64, 64));
        render_lock_frame(&mut panel).unwrap();

        assert_eq!(panel.swaps().len(), 1);
        assert_eq!(panel.swaps()[0].refresh, Refresh::Full);
        assert!(
            (0..64)
                .flat_map(|y| (0..64).map(move |x| (x, y)))
                .any(|(x, y)| panel.pixel(x, y) != Some(0xFFFF_FFFF)),
            "expected at least one non-blank pixel from the rendered lock screen"
        );
    }

    /// Pins down the "distinguishable from across a room" claim in the
    /// module doc: the lock screen's background is dark, the opposite of
    /// `render_stopped_frame`'s paper-coloured one.
    #[test]
    fn the_background_is_ink_not_paper() {
        let mut panel = MemoryPanel::new(Size::new(64, 64));
        render_lock_frame(&mut panel).unwrap();

        assert_eq!(
            panel.pixel(0, 0),
            Some(0xFF00_0000 | palette::INK.to_argb())
        );
    }

    #[test]
    fn a_zero_sized_panel_is_refused_rather_than_panicking() {
        let mut panel = MemoryPanel::new(Size::new(0, 0));
        assert!(render_lock_frame(&mut panel).is_err());
    }
}
