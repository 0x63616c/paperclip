//! The narrow interface everything above the device speaks to.
//!
//! One trait, two implementations: [`MemoryPanel`], which is what a Mac build
//! and every test gets, and the vendor-engine panel behind the
//! `vendor-engine` feature. Apps never see either — they draw into a
//! [`Canvas`] and the host presents it, which is the whole point of §9's
//! isolation rule.
//!
//! ## What a panel promises
//!
//! - Its buffer is `width * height` ARGB8888 pixels, opaque, row-major, with
//!   a stride in pixels that may exceed the width.
//! - Writing to the buffer changes nothing on the glass. Only [`Panel::swap`]
//!   does, and only inside the rectangle it is given.
//! - A swap rectangle outside the panel is an error, not a clamp.
//!
//! ## What it does not promise
//!
//! That anything is legible. No implementation in this repository has driven
//! the real panel; see `docs/adr/0009-device-adapter-ffi-boundary.md`.

use std::fmt;

use paper_sdk::{Canvas, Size};

use crate::error::DeviceError;
use crate::waveform::{GhostControl, PixelRect, Refresh, Waveform};

/// Mutable access to a panel's pixels.
///
/// Held only for as long as a caller is drawing. The lifetime is what stops a
/// buffer pointer outliving the engine that owns it — the vendor buffer is
/// borrowed from C++, never owned by Rust, and dropping the panel while a
/// slice into it is alive is exactly the bug this shape makes impossible.
#[derive(Debug)]
pub struct PanelBuffer<'a> {
    /// The pixels, ARGB8888, `stride` per row.
    pub pixels: &'a mut [u32],
    /// Pixels per row, which may be larger than the panel width.
    pub stride: usize,
    /// The panel extent these pixels cover.
    pub size: Size,
}

/// One of the three buffers the vendor engine was given.
///
/// `paperclip_ep_open` hands it `setBuffers(make_tuple(front, back), &aux)`,
/// and it presents from `front` — settled by WWW-29's disassembly of
/// `setBuffers`, not by reading the planes back, which cannot answer it. Being
/// able to read all three is still how a frame gets checked against what was
/// sent; see [`Panel::readback`] for what that is and is not worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// Where [`Panel::buffer`] draws.
    Front,
    /// The other half of the engine's pair.
    Back,
    /// The auxiliary buffer the engine was given a pointer to.
    Aux,
}

impl Plane {
    /// Every plane, in the order a report lists them.
    pub const ALL: [Plane; 3] = [Plane::Front, Plane::Back, Plane::Aux];

    /// The name used in reports.
    pub fn name(self) -> &'static str {
        match self {
            Plane::Front => "front",
            Plane::Back => "back",
            Plane::Aux => "aux",
        }
    }
}

/// A surface the tablet can be made to show.
pub trait Panel: fmt::Debug {
    /// The panel extent in pixels.
    fn size(&self) -> Size;

    /// Borrows the pixels for drawing. Changes nothing until [`Self::swap`].
    fn buffer(&mut self) -> Result<PanelBuffer<'_>, DeviceError>;

    /// Drives `rect` to the pixels currently in the buffer.
    ///
    /// Keep `refresh` at [`Refresh::Partial`] unless a full flash is actually
    /// wanted: the vendor backend escalates any full update to the whole
    /// panel regardless of the rectangle.
    fn swap(
        &mut self,
        rect: PixelRect,
        waveform: Waveform,
        refresh: Refresh,
    ) -> Result<(), DeviceError>;

    /// Sets the engine's ghost-suppression policy.
    fn ghost_control(&mut self, mode: GhostControl) -> Result<(), DeviceError>;

    /// Copies a plane out, so what was sent can be compared with what the
    /// engine is holding.
    ///
    /// This is **not** a picture of the panel. It is the buffer on this side of
    /// the vendor boundary, and the strongest thing a match can support is
    /// "the engine still has our pixels" — which does rule out the one failure
    /// that is otherwise invisible from here, a silent fallback that reports
    /// success over a blank or substituted frame (§17).
    ///
    /// It supports even that much only because the vendor engine *shares* the
    /// front buffer rather than copying it, so reading it reads the engine's
    /// memory. WWW-29 found that the bridge had broken that sharing, which made
    /// a front match this side's private copy agreeing with itself — true for
    /// any engine behaviour whatever. The bridge now refuses a detached front
    /// with [`VendorStatus::Detached`] instead of answering; see ADR-0009.
    ///
    /// [`VendorStatus::Detached`]: crate::error::VendorStatus::Detached
    fn readback(&self, plane: Plane) -> Result<Vec<u32>, DeviceError>;

    /// Drives the whole panel white and waits for the waveform to settle.
    ///
    /// Required before handing the display back. WWW-20 photographed what
    /// skipping it looks like: bands and hairlines laid over a stock UI that
    /// Xochitl's own repaint does not remove.
    fn clear(&mut self) -> Result<(), DeviceError>;
}

