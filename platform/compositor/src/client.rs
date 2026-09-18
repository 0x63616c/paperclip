//! Being a compositor client: connect, hand over a pool, speak
//! attach/commit/release (WWW-81).
//!
//! This is the other side of [`crate::server`]'s wire — a
//! [`paper_sdk::SurfaceProvider`] an app links instead of
//! [`paper_sdk::LocalSurfaces`], so [`paper_sdk::run`]'s Hello/Draw/Ready
//! lifecycle is unchanged and only *where the pixels go* differs. An app
//! using [`CompositorSurfaces`] is an ordinary client with no special access
//! to the panel: it never opens `paper_device`, never touches
//! `/tmp/epframebuffer.lock`, and learns nothing about what else is
//! connected.
//!
//! ## Why a `memfd`, not a temp file
//!
//! Paperclip installs nothing persistent (§8) and cleans up nothing on exit
//! by convention — a leaked temp file is exactly the kind of residue that
//! rule exists to prevent. `memfd_create` is anonymous and referenced only by
//! fd: closing every fd that names it (this process's and the compositor's
//! `dup`d copy) frees it with nothing to unlink.
//!
//! ## Buffer lifecycle, single-threaded
//!
//! An app's draw loop ([`paper_sdk::run`]) is synchronous: one thread,
//! one [`Surface::publish`] call per `Draw` request, no request outstanding
//! while the previous one is being answered. So a client only ever needs one
//! slot ahead of what the compositor has released, and blocking on
//! [`HostEvent::Released`] for the slot about to be reattached — rather than
//! building a second thread or a reactor to watch for it — is enough: by the
//! time this client asks for a slot back, the compositor's own release
//! notification (sent right after presenting, `server.rs`'s
//! `present_foreground`) is normally already sitting in the socket buffer.

#![allow(unsafe_code)]

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use paper_protocol::{BufferSlot, Damage, ShmPoolDescriptor, SurfaceDescriptor, codec};
use paper_sdk::Canvas;
use paper_sdk::{Surface, SurfaceError, SurfaceProvider};

use crate::fdpass;
use crate::pool::Pool;
use crate::wire::{ClientHello, ClientRequest, ClientRole, HostEvent};

/// Why a client could not become a compositor client.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
    /// The compositor's socket could not be reached.
    #[error("could not connect to the compositor at {path}: {source}")]
    Connect {
        /// The socket path that refused the connection.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The host described a surface this pool cannot be built from — the
    /// same terms [`SurfaceDescriptor::bytes`] refuses on.
    #[error("the surface descriptor does not describe a usable pool")]
    UnusableDescriptor,
    /// `memfd_create` failed.
    #[error("could not create the client's shared-memory pool: {0}")]
    Memfd(io::Error),
    /// Sizing the pool's backing memory failed.
    #[error("could not size the client's shared-memory pool: {0}")]
    Resize(io::Error),
    /// The hello frame, with its fd attached, could not be sent.
    #[error("could not send the client hello: {0}")]
    Hello(io::Error),
    /// The pool could not be mapped into this process.
    #[error(transparent)]
    Pool(#[from] crate::pool::PoolError),
}

/// The environment variable an app-launching unit sets to tell a client
/// which compositor socket to connect to.
///
/// A shared constant rather than a literal duplicated in
/// `platform/host/src/units.rs` (which sets it) and here (which reads it) on
/// purpose: ADR-0011 records what a merely-similar-looking duplicate name
/// cost once already (`RecoveryConfig`'s `wakelock_name`, WWW-35) — the fix
/// there was the same shape as this, one constant two crates reference
/// rather than two crates agreeing to spell the same string identically.
pub const SOCKET_ENV: &str = "PAPERCLIP_COMPOSITOR_SOCKET";

/// Where a client connects when [`SOCKET_ENV`] is not set — `paperctl dev`,
/// the render test card run by hand, or any other launch that predates a
/// real host setting the environment for it.
pub const DEFAULT_SOCKET: &str = "/tmp/paperclip-compositor.sock";

/// [`SOCKET_ENV`] if set, otherwise [`DEFAULT_SOCKET`].
pub fn socket_path() -> PathBuf {
    std::env::var_os(SOCKET_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET))
}

