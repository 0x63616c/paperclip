//! One error type for everything the tablet can refuse to do.
//!
//! Error conversion across the FFI boundary is a §9 requirement, so it is
//! written down here rather than left to each call site. The C ABI in
//! `native/` returns a plain `int32_t` status and never throws; every non-zero
//! status becomes [`DeviceError::Vendor`] carrying that code and whatever the
//! bridge recorded in its thread-local message slot. Nothing else crosses.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// What went wrong while talking to the device.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DeviceError {
    /// This build has no backend for the thing being asked for — a Mac build,
    /// or a device build without the `vendor-engine` feature.
    #[error("no device backend in this build: {0}")]
    Unsupported(&'static str),

    /// A file or device node could not be opened, read or written.
    #[error("{action} {}: {source}", path.display())]
    Io {
        /// What was being attempted, as a verb phrase: "open", "write to".
        action: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },

    /// The vendor waveform engine returned a non-zero status.
    #[error("vendor display engine: {message} (status {code})")]
    Vendor {
        /// The `int32_t` the C ABI returned. See [`VendorStatus`].
        code: i32,
        /// The bridge's own description, already copied out of C++ storage.
        message: String,
    },

    /// Something on the device was not shaped the way this code expects — a
    /// short read from an event node, an unparseable lock file, a panel whose
    /// geometry disagrees with [`paper_sdk::SCREEN`].
    #[error("{0}")]
    Unexpected(String),

    /// Another process holds the display. Never force past this: the holder is
    /// normally `xochitl`, and two writers on this panel is the failure mode
    /// the wakelock rule exists to prevent.
    #[error("the display is held by {holder}")]
    DisplayBusy {
        /// Whatever the advisory lock named, verbatim.
        holder: String,
    },
}

impl DeviceError {
    /// Wraps an I/O failure with the path and the verb, so the message reads
    /// as a sentence without every call site formatting one.
    pub fn io(action: &'static str, path: impl AsRef<Path>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }

    /// Wraps a description of something structurally wrong.
    pub fn unexpected(what: impl fmt::Display) -> Self {
        Self::Unexpected(what.to_string())
    }
}

/// The status codes the C ABI in `native/` may return.
///
/// These are *ours*, not the vendor's: the bridge catches everything and maps
/// it onto this closed set, so a C++ exception, a Qt assertion path or a null
/// `EPFramebuffer::instance()` all arrive in Rust as an ordinary `Result`.
/// Keep in step with `native/paperclip_ep.h`, which is the other half of this
/// definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
#[non_exhaustive]
pub enum VendorStatus {
    /// The call succeeded.
    Ok = 0,
    /// A caller passed something the bridge rejected before doing any work.
    InvalidArgument = 1,
    /// `EPFramebuffer::instance()` returned null, or the engine had not been
    /// opened.
    NotOpen = 2,
    /// `checkLockFile()` said another `EPFramebuffer` exists.
    Locked = 3,
    /// A C++ exception was caught at the boundary. It did not propagate.
    Exception = 4,
    /// Called from a thread that is not the one that opened the engine.
    WrongThread = 5,
    /// Allocation failed.
    OutOfMemory = 6,
    /// The drawing buffer stopped being the memory the vendor engine holds, so
    /// the frame that was drawn is not the frame that would have been
    /// presented. Never a successful present; see the no-detach invariant in
    /// `native/paperclip_ep.h` and ADR-0009.
    Detached = 7,
    /// The bridge returned a code this build does not know.
    Unknown = -1,
}

impl VendorStatus {
    /// Classifies a raw status from the C ABI.
    pub fn from_raw(code: i32) -> Self {
        match code {
            0 => Self::Ok,
            1 => Self::InvalidArgument,
            2 => Self::NotOpen,
            3 => Self::Locked,
            4 => Self::Exception,
            5 => Self::WrongThread,
            6 => Self::OutOfMemory,
            7 => Self::Detached,
            _ => Self::Unknown,
        }
    }

    /// A description used when the bridge supplied no message of its own.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::InvalidArgument => "the bridge rejected an argument",
            Self::NotOpen => "the waveform engine is not open",
            Self::Locked => "another EPFramebuffer instance holds the panel",
            Self::Exception => "a C++ exception was caught at the FFI boundary",
            Self::WrongThread => "called from a thread other than the owning one",
            Self::OutOfMemory => "allocation failed",
            Self::Detached => {
                "the drawing buffer detached from the engine's: the frame that was drawn is \
                 not the frame the engine holds"
            }
            Self::Unknown => "the bridge returned an unrecognised status",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceError, VendorStatus};
    use std::io;

    #[test]
    fn io_errors_name_the_path_and_the_verb() {
        let error = DeviceError::io(
            "open",
            "/dev/input/event3",
            io::Error::from(io::ErrorKind::PermissionDenied),
        );
        let rendered = error.to_string();
        assert!(
            rendered.starts_with("open /dev/input/event3: "),
            "{rendered}"
        );
    }

    #[test]
    fn unknown_statuses_do_not_panic_or_alias_a_known_one() {
        assert_eq!(VendorStatus::from_raw(0), VendorStatus::Ok);
        assert_eq!(VendorStatus::from_raw(3), VendorStatus::Locked);
        assert_eq!(VendorStatus::from_raw(7), VendorStatus::Detached);
        assert_eq!(VendorStatus::from_raw(4242), VendorStatus::Unknown);
        assert_eq!(VendorStatus::from_raw(-7), VendorStatus::Unknown);
    }

    #[test]
    fn a_busy_display_says_who_has_it() {
        let error = DeviceError::DisplayBusy {
            holder: "xochitl".to_owned(),
        };
        assert!(error.to_string().contains("xochitl"));
    }
}
