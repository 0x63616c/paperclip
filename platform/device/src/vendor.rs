//! The Rust half of the C ABI in `native/`.
//!
//! Compiled only with the `vendor-engine` feature, which only an aarch64 Linux
//! build can turn on. Everything here is the mechanical translation of
//! `native/paperclip_ep.h`; the contract it implements is documented there and
//! in ADR-0009, not repeated.
//!
//! **Nothing in this module has run.** It has never been compiled against the
//! real library, because WWW-3's first pass could not reach the tablet to copy
//! it. What is verified is that the C++ declarations it depends on generate
//! exactly the symbols the library exports — `native/check-abi.sh`.

#![allow(unsafe_code)]
// Justified per the note in the workspace manifest. This module is the FFI
// boundary §9 permits; it is the only place in the crate that calls C++, every
// call is a plain function call into `native/paperclip_ep.cpp`, and each
// `unsafe` block below carries the reason it is sound.

use std::ffi::{CStr, c_char};
use std::ptr::NonNull;

use paper_sdk::Size;

use crate::error::{DeviceError, VendorStatus};
use crate::panel::{Panel, PanelBuffer};
use crate::waveform::{ContentType, GhostControl, PixelRect, Refresh, Waveform};

/// The ABI version this crate was built against. Must match
/// `PAPERCLIP_EP_ABI_VERSION` in `native/paperclip_ep.h`.
const ABI_VERSION: u32 = 1;

#[repr(C)]
struct RawHandle {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn paperclip_ep_abi_version() -> u32;
    fn paperclip_ep_open(out: *mut *mut RawHandle) -> i32;
    fn paperclip_ep_close(ep: *mut RawHandle);
    fn paperclip_ep_geometry(
        ep: *mut RawHandle,
        width: *mut i32,
        height: *mut i32,
        stride_pixels: *mut i32,
    ) -> i32;
    fn paperclip_ep_buffer(ep: *mut RawHandle) -> *mut u32;
    fn paperclip_ep_swap(
        ep: *mut RawHandle,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        content: i32,
        mode: i32,
        full: i32,
    ) -> i32;
    fn paperclip_ep_ghost_control(ep: *mut RawHandle, mode: i32) -> i32;
    fn paperclip_ep_clear(ep: *mut RawHandle) -> i32;
    fn paperclip_ep_last_error() -> *const c_char;
}

/// Reads the bridge's thread-local error text.
fn last_error() -> String {
    // SAFETY: the C ABI guarantees a non-NULL, NUL-terminated, thread-local
    // string that stays valid until the next failing call on this thread. It is
    // copied here before anything else can run.
    let raw = unsafe { paperclip_ep_last_error() };
    if raw.is_null() {
        return String::new();
    }
    // SAFETY: as above — `raw` points at a NUL-terminated string owned by the
    // bridge, and this borrow does not outlive the statement.
    unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned()
}

fn check(code: i32) -> Result<(), DeviceError> {
    let status = VendorStatus::from_raw(code);
    if status == VendorStatus::Ok {
        return Ok(());
    }
    let message = last_error();
    Err(DeviceError::Vendor {
        code,
        message: if message.is_empty() {
            status.describe().to_owned()
        } else {
            message
        },
    })
}

/// The panel, driven by the vendor waveform engine.
///
/// Not `Send` and not `Sync`, and that is load-bearing rather than incidental:
/// `libepaper` asserts when `EPFramebuffer` is constructed off the main
/// thread, and the bridge rejects calls from any thread but the opening one.
/// The `NonNull` field makes the compiler enforce what the C++ only checks at
/// runtime.
#[derive(Debug)]
pub struct VendorPanel {
    handle: NonNull<RawHandle>,
    size: Size,
    stride: usize,
}