/// Opens [`CompositorSurface`]s by connecting to the compositor's
/// well-known socket — one per process, matching every app's single,
/// windowless viewport (WWW-81: "windowing is explicitly out of scope").
#[derive(Debug, Clone)]
pub struct CompositorSurfaces {
    socket: PathBuf,
    role: ClientRole,
    label: String,
}

impl CompositorSurfaces {
    /// A provider that will connect to `socket`, registering as `role` under
    /// `label` — the name a crashed or killed client's stopped frame shows
    /// (`crate::stopped`).
    pub fn new(socket: impl Into<PathBuf>, role: ClientRole, label: impl Into<String>) -> Self {
        Self {
            socket: socket.into(),
            role,
            label: label.into(),
        }
    }
}

impl SurfaceProvider for CompositorSurfaces {
    type Surface = CompositorSurface;

    fn open(&mut self, descriptor: &SurfaceDescriptor) -> Result<Self::Surface, SurfaceError> {
        CompositorSurface::connect(&self.socket, self.role, &self.label, *descriptor).map_err(
            |error| {
                tracing::error!(%error, socket = %self.socket.display(), "could not become a compositor client");
                SurfaceError::Allocation {
                    width: descriptor.extent.width,
                    height: descriptor.extent.height,
                }
            },
        )
    }
}

/// A bound compositor client connection: the pool it shares with the
/// compositor, and the canvas an app draws into before each publish copies
/// it into whichever slot is free.
#[derive(Debug)]
pub struct CompositorSurface {
    stream: UnixStream,
    pool: Pool,
    canvas: Canvas,
    next: BufferSlot,
    /// Whether each slot (indexed by `slot_index`) is still with the
    /// compositor, awaiting `Released`.
    in_flight: [bool; 2],
}

impl CompositorSurface {
    fn connect(
        socket: &Path,
        role: ClientRole,
        label: &str,
        descriptor: SurfaceDescriptor,
    ) -> Result<Self, ConnectError> {
        if descriptor.bytes().is_none() {
            return Err(ConnectError::UnusableDescriptor);
        }
        let pool_descriptor = ShmPoolDescriptor { buffer: descriptor };
        let pool_bytes = pool_descriptor
            .pool_bytes()
            .ok_or(ConnectError::UnusableDescriptor)?;

        let stream = UnixStream::connect(socket).map_err(|source| ConnectError::Connect {
            path: socket.to_path_buf(),
            source,
        })?;

        let fd = new_memfd(pool_bytes)?;
        let hello = ClientHello {
            role,
            label: label.to_owned(),
            pool: pool_descriptor,
        };
        let body = codec::encode(&hello)
            .map_err(|error| ConnectError::Hello(io::Error::other(error.to_string())))?;
        let mut frame = Vec::with_capacity(codec::LENGTH_PREFIX_BYTES + body.len());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(&body);
        fdpass::send_with_fd(&stream, &frame, fd.as_raw_fd()).map_err(ConnectError::Hello)?;

        // SAFETY: `fd` was just created by `new_memfd`, is open, owned by
        // this function, and sized to exactly `pool_bytes` — the same
        // contract `Pool::from_file` documents for its `file` parameter.
        // `send_with_fd` dup's the descriptor into the compositor rather
        // than consuming it, so `fd` is still ours to hand to `File`.
        let file = std::fs::File::from(fd);
        let pool = Pool::from_file(pool_descriptor, file)?;

        let canvas = Canvas::new(descriptor.extent).ok_or(ConnectError::UnusableDescriptor)?;

        Ok(Self {
            stream,
            pool,
            canvas,
            next: BufferSlot::A,
            in_flight: [false, false],
        })
    }

