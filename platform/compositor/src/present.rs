//! Presenting a connected client's own pixels — as opposed to
//! [`crate::stopped`]'s synthesised frame — to the panel (WWW-78).
//!
//! `paper_device::panel::present` takes a [`paper_sdk::Canvas`], and a
//! client's pool slot is a raw `&[u8]` this crate never rasterised — it
//! arrived over shared memory from a process this crate does not link
//! against. So this blits directly into the panel's own buffer instead of
//! building a `Canvas` just to hold bytes that already are the panel's
//! format, and calls [`Panel::swap`] itself rather than going through
//! `present`, which means the per-present EINK telemetry `present` records
//! is not recorded on this path — a gap worth closing once a real client
//! makes this path's frequency worth measuring (WWW-81).
//!
//! Always presents the whole extent regardless of what the client's
//! [`Damage`] claimed. `Damage::Full` is always a correct superset (per its
//! own doc, "the host may present more than this claims"), and a partial
//! blit straight into the panel's buffer is an optimisation this ticket does
//! not need to make the crash/hang acceptance criteria true.

use paper_device::{DeviceError, Panel, PixelRect, Refresh, Waveform};
use paper_protocol::ShmPoolDescriptor;

/// Blits `slot_bytes` (one [`crate::pool::Pool`] slot, packed ARGB8888 per
/// `descriptor`) into `panel`'s buffer and swaps the whole panel.
///
/// Refuses a client surface that is not exactly the panel's size — this
/// compositor does not yet place a client's surface anywhere but full-screen
/// (single-foreground-app, per the project's v1 scope), so a mismatch is a
/// caller bug worth an error rather than a silently clipped frame.
pub fn present_pool_slot(
    panel: &mut dyn Panel,
    descriptor: ShmPoolDescriptor,
    slot_bytes: &[u8],
    waveform: Waveform,
) -> Result<(), DeviceError> {
    let extent = descriptor.buffer.extent;
    let panel_size = panel.size();
    if extent != panel_size {
        return Err(DeviceError::unexpected(format!(
            "client surface is {extent:?} but the panel is {panel_size:?}; \
             this compositor only presents a client full-screen"
        )));
    }

    let stride_pixels = (descriptor.buffer.stride_bytes / 4) as usize;
    {
        let buffer = panel.buffer()?;
        let panel_stride = buffer.stride;
        for y in 0..extent.height as usize {
            let src_start = y * stride_pixels * 4;
            let src_row = &slot_bytes[src_start..src_start + extent.width as usize * 4];
            let dst_start = y * panel_stride;
            let dst_row = &mut buffer.pixels[dst_start..dst_start + extent.width as usize];
            for (dst, chunk) in dst_row.iter_mut().zip(src_row.chunks_exact(4)) {
                *dst = u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
        }
    }
    panel.swap(PixelRect::whole(panel_size), waveform, Refresh::Partial)
}

#[cfg(test)]
mod tests {
    use super::present_pool_slot;
    use paper_device::{MemoryPanel, Waveform};
    use paper_protocol::{PixelFormat, ShmPoolDescriptor, Size, SurfaceDescriptor};

    fn descriptor(size: Size) -> ShmPoolDescriptor {
        ShmPoolDescriptor {
            buffer: SurfaceDescriptor::packed(size, PixelFormat::Argb8888),
        }
    }

    #[test]
    fn a_clients_pixels_land_on_the_panel_unchanged() {
        let size = Size::new(4, 4);
        let mut panel = MemoryPanel::new(size);
        let pixel = 0xFF11_2233u32;
        let bytes: Vec<u8> = std::iter::repeat_with(|| pixel.to_ne_bytes())
            .take(16)
            .flatten()
            .collect();

        present_pool_slot(&mut panel, descriptor(size), &bytes, Waveform::CONTENT).unwrap();

        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(panel.pixel(x, y), Some(pixel));
            }
        }
        assert_eq!(panel.swaps().len(), 1);
    }

    #[test]
    fn a_surface_that_is_not_the_panel_size_is_refused() {
        let panel_size = Size::new(8, 8);
        let mut panel = MemoryPanel::new(panel_size);
        let client_size = Size::new(4, 4);
        let bytes = vec![0u8; 4 * 4 * 4];

        let error = present_pool_slot(
            &mut panel,
            descriptor(client_size),
            &bytes,
            Waveform::CONTENT,
        )
        .expect_err("refuses");
        assert!(error.to_string().contains("panel"), "{error}");
        assert!(panel.swaps().is_empty());
    }
}