impl VendorPanel {
    /// Opens the engine. **Main thread only.**
    ///
    /// Fails with [`DeviceError::Vendor`] carrying [`VendorStatus::Locked`]
    /// when the vendor's own `checkLockFile()` says another `EPFramebuffer`
    /// exists — normally because `xochitl` is still running.
    pub fn open() -> Result<Self, DeviceError> {
        // SAFETY: no arguments, no state; the bridge returns a constant.
        let built = unsafe { paperclip_ep_abi_version() };
        if built != ABI_VERSION {
            return Err(DeviceError::unexpected(format!(
                "native bridge ABI {built}, this crate expects {ABI_VERSION}; \
                 rebuild platform/device/native"
            )));
        }

        let mut raw: *mut RawHandle = std::ptr::null_mut();
        // SAFETY: `raw` is a live, correctly typed out-parameter. The bridge
        // either writes an owned handle and returns OK, or leaves it null.
        check(unsafe { paperclip_ep_open(&mut raw) })?;
        let handle = NonNull::new(raw).ok_or_else(|| {
            DeviceError::unexpected("the bridge reported success but returned no handle")
        })?;

        let (mut width, mut height, mut stride) = (0i32, 0i32, 0i32);
        // SAFETY: `handle` is the handle the bridge just gave us, and the three
        // out-parameters are live and correctly typed.
        let status =
            unsafe { paperclip_ep_geometry(handle.as_ptr(), &mut width, &mut height, &mut stride) };
        if let Err(error) = check(status) {
            // SAFETY: `handle` is owned here and has not been closed. Closing it
            // on this path is what stops a failed open leaking the engine.
            unsafe { paperclip_ep_close(handle.as_ptr()) };
            return Err(error);
        }
        if width <= 0 || height <= 0 || stride < width {
            // SAFETY: as above.
            unsafe { paperclip_ep_close(handle.as_ptr()) };
            return Err(DeviceError::unexpected(format!(
                "the bridge reported {width}x{height} at stride {stride}"
            )));
        }

        Ok(Self {
            handle,
            size: Size::new(width as u32, height as u32),
            stride: stride as usize,
        })
    }
}

impl Panel for VendorPanel {
    fn size(&self) -> Size {
        self.size
    }

    fn buffer(&mut self) -> Result<PanelBuffer<'_>, DeviceError> {
        // SAFETY: `self.handle` is live for as long as `self` is.
        let raw = unsafe { paperclip_ep_buffer(self.handle.as_ptr()) };
        let raw = NonNull::new(raw).ok_or_else(|| DeviceError::Vendor {
            code: VendorStatus::NotOpen as i32,
            message: last_error(),
        })?;
        let len = self.stride * self.size.height as usize;

        // SAFETY: the bridge owns `height * stride` u32 of ARGB8888 for the
        // lifetime of the handle, and `&mut self` means no other borrow of it
        // exists. The returned slice's lifetime is tied to `self`, so it cannot
        // outlive the engine that owns the memory.
        let pixels = unsafe { std::slice::from_raw_parts_mut(raw.as_ptr(), len) };
        Ok(PanelBuffer {
            pixels,
            stride: self.stride,
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
        if rect.is_empty() {
            return Ok(());
        }
        let content = match waveform.content() {
            ContentType::Mono => 0,
            ContentType::Color => 1,
        };
        // SAFETY: the handle is live, and the rectangle has just been checked
        // against the geometry the bridge itself reported. The bridge checks it
        // again; this is belt and braces on the one call that can scribble.
        let status = unsafe {
            paperclip_ep_swap(
                self.handle.as_ptr(),
                rect.x as i32,
                rect.y as i32,
                rect.width as i32,
                rect.height as i32,
                content,
                waveform.mode(),
                refresh.full_flag(),
            )
        };
        check(status)
    }

    fn ghost_control(&mut self, mode: GhostControl) -> Result<(), DeviceError> {
        // SAFETY: the handle is live; `mode` is a plain integer.
        check(unsafe { paperclip_ep_ghost_control(self.handle.as_ptr(), mode as i32) })
    }

    fn clear(&mut self) -> Result<(), DeviceError> {
        // SAFETY: the handle is live.
        check(unsafe { paperclip_ep_clear(self.handle.as_ptr()) })
    }
}

impl Drop for VendorPanel {
    fn drop(&mut self) {
        // Shutdown order, from `native/paperclip_ep.h`: the caller clears the
        // panel before dropping. Dropping does not clear, because a drop during
        // an unwind should hand the display back as fast as possible rather
        // than spend a settled full refresh doing it.
        //
        // SAFETY: `self.handle` was produced by `paperclip_ep_open`, has not
        // been closed, and cannot be used again — this is `drop`.
        unsafe { paperclip_ep_close(self.handle.as_ptr()) };
    }
}