    /// Blocks until `slot` is confirmed released, if it is not already free.
    fn wait_for_release(&mut self, slot: BufferSlot) -> Result<(), SurfaceError> {
        if !self.in_flight[slot_index(slot)] {
            return Ok(());
        }
        loop {
            let event: HostEvent = codec::read_message(&mut self.stream).map_err(|error| {
                tracing::error!(%error, "lost the compositor connection waiting for a release");
                SurfaceError::Allocation {
                    width: self.canvas.size().width,
                    height: self.canvas.size().height,
                }
            })?;
            let HostEvent::Released(released) = event;
            self.in_flight[slot_index(released)] = false;
            if released == slot {
                return Ok(());
            }
        }
    }
}

impl Surface for CompositorSurface {
    fn canvas(&mut self) -> &mut Canvas {
        &mut self.canvas
    }

    fn publish(&mut self, damage: &Damage) -> Result<(), SurfaceError> {
        let slot = self.next;
        self.wait_for_release(slot)?;

        let bytes = self.pool.slot_mut(slot);
        // SAFETY: `bytes` is a slice into this client's own `mmap`ed pool,
        // page-aligned at its start; `BufferSlot::B`'s offset is one slot's
        // byte length, itself a multiple of 4 (`PixelFormat::Argb8888` is 4
        // bytes/pixel and `SurfaceDescriptor::packed`'s stride is
        // `width * 4`) — so both slots start at a 4-byte-aligned offset.
        // `bytes.len()` is exactly `slot_bytes()`, a whole number of
        // `u32`s by the same reasoning, so the reinterpreted slice covers
        // precisely the mapped bytes with no partial trailing element.
        let pixels: &mut [u32] = unsafe {
            std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<u32>(), bytes.len() / 4)
        };
        if !self.canvas.fill_argb8888(pixels) {
            return Err(SurfaceError::Allocation {
                width: self.canvas.size().width,
                height: self.canvas.size().height,
            });
        }

        write_request(&mut self.stream, &ClientRequest::Attach(slot))?;
        write_request(&mut self.stream, &ClientRequest::Commit(damage.clone()))?;
        self.in_flight[slot_index(slot)] = true;
        self.next = slot.other();
        Ok(())
    }
}

fn write_request(stream: &mut UnixStream, request: &ClientRequest) -> Result<(), SurfaceError> {
    codec::write_message(stream, request).map_err(|error| {
        tracing::error!(%error, "could not reach the compositor");
        SurfaceError::Allocation {
            width: 0,
            height: 0,
        }
    })
}

const fn slot_index(slot: BufferSlot) -> usize {
    match slot {
        BufferSlot::A => 0,
        BufferSlot::B => 1,
    }
}

/// Creates anonymous backing storage for a client pool, sized to exactly
/// `bytes`.
///
/// `memfd_create` on Linux — the device, and the VM harness. Elsewhere (the
/// Mac desktop preview, this crate's own tests) it does not exist, so a
/// plain temp file, unlinked the instant it is created, stands in: closing
/// every fd that names it frees it with nothing left in a directory, the
/// same property `memfd_create` gives for free on the device.
#[cfg(target_os = "linux")]
fn new_memfd(bytes: u64) -> Result<OwnedFd, ConnectError> {
    use std::os::fd::FromRawFd;

    let name = c"paperclip-client-pool";
    // SAFETY: `name` is a valid, NUL-terminated C string live for the call;
    // `memfd_create` either returns a valid owned fd or -1 with `errno` set,
    // both handled below before the value is trusted.
    let raw = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    if raw < 0 {
        return Err(ConnectError::Memfd(io::Error::last_os_error()));
    }
    // SAFETY: `raw` was just returned by `memfd_create` as a fresh fd this
    // function has not yet handed to anything else, so taking ownership here
    // is the only claim on it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    resize(&fd, bytes)?;
    Ok(fd)
}

#[cfg(not(target_os = "linux"))]
fn new_memfd(bytes: u64) -> Result<OwnedFd, ConnectError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "paperclip-client-pool-{}-{unique}",
        std::process::id()
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map_err(ConnectError::Memfd)?;
    let _ = std::fs::remove_file(&path);
    let fd = OwnedFd::from(file);
    resize(&fd, bytes)?;
    Ok(fd)
}

