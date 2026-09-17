//! The reMarkable Paper Pro adapter.
//!
//! Everything vendor-specific lives here (§9). Apps never reach it: they draw
//! into a [`Canvas`](paper_sdk::Canvas) and handle
//! [`PointerEvent`](paper_sdk::PointerEvent)s, and whether those pixels reach
//! a Mac window or an e-ink panel is not theirs to know. Nothing above this
//! crate may import Qt types, Linux device paths, SSH, systemd or Xochitl
//! controls.
//!
//! ## What this crate is, in one picture
//!
//! ```text
//!     apps  ->  paper_sdk::Canvas  ->  panel::present  ->  Panel
//!                                                           |
//!                             +-----------------------------+
//!                             |                             |
//!                      MemoryPanel                    VendorPanel
//!                   (Mac, tests, dry runs)      (`vendor-engine` feature)
//!                                                           |
//!                                              native/ C ABI -> libqsgepaper
//! ```
//!
//! The rasterisation above the split is the same code on both sides — the
//! same [`Canvas`](paper_sdk::Canvas), the same bundled stroke font, the same
//! ARGB8888 output. That is a §9 requirement and it is why the device backend
//! is a *presenter* rather than a renderer.
//!
//! ## What is proven, and what is not
//!
//! Proven, on the Mac, by the tests in this crate: coordinate transforms, the
//! evdev state machines including multitouch and dropped frames, swap
//! rectangle arithmetic, the wakelock's release-on-every-path behaviour, the
//! advisory lock parsing, and — via `native/check-abi.sh` — that the C++
//! declarations generate exactly the symbols `libqsgepaper.so` exports.
//!
//! **Not proven: anything about the glass.** WWW-3 superseded the first half of
//! what used to be written here — [`VendorPanel`] has been compiled against the
//! real library, has taken the display from stock three times and has driven
//! the EPD rails through a panel-specific waveform table. What none of that
//! established is whether the resulting *image* is correct: a wrong byte order
//! or a wrong buffer looks identical from this side, and nothing in this crate
//! has seen the panel. A passing test here is still not device qualification;
//! see `docs/adr/0009-device-adapter-ffi-boundary.md` for the list of gates
//! that remain open and what each one blocks.
//!
//! ## What is deliberately elsewhere
//!
//! Stopping and restarting `xochitl.service`, writing runtime units, and
//! supervising the host are the host state machine's (WWW-4). This crate
//! supplies the primitives that any orchestration needs — [`WakeLock`],
//! [`DisplayLocks`] — and holds no opinion about systemd.

pub mod error;
pub mod hold;
pub mod input;
pub mod panel;
pub mod session;
pub mod stock;
pub mod takeover;
pub mod waveform;

#[cfg(feature = "vendor-engine")]
pub mod vendor;

pub use error::{DeviceError, VendorStatus};
pub use hold::{
    DEFAULT_HOLD, DisplayProbe, DisplaySample, FrameDigest, HoldPlan, PanelRecord, PanelWork,
    present_and_hold,
};
pub use input::{
    ContactIds, InputNode, InputRole, PenDecoder, PointerTransform, RawEvent, TouchDecoder,
};
pub use panel::{MemoryPanel, Panel, PanelBuffer, Plane, Swap, present};
pub use session::{DisplayLockHolder, DisplayLocks, RESUME_BRIDGE_DELAY, WakeLock};
pub use stock::{STOCK_UNIT, ServiceControl, StartBudget, Stock, StockHealth};
pub use takeover::{Takeover, WAKELOCK_TAG, Watchdog};
pub use waveform::{ContentType, GhostControl, PixelRect, Refresh, Waveform};

#[cfg(target_os = "linux")]
pub use hold::{HoldReport, RegistryCheck, open_and_hold, present_only};

#[cfg(feature = "vendor-engine")]
pub use vendor::VendorPanel;

/// Opens the best panel this build can provide.
///
/// With the `vendor-engine` feature that is the real one; without it, a
/// [`MemoryPanel`] of the right size, which presents nothing and says so.
/// Callers that must not silently draw into a void should check
/// [`is_real_device`] rather than inferring it from a successful open.
pub fn open_panel() -> Result<Box<dyn Panel>, DeviceError> {
    #[cfg(feature = "vendor-engine")]
    {
        Ok(Box::new(vendor::VendorPanel::open()?))
    }
    #[cfg(not(feature = "vendor-engine"))]
    {
        Ok(Box::new(MemoryPanel::new(paper_sdk::SCREEN)))
    }
}

/// Whether [`open_panel`] returns something that can reach the glass.
///
/// `false` on a Mac build, and on any build without `vendor-engine`. Callers
/// that would do something irreversible on the strength of it — stopping
/// Xochitl, say — must ask before they act, not after.
pub const fn is_real_device() -> bool {
    cfg!(feature = "vendor-engine")
}

#[cfg(test)]
mod tests {
    use super::{is_real_device, open_panel};
    use paper_sdk::SCREEN;

    #[test]
    fn a_build_without_the_vendor_engine_says_so_rather_than_pretending() {
        assert!(!is_real_device());
        let panel = open_panel().expect("opens");
        assert_eq!(panel.size(), SCREEN);
    }

    #[test]
    fn the_panel_a_mac_build_opens_is_the_screen_the_sdk_draws_for() {
        // If these ever disagree, `present` starts refusing every frame with an
        // "identity" error, and this is the cheaper place to find out.
        let panel = open_panel().expect("opens");
        assert_eq!(panel.size(), SCREEN);
        assert_eq!(SCREEN.width, 1620);
        assert_eq!(SCREEN.height, 2160);
    }
}
