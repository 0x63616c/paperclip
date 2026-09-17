//! Where an app's pixels go.
//!
//! An app draws into a [`Canvas`] the size of its viewport and then says which
//! parts changed. It never learns where that viewport sits on the panel, what
//! else is on screen, or what presents it — on the Mac that is a letterboxed
//! window, on the tablet it is the vendor waveform engine behind
//! `paper_device`.
//!
//! ## The surface belongs to the host
//!
//! A full 1620×2160 ARGB8888 frame is 14 MiB. Sending that down a socket once
//! per refresh would be the platform's largest single cost and would buy
//! nothing, because the host has to end up with those bytes in a mapping the
//! waveform engine can read either way. So the host owns the memory, describes
//! it in [`Hello`](paper_protocol::Hello), and the app writes into it.
//!
//! How the memory actually reaches an app process on the device — a file
//! descriptor passed with the launch connection, mapped before readiness — is
//! the supervisor's business (WWW-4). What is settled here is the *shape* both
//! sides agree on, and one implementation of it, [`LocalSurface`], which
//! allocates its own buffer. That is what the desktop preview and every test
//! in this repository use, and it is a real implementation rather than a mock:
//! the drawing path above it is byte-for-byte the same one the device runs.

use std::sync::{Arc, Mutex};

use paper_protocol::{Damage, PixelFormat, SurfaceDescriptor};

use crate::canvas::Canvas;

/// Why a surface could not be opened or published.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SurfaceError {
    /// The host described a surface that cannot exist.
    #[error("the host described an unusable surface: {extent:?} at {stride} bytes per row")]
    Unusable {
        /// The extent it claimed.
        extent: paper_protocol::Size,
        /// The stride it claimed.
        stride: u32,
    },

    /// The host asked for a pixel format this build cannot produce.
    #[error("this build cannot draw {format:?}")]
    UnsupportedFormat {
        /// The format asked for.
        format: PixelFormat,
    },

    /// The surface could not be allocated.
    #[error("cannot allocate a {width}x{height} surface")]
    Allocation {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
}

/// A bound drawing surface.
pub trait Surface {
    /// The canvas covering the whole viewport.
    fn canvas(&mut self) -> &mut Canvas;

    /// Hands the drawn frame to the host.
    ///
    /// `damage` is what the app is about to claim in its
    /// [`FrameDone`](paper_protocol::FrameDone); an implementation that copies
    /// into a mapping may use it to copy less.
    fn publish(&mut self, damage: &Damage) -> Result<(), SurfaceError>;
}

/// Opens the surface a [`Hello`](paper_protocol::Hello) described.
pub trait SurfaceProvider {
    /// What it opens.
    type Surface: Surface;

    /// Binds the described surface.
    fn open(&mut self, descriptor: &SurfaceDescriptor) -> Result<Self::Surface, SurfaceError>;
}

/// A record of what was published, for the desktop preview and for tests.
#[derive(Debug, Clone, Default)]
pub struct SurfaceLog {
    published: Arc<Mutex<Vec<Damage>>>,
}

impl SurfaceLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every frame published so far, in order.
    ///
    /// Panics only if a previous holder panicked while publishing, which in a
    /// test is the failure you want to see rather than one to paper over.
    pub fn published(&self) -> Vec<Damage> {
        self.published
            .lock()
            .expect("surface log poisoned by a panicking publisher")
            .clone()
    }

    /// How many frames have been published.
    pub fn frames(&self) -> usize {
        self.published
            .lock()
            .expect("surface log poisoned by a panicking publisher")
            .len()
    }
}

/// A surface backed by an ordinary allocation in this process.
///
/// What the Mac uses, and what every test uses. Publishing appends to a
/// [`SurfaceLog`]; the pixels stay in the canvas, where a caller that wants
/// them — `paperctl screenshot`, a system test — can read them.
#[derive(Debug)]
pub struct LocalSurface {
    canvas: Canvas,
    log: SurfaceLog,
}

impl Surface for LocalSurface {
    fn canvas(&mut self) -> &mut Canvas {
        &mut self.canvas
    }

    fn publish(&mut self, damage: &Damage) -> Result<(), SurfaceError> {
        self.log
            .published
            .lock()
            .expect("surface log poisoned by a panicking publisher")
            .push(damage.clone());
        Ok(())
    }
}

/// Provides [`LocalSurface`]s, all reporting into one [`SurfaceLog`].
#[derive(Debug, Clone, Default)]
pub struct LocalSurfaces {
    log: SurfaceLog,
}

impl LocalSurfaces {
    /// A provider with a fresh log.
    pub fn new() -> Self {
        Self::default()
    }

    /// The log its surfaces publish into.
    pub fn log(&self) -> SurfaceLog {
        self.log.clone()
    }
}

impl SurfaceProvider for LocalSurfaces {
    type Surface = LocalSurface;

    fn open(&mut self, descriptor: &SurfaceDescriptor) -> Result<Self::Surface, SurfaceError> {
        if descriptor.format != PixelFormat::Argb8888 {
            return Err(SurfaceError::UnsupportedFormat {
                format: descriptor.format,
            });
        }
        if descriptor.bytes().is_none() {
            return Err(SurfaceError::Unusable {
                extent: descriptor.extent,
                stride: descriptor.stride_bytes,
            });
        }
        let canvas = Canvas::new(descriptor.extent).ok_or(SurfaceError::Allocation {
            width: descriptor.extent.width,
            height: descriptor.extent.height,
        })?;
        Ok(LocalSurface {
            canvas,
            log: self.log.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalSurfaces, Surface, SurfaceError, SurfaceProvider};
    use paper_protocol::{Damage, PixelFormat, Size, SurfaceDescriptor};

    #[test]
    fn a_surface_is_the_size_the_host_described() {
        let mut surfaces = LocalSurfaces::new();
        let descriptor = SurfaceDescriptor::packed(Size::new(320, 480), PixelFormat::Argb8888);
        let mut surface = surfaces.open(&descriptor).unwrap();
        assert_eq!(surface.canvas().size(), Size::new(320, 480));

        surface.publish(&Damage::Full).unwrap();
        assert_eq!(surfaces.log().frames(), 1);
        assert_eq!(surfaces.log().published(), vec![Damage::Full]);
    }

    /// A host that describes a buffer too narrow for the width it claims gets
    /// an error, not a surface an app would write off the end of.
    #[test]
    fn an_impossible_descriptor_is_refused() {
        let mut surfaces = LocalSurfaces::new();
        let narrow = SurfaceDescriptor {
            extent: Size::new(100, 100),
            stride_bytes: 8,
            format: PixelFormat::Argb8888,
        };
        assert!(matches!(
            surfaces.open(&narrow),
            Err(SurfaceError::Unusable { .. })
        ));
    }
}