fn resize(fd: &OwnedFd, bytes: u64) -> Result<(), ConnectError> {
    let bytes = i64::try_from(bytes).map_err(|_| {
        ConnectError::Resize(io::Error::other(
            "pool size does not fit in a signed 64-bit length",
        ))
    })?;
    // SAFETY: `fd` names an open file this function was handed ownership of
    // (or a live borrow of it); `ftruncate` only ever sizes the file it
    // names.
    let result = unsafe { libc::ftruncate(fd.as_raw_fd(), bytes) };
    if result != 0 {
        return Err(ConnectError::Resize(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CompositorSurface, ConnectError};
    use crate::server::Compositor;
    use crate::wire::ClientRole;
    use paper_device::MemoryPanel;
    use paper_protocol::{Damage, PixelFormat, Size, SurfaceDescriptor};
    use paper_sdk::Surface;
    use std::time::Duration;

    fn socket_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "paper-compositor-client-test-{name}-{}-{}",
            std::process::id(),
            name.len()
        ))
    }

    #[test]
    fn connecting_registers_a_client_the_compositor_can_see() {
        let path = socket_path("connect");
        let _ = std::fs::remove_file(&path);
        let mut compositor =
            Compositor::bind(&path, Box::new(MemoryPanel::new(Size::new(64, 64)))).unwrap();

        let descriptor = SurfaceDescriptor::packed(Size::new(16, 16), PixelFormat::Argb8888);
        // `CompositorSurface` holds a `Pool`, which is `!Send` (it wraps a
        // raw `NonNull` mapping) — the same reason the compositor's own
        // clients never leave the thread that accepted them. The connect
        // result crosses back as a `bool`, not the value itself.
        let client = std::thread::spawn(move || {
            CompositorSurface::connect(&path, ClientRole::App, "test-client", descriptor).is_ok()
        });

        // Give the compositor's accept loop a chance to run the handshake.
        let mut connected = false;
        for _ in 0..50 {
            let events = compositor.run_once(Duration::from_millis(20));
            if events.iter().any(|event| {
                matches!(
                    event,
                    crate::server::CompositorEvent::ClientConnected { .. }
                )
            }) {
                connected = true;
                break;
            }
        }
        assert!(connected, "the compositor never saw the handshake complete");
        assert!(client.join().unwrap(), "the client failed to connect");
    }

    #[test]
    fn publishing_attaches_commits_and_alternates_slots() {
        let path = socket_path("publish");
        let _ = std::fs::remove_file(&path);
        let mut compositor =
            Compositor::bind(&path, Box::new(MemoryPanel::new(Size::new(16, 16)))).unwrap();

        let descriptor = SurfaceDescriptor::packed(Size::new(16, 16), PixelFormat::Argb8888);
        let connect_path = path.clone();
        let client = std::thread::spawn(move || {
            let mut surface = CompositorSurface::connect(
                &connect_path,
                ClientRole::App,
                "test-client",
                descriptor,
            )
            .expect("connects");
            surface.canvas().clear(paper_sdk::palette::INK);
            surface.publish(&Damage::Full).expect("first publish");
            surface.canvas().clear(paper_sdk::palette::PAPER);
            surface.publish(&Damage::Full).expect("second publish");
        });

        let mut frames = 0;
        for _ in 0..100 {
            let events = compositor.run_once(Duration::from_millis(20));
            frames += events
                .iter()
                .filter(|event| {
                    matches!(event, crate::server::CompositorEvent::FramePresented { .. })
                })
                .count();
            if frames >= 2 {
                break;
            }
        }
        assert_eq!(frames, 2, "expected both publishes to reach the panel");
        client.join().unwrap();
    }

    #[test]
    fn an_unreachable_socket_is_a_connect_error() {
        let path = socket_path("missing");
        let _ = std::fs::remove_file(&path);
        let descriptor = SurfaceDescriptor::packed(Size::new(4, 4), PixelFormat::Argb8888);
        let error = CompositorSurface::connect(&path, ClientRole::App, "nobody-home", descriptor)
            .expect_err("nothing is listening");
        assert!(matches!(error, ConnectError::Connect { .. }), "{error:?}");
    }
}