/// Copies a canvas rectangle into a panel and swaps it, in one call.
///
/// This is the only place the two halves of §9 meet — the shared rasteriser
/// above, the vendor engine below — so the ARGB8888 conversion happens once
/// and both backends get the same pixels.
///
/// The canvas must be exactly the panel's size. A letterboxed or scaled
/// presentation is a desktop concern; on the device the mapping is the
/// identity and anything else is a bug worth an error rather than a blur.
pub fn present(
    panel: &mut dyn Panel,
    canvas: &Canvas,
    rect: PixelRect,
    waveform: Waveform,
    refresh: Refresh,
) -> Result<(), DeviceError> {
    let size = panel.size();
    if canvas.size() != size {
        return Err(DeviceError::unexpected(format!(
            "canvas is {}x{} but the panel is {}x{}; the device mapping is the identity",
            canvas.size().width,
            canvas.size().height,
            size.width,
            size.height
        )));
    }
    if !rect.fits_in(size) {
        return Err(DeviceError::unexpected(format!(
            "swap rectangle {rect:?} leaves a {}x{} panel",
            size.width, size.height
        )));
    }
    if rect.is_empty() {
        return Ok(());
    }

    {
        let buffer = panel.buffer()?;
        let stride = buffer.stride;
        if !canvas.blit_argb8888(
            buffer.pixels,
            stride,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
        ) {
            return Err(DeviceError::unexpected(format!(
                "the panel buffer is too small for {rect:?} at stride {stride}"
            )));
        }
    }
    panel.swap(rect, waveform, refresh)
}

/// A panel that exists only in memory.
///
/// What a Mac build gets, what every test drives, and what `paperctl` uses to
/// dry-run a presentation without a tablet. It records the swaps it was asked
/// for so a test can assert on the rectangles and waveforms a screen produces
/// — which is the part of device behaviour that *can* be checked off-device.
///
/// It is not a simulator. It says nothing about legibility, refresh time or
/// ghosting.
#[derive(Debug)]
pub struct MemoryPanel {
    size: Size,
    pixels: Vec<u32>,
    swaps: Vec<Swap>,
    cleared: usize,
    ghost: GhostControl,
}

/// One recorded [`Panel::swap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swap {
    /// The rectangle presented.
    pub rect: PixelRect,
    /// The waveform it was presented with.
    pub waveform: Waveform,
    /// Whether it was a partial update or a full refresh.
    pub refresh: Refresh,
}

impl MemoryPanel {
    /// A panel of `size`, filled with opaque white.
    pub fn new(size: Size) -> Self {
        let count = size.width as usize * size.height as usize;
        Self {
            size,
            pixels: vec![0xFFFF_FFFF; count],
            swaps: Vec::new(),
            cleared: 0,
            ghost: GhostControl::default(),
        }
    }

    /// Every swap so far, oldest first.
    pub fn swaps(&self) -> &[Swap] {
        &self.swaps
    }

    /// How many times [`Panel::clear`] has run.
    pub fn clears(&self) -> usize {
        self.cleared
    }

    /// The ghost-control mode last set.
    pub fn ghost_mode(&self) -> GhostControl {
        self.ghost
    }

    /// The pixel at `(x, y)`, or `None` off the panel.
    pub fn pixel(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.size.width || y >= self.size.height {
            return None;
        }
        self.pixels
            .get(y as usize * self.size.width as usize + x as usize)
            .copied()
    }

    /// Forgets the recorded history, keeping the pixels.
    pub fn forget_history(&mut self) {
        self.swaps.clear();
        self.cleared = 0;
    }
}

impl Panel for MemoryPanel {
    fn size(&self) -> Size {
        self.size
    }

    fn buffer(&mut self) -> Result<PanelBuffer<'_>, DeviceError> {
        Ok(PanelBuffer {
            stride: self.size.width as usize,
            pixels: &mut self.pixels,
            size: self.size,
        })
    }

    fn swap(
        &mut self,
        rect: PixelRect,
        waveform: Waveform,
        refresh: Refresh,
    ) -> Result<(), DeviceError> {
        if !rect.fits_in(self.size) {
            return Err(DeviceError::unexpected(format!(
                "swap rectangle {rect:?} leaves a {}x{} panel",
                self.size.width, self.size.height
            )));
        }
        self.swaps.push(Swap {
            rect,
            waveform,
            refresh,
        });
        Ok(())
    }

    fn readback(&self, plane: Plane) -> Result<Vec<u32>, DeviceError> {
        // One buffer, reported as all three. A memory panel has no engine to
        // disagree with, so every plane is what was written — which makes the
        // comparison in a report trivially true here and meaningfully false
        // only on the device, where it is worth checking.
        let _ = plane;
        Ok(self.pixels.clone())
    }

    fn ghost_control(&mut self, mode: GhostControl) -> Result<(), DeviceError> {
        self.ghost = mode;
        Ok(())
    }

    fn clear(&mut self) -> Result<(), DeviceError> {
        self.pixels.fill(0xFFFF_FFFF);
        self.cleared += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{MemoryPanel, Panel, present};
    use crate::waveform::{GhostControl, PixelRect, Refresh, Waveform};
    use paper_sdk::{Canvas, Rect, Size, palette};

    fn canvas(size: Size, ink: Rect) -> Canvas {
        let mut canvas = Canvas::new(size).expect("allocates");
        canvas.fill_rect(ink, palette::INK);
        canvas
    }

    #[test]
    fn presenting_copies_only_the_rectangle_and_records_one_swap() {
        let size = Size::new(32, 32);
        let mut panel = MemoryPanel::new(size);
        let canvas = canvas(size, Rect::new(4.0, 4.0, 4.0, 4.0));

        present(
            &mut panel,
            &canvas,
            PixelRect::new(4, 4, 4, 4),
            Waveform::INK,
            Refresh::Partial,
        )
        .expect("presents");

        assert_eq!(panel.swaps().len(), 1);
        assert_eq!(panel.swaps()[0].rect, PixelRect::new(4, 4, 4, 4));
        assert_eq!(panel.swaps()[0].waveform, Waveform::INK);
        assert_eq!(panel.swaps()[0].refresh, Refresh::Partial);

        assert_ne!(panel.pixel(5, 5), Some(0xFFFF_FFFF));
        // Outside the rectangle the panel still holds what it held before.
        assert_eq!(panel.pixel(20, 20), Some(0xFFFF_FFFF));
    }

    #[test]
    fn every_presented_pixel_is_opaque() {
        let size = Size::new(8, 8);
        let mut panel = MemoryPanel::new(size);
        let canvas = canvas(size, Rect::new(0.0, 0.0, 8.0, 8.0));

        present(
            &mut panel,
            &canvas,
            PixelRect::whole(size),
            Waveform::CONTENT,
            Refresh::Partial,
        )
        .expect("presents");

        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(panel.pixel(x, y).expect("on the panel") >> 24, 0xFF);
            }
        }
    }

    #[test]
    fn a_canvas_that_is_not_the_panel_size_is_refused_not_scaled() {
        let mut panel = MemoryPanel::new(Size::new(32, 32));
        let canvas = canvas(Size::new(16, 16), Rect::new(0.0, 0.0, 4.0, 4.0));

        let error = present(
            &mut panel,
            &canvas,
            PixelRect::new(0, 0, 4, 4),
            Waveform::INK,
            Refresh::Partial,
        )
        .expect_err("refuses");
        assert!(error.to_string().contains("identity"), "{error}");
        assert!(panel.swaps().is_empty());
    }

    #[test]
    fn a_rectangle_off_the_panel_is_an_error_and_swaps_nothing() {
        let size = Size::new(32, 32);
        let mut panel = MemoryPanel::new(size);
        let canvas = canvas(size, Rect::new(0.0, 0.0, 4.0, 4.0));

        present(
            &mut panel,
            &canvas,
            PixelRect::new(30, 30, 8, 8),
            Waveform::INK,
            Refresh::Partial,
        )
        .expect_err("refuses");
        assert!(panel.swaps().is_empty());
    }

    #[test]
    fn an_empty_rectangle_is_a_no_op_rather_than_a_swap() {
        let size = Size::new(16, 16);
        let mut panel = MemoryPanel::new(size);
        let canvas = canvas(size, Rect::new(0.0, 0.0, 4.0, 4.0));

        present(
            &mut panel,
            &canvas,
            PixelRect::new(4, 4, 0, 0),
            Waveform::INK,
            Refresh::Partial,
        )
        .expect("succeeds");
        assert!(panel.swaps().is_empty());
    }

    #[test]
    fn clearing_returns_the_panel_to_white_and_is_counted() {
        let size = Size::new(16, 16);
        let mut panel = MemoryPanel::new(size);
        let canvas = canvas(size, Rect::new(0.0, 0.0, 16.0, 16.0));

        present(
            &mut panel,
            &canvas,
            PixelRect::whole(size),
            Waveform::UI,
            Refresh::Full,
        )
        .expect("presents");
        assert_ne!(panel.pixel(1, 1), Some(0xFFFF_FFFF));

        panel.clear().expect("clears");
        assert_eq!(panel.clears(), 1);
        assert_eq!(panel.pixel(1, 1), Some(0xFFFF_FFFF));
    }

    #[test]
    fn ghost_control_is_remembered() {
        let mut panel = MemoryPanel::new(Size::new(8, 8));
        assert_eq!(panel.ghost_mode(), GhostControl::Auto);
        panel.ghost_control(GhostControl::Full).expect("sets");
        assert_eq!(panel.ghost_mode(), GhostControl::Full);
    }
}
