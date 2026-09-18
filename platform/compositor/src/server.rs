//! The compositor's accept loop: EOF teardown, non-blocking client I/O,
//! crash/hang isolation (WWW-78), and the sleep/lock overlay (WWW-82).
//!
//! ## Sleep and lock stop presenting without disturbing state
//!
//! [`Compositor::sleep`] draws [`crate::sleep::render_lock_frame`] and sets
//! an internal flag; `present_foreground` checks that flag before every
//! blit and, while set, releases a committed buffer without ever reading
//! it. A client's `Surface`/`Pool` keep advancing exactly as they would
//! unlocked — attaching, damaging, committing — so [`Compositor::wake`] has
//! an up-to-date frame to present the instant it clears the flag, rather
//! than a stale one from the moment sleep began. A foreground client dying
//! while locked (`disconnect`) is handled the same way: the state
//! transition to `Foreground::Stopped` happens immediately, but the
//! stopped frame itself is not painted until `wake`.
//!
//! ## Non-blocking is the whole answer to "a wedged client stalls nobody"
//!
//! Every fd this module touches — the listener, a pending connection, a
//! registered client — is set non-blocking the moment it exists. `poll`
//! (`libc::poll`, not `mio`/`tokio`: this workspace has no async runtime,
//! see `platform/compositor/Cargo.toml`) is the only place [`Compositor`]
//! ever waits, and it waits on every fd at once with one bounded timeout. A
//! client that never sends anything simply never comes back from `poll` as
//! readable — it costs one entry in a `pollfd` array, not a blocked `read`
//! call that starves every other client. [`HELLO_DEADLINE`] exists only to
//! stop that "costs one entry forever" case from being a slow resource
//! leak; it is not what makes the loop non-blocking.
//!
//! ## EOF teardown touches exactly the dead client
//!
//! `Compositor::disconnect` removes one entry from `clients`, dropping its
//! [`Surface`] and [`Pool`] (which `munmap`s, see `pool.rs`) and closing its
//! socket. Nothing else in [`Compositor`] — the listener, `poll`'s fd list
//! for every other client, the panel — is touched by that call, which is
//! what "the compositor's event loop and panel fd must be untouched by this"
//! (WWW-78) means structurally rather than by convention.
//!
//! `libc::poll` is this module's only real system call — following
//! `pool.rs`'s and `fdpass.rs`'s pattern: `#![allow(unsafe_code)]` at the
//! module, a written reason on the block.

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use paper_device::{Panel, Waveform};
use paper_protocol::codec::{self, CodecError};
use paper_protocol::{BufferSlot, MAX_MESSAGE_BYTES};

use crate::fdpass;
use crate::pool::{Pool, PoolError};
use crate::present::present_pool_slot;
use crate::sleep::render_lock_frame;
use crate::stopped::render_stopped_frame;
use crate::surface::Surface;
use crate::wire::{ClientRequest, ClientRole, HostEvent};

/// How long a connection may sit without completing its `ClientHello` before
/// the compositor drops it.
///
/// A connection that never says who it is is exactly the "wedged" case the
/// non-blocking design above tolerates without stalling anything else — but
/// tolerating it *forever* is a slow resource leak, not isolation. Sized
/// like `paper_protocol::limits::READY_DEADLINE` (an app's analogous
/// "identify yourself" deadline): generous for a process whose only job at
/// this point is to send one small message.
pub const HELLO_DEADLINE: Duration = Duration::from_secs(2);

/// Identifies one connected client for the lifetime of its connection.
///
/// Assigned by the compositor, monotonically — never the raw fd, which is
/// reused by the kernel the moment a connection closes and would otherwise
/// let a stale [`ClientId`] silently name a different client later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(u64);

/// Which surface the panel is currently showing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Foreground {
    /// Nothing has connected yet.
    None,
    /// This client is presenting.
    Client(ClientId),
    /// A client died or was dropped while foreground and nothing has taken
    /// its place — `label` names it, from its own `ClientHello`.
    Stopped { label: String },
}

/// One connected client: its role, its buffer lifecycle, and the transport
/// it arrived on.
#[derive(Debug)]
struct Client {
    role: ClientRole,
    label: String,
    stream: UnixStream,
    surface: Surface,
    pool: Pool,
    reader: FrameReader,
}

/// A connection that has not yet completed its `ClientHello`.
#[derive(Debug)]
struct Pending {
    stream: UnixStream,
    connected_at: Instant,
}

/// Incremental framing over a non-blocking fd: accumulates bytes across
/// reads that may stop mid-frame, and hands back exactly the frames that
/// have fully arrived.
///
/// Reusing `paper_protocol::codec::read_message` here would silently corrupt
/// framing the first time a non-blocking read stops mid-body: it has no way
/// to remember "the 4-byte prefix is already consumed, N body bytes are
/// still owed" across two separate calls, so the next call would read a
/// fresh 4-byte prefix from what is actually the middle of the previous
/// frame's body.
#[derive(Debug, Default)]
struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    fn feed(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Pops the oldest complete frame's body, if one has fully arrived.
    fn take_frame(&mut self) -> Result<Option<Vec<u8>>, CodecError> {
        if self.buf.len() < codec::LENGTH_PREFIX_BYTES {
            return Ok(None);
        }
        let len =
            u32::from_le_bytes(self.buf[..codec::LENGTH_PREFIX_BYTES].try_into().unwrap()) as usize;
        if len > MAX_MESSAGE_BYTES {
            return Err(CodecError::TooLarge {
                len: len as u64,
                max: MAX_MESSAGE_BYTES,
            });
        }
        let frame_len = codec::LENGTH_PREFIX_BYTES + len;
        if self.buf.len() < frame_len {
            return Ok(None);
        }
        let body = self.buf[codec::LENGTH_PREFIX_BYTES..frame_len].to_vec();
        self.buf.drain(..frame_len);
        Ok(Some(body))
    }
}

/// Why a connection's `ClientHello` handshake failed.
#[derive(Debug, thiserror::Error)]
enum HandshakeError {
    #[error("hello frame was truncated")]
    Truncated,
    #[error("hello frame was not a valid ClientHello: {0}")]
    Malformed(CodecError),
    #[error("hello arrived with no fd attached")]
    NoFd,
    #[error("hello's pool descriptor was invalid: {0}")]
    Pool(PoolError),
}

/// Decodes the `ClientHello` frame at the start of `body`, returning it
/// alongside how many bytes it occupied.
///
/// The caller's `recvmsg` reads whatever the kernel already has buffered,
/// which — on a fast local socket — is often the hello *and* whatever the
/// client wrote immediately after it (an `Attach`/`Commit` pair, for a
/// client that draws its first frame before waiting for anything back).
/// Reporting the consumed length is what lets [`Compositor::finish_handshake`]
/// hand those trailing bytes to the new client's [`FrameReader`] instead of
/// silently dropping them.
fn decode_hello(body: &[u8]) -> Result<(crate::wire::ClientHello, usize), HandshakeError> {
    if body.len() < codec::LENGTH_PREFIX_BYTES {
        return Err(HandshakeError::Truncated);
    }
    let len = u32::from_le_bytes(body[..codec::LENGTH_PREFIX_BYTES].try_into().unwrap()) as usize;
    let frame_end = codec::LENGTH_PREFIX_BYTES + len;
    if body.len() < frame_end {
        return Err(HandshakeError::Truncated);
    }
    let hello = codec::decode(&body[codec::LENGTH_PREFIX_BYTES..frame_end])
        .map_err(HandshakeError::Malformed)?;
    Ok((hello, frame_end))
}

/// Why binding a compositor socket failed.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// The socket path could not be bound.
    #[error("failed to bind {path}: {source}")]
    Bind {
        /// The path that was refused.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The listener could not be set non-blocking.
    #[error("failed to make the listener non-blocking: {0}")]
    NonBlocking(io::Error),
}

/// What happened during one [`Compositor::run_once`] tick, for a caller (or
/// a test) to react to without reaching into private state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompositorEvent {
    /// A client completed its handshake and is now tracked.
    ClientConnected {
        /// The new client.
        id: ClientId,
        /// The role it registered as.
        role: ClientRole,
        /// The name it gave.
        label: String,
    },
    /// A pending or registered connection ended — EOF, a protocol violation,
    /// or the [`HELLO_DEADLINE`] expiring on an unregistered one.
    ///
    /// `id`/`label` are `None` for a connection that never finished its
    /// handshake: it was never assigned a [`ClientId`] or a label to report.
    ClientDisconnected {
        /// The client that disconnected, if it had registered.
        id: Option<ClientId>,
        /// Its label, if it had registered.
        label: Option<String>,
    },
    /// The foreground client committed a frame that was presented.
    FramePresented {
        /// The client whose frame was presented.
        id: ClientId,
    },
    /// The foreground client died or was dropped while showing; its stopped
    /// frame was presented in its place.
    ForegroundStopped {
        /// The label the stopped frame names.
        label: String,
    },
    /// The panel returned to the registered Home client.
    ForegroundHome {
        /// Home's client id.
        id: ClientId,
    },
    /// [`Compositor::sleep`] drew the lock screen.
    Locked,
    /// [`Compositor::wake`] cleared the lock screen.
    Unlocked,
}

/// One fd [`Compositor::run_once`] is waiting on, and what it is for.
enum PollTarget {
    Listener,
    Pending(RawFd),
    Client(ClientId),
}

/// The compositor: owns the panel exclusively, accepts client connections,
/// and tears one down the moment it dies or looks wedged rather than ever
/// blocking on it (WWW-52, WWW-77, WWW-78).
#[derive(Debug)]
pub struct Compositor {
    listener: UnixListener,
    panel: Box<dyn Panel>,
    pending: HashMap<RawFd, Pending>,
    clients: HashMap<ClientId, Client>,
    home: Option<ClientId>,
    foreground: Foreground,
    next_id: u64,
    /// Whether the lock screen is currently covering the panel (WWW-82).
    locked: bool,
}

impl Compositor {
    /// Binds a compositor socket at `path` and takes exclusive ownership of
    /// `panel`.
    pub fn bind(path: &Path, panel: Box<dyn Panel>) -> Result<Self, BindError> {
        let listener = UnixListener::bind(path).map_err(|source| BindError::Bind {
            path: path.to_path_buf(),
            source,
        })?;
        listener
            .set_nonblocking(true)
            .map_err(BindError::NonBlocking)?;
        Ok(Self {
            listener,
            panel,
            pending: HashMap::new(),
            clients: HashMap::new(),
            home: None,
            foreground: Foreground::None,
            next_id: 0,
            locked: false,
        })
    }

    /// The client currently presenting, if any.
    pub fn foreground_client(&self) -> Option<ClientId> {
        match &self.foreground {
            Foreground::Client(id) => Some(*id),
            _ => None,
        }
    }

    /// The label a stopped frame is currently showing, if the panel is
    /// showing one.
    pub fn stopped_label(&self) -> Option<&str> {
        match &self.foreground {
            Foreground::Stopped { label } => Some(label),
            _ => None,
        }
    }

    /// The registered Home client, if one is connected.
    pub fn home_client(&self) -> Option<ClientId> {
        self.home
    }

    /// Whether the lock screen is currently covering the panel (WWW-82).
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Draws the lock screen over whatever is currently foreground and stops
    /// presenting client frames — see `present_foreground` — until
    /// [`Self::wake`] clears it.
    ///
    /// A no-op past the first call in a row: sleeping again while already
    /// locked would otherwise cost a redundant full refresh for no visible
    /// change. What triggers this call — an idle timer, a power button, a
    /// real suspend signal — is not this crate's job; see `sleep`'s module
    /// doc.
    pub fn sleep(&mut self, events: &mut Vec<CompositorEvent>) {
        if self.locked {
            return;
        }
        match render_lock_frame(self.panel.as_mut()) {
            Ok(()) => {
                self.locked = true;
                events.push(CompositorEvent::Locked);
            }
            Err(err) => tracing::error!(?err, "rendering the lock frame failed"),
        }
    }

    /// Clears the lock screen and re-presents whatever the panel would be
    /// showing had [`Self::sleep`] never been called — the foreground
    /// client's latest committed frame, or a stopped frame if the foreground
    /// client died while locked.
    ///
    /// A no-op if not currently locked.
    pub fn wake(&mut self, events: &mut Vec<CompositorEvent>) {
        if !self.locked {
            return;
        }
        self.locked = false;
        events.push(CompositorEvent::Unlocked);
        match &self.foreground {
            Foreground::Client(_) => self.present_foreground(events),
            Foreground::Stopped { label } => {
                let label = label.clone();
                if let Err(err) = render_stopped_frame(self.panel.as_mut(), &label) {
                    tracing::error!(?err, "re-rendering the stopped frame on wake failed");
                }
            }
            Foreground::None => {}
        }
    }

    /// The role a connected client registered as.
    pub fn client_role(&self, id: ClientId) -> Option<ClientRole> {
        self.clients.get(&id).map(|client| client.role)
    }

    /// Whether `id` is still a connected, registered client.
    pub fn is_connected(&self, id: ClientId) -> bool {
        self.clients.contains_key(&id)
    }

    /// The panel this compositor owns exclusively.
    pub fn panel(&self) -> &dyn Panel {
        self.panel.as_ref()
    }

    /// Mutable access to the panel this compositor owns exclusively.
    ///
    /// For shutdown only (WWW-81): `paperclip-compositor`'s `main` clears the
    /// panel before this value is dropped, following ADR-0009's shutdown
    /// ordering. Nothing inside this crate's own event loop needs this — it
    /// reaches the panel only through its own private `present_foreground`
    /// and [`crate::stopped::render_stopped_frame`], both driven by `run_once`.
    pub fn panel_mut(&mut self) -> &mut dyn Panel {
        self.panel.as_mut()
    }

    /// The label a connected client registered under, if `id` is still
    /// connected.
    pub fn client_label(&self, id: ClientId) -> Option<&str> {
        self.clients.get(&id).map(|client| client.label.as_str())
    }

    /// Runs one iteration: accepts new connections, services every fd
    /// `poll` reports ready within `timeout`, and reaps any pending
    /// connection that has outlived [`HELLO_DEADLINE`]. Never blocks longer
    /// than `timeout`, and never blocks at all on a single fd.
    pub fn run_once(&mut self, timeout: Duration) -> Vec<CompositorEvent> {
        let mut events = Vec::new();
        let ready = match self.poll(timeout) {
            Ok(ready) => ready,
            Err(err) => {
                tracing::error!(?err, "poll failed");
                return events;
            }
        };

        for target in ready {
            match target {
                PollTarget::Listener => self.accept_all(),
                PollTarget::Pending(fd) => self.service_pending(fd, &mut events),
                PollTarget::Client(id) => self.service_client(id, &mut events),
            }
        }

        self.reap_expired_pending(&mut events);
        events
    }

    fn poll(&self, timeout: Duration) -> io::Result<Vec<PollTarget>> {
        let mut fds = Vec::with_capacity(1 + self.pending.len() + self.clients.len());
        let mut targets = Vec::with_capacity(fds.capacity());

        fds.push(libc::pollfd {
            fd: self.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        targets.push(PollTarget::Listener);

        for (&fd, pending) in &self.pending {
            fds.push(libc::pollfd {
                fd: pending.stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            targets.push(PollTarget::Pending(fd));
        }

        for (&id, client) in &self.clients {
            fds.push(libc::pollfd {
                fd: client.stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            targets.push(PollTarget::Client(id));
        }

        let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `fds` is a valid pointer to `fds.len()` initialised
        // `pollfd`s for the duration of this call. `poll` reads `fd`/`events`
        // and writes `revents` in place; it neither resizes nor frees the
        // buffer.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(fds
            .into_iter()
            .zip(targets)
            .filter(|(fd, _)| fd.revents != 0)
            .map(|(_, target)| target)
            .collect())
    }

    fn accept_all(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, _addr)) => match stream.set_nonblocking(true) {
                    Ok(()) => {
                        let fd = stream.as_raw_fd();
                        self.pending.insert(
                            fd,
                            Pending {
                                stream,
                                connected_at: Instant::now(),
                            },
                        );
                    }
                    Err(err) => {
                        tracing::warn!(?err, "could not make an accepted connection non-blocking");
                    }
                },
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => {
                    tracing::warn!(?err, "accept failed");
                    break;
                }
            }
        }
    }

    fn service_pending(&mut self, fd: RawFd, events: &mut Vec<CompositorEvent>) {
        let Some(pending) = self.pending.remove(&fd) else {
            return;
        };
        let mut buf = vec![0u8; MAX_MESSAGE_BYTES + codec::LENGTH_PREFIX_BYTES];
        match fdpass::recv_with_fd(&pending.stream, &mut buf) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                // A spurious wakeup: still waiting for the rest of the
                // handshake, so put it back exactly as it was.
                self.pending.insert(fd, pending);
            }
            Err(_) | Ok((0, _)) => {
                events.push(CompositorEvent::ClientDisconnected {
                    id: None,
                    label: None,
                });
            }
            Ok((n, owned_fd)) => {
                self.finish_handshake(pending.stream, &buf[..n], owned_fd, events);
            }
        }
    }

    fn finish_handshake(
        &mut self,
        stream: UnixStream,
        body: &[u8],
        fd: Option<std::os::fd::OwnedFd>,
        events: &mut Vec<CompositorEvent>,
    ) {
        let outcome = decode_hello(body).and_then(|(hello, consumed)| {
            let file = File::from(fd.ok_or(HandshakeError::NoFd)?);
            let pool = Pool::from_file(hello.pool, file).map_err(HandshakeError::Pool)?;
            Ok((hello, pool, consumed))
        });

        let (hello, pool, consumed) = match outcome {
            Ok(triple) => triple,
            Err(err) => {
                tracing::warn!(?err, "a connection's ClientHello was refused");
                events.push(CompositorEvent::ClientDisconnected {
                    id: None,
                    label: None,
                });
                return;
            }
        };

        let id = self.next_client_id();
        let surface = Surface::new(hello.pool);
        // Whatever `body` held past the hello's own frame — an eager
        // client's `Attach`/`Commit`, already sitting in the same
        // `recvmsg` read — belongs to this client's ongoing framing, not
        // to the hello. Seeding the reader with it before this client is
        // ever polled again is what stops it being silently lost.
        let mut reader = FrameReader::default();
        reader.feed(&body[consumed..]);
        self.clients.insert(
            id,
            Client {
                role: hello.role,
                label: hello.label.clone(),
                stream,
                surface,
                pool,
                reader,
            },
        );
        if hello.role == ClientRole::Home {
            self.home = Some(id);
        }
        if self.foreground == Foreground::None || hello.role == ClientRole::App {
            self.foreground = Foreground::Client(id);
        }
        events.push(CompositorEvent::ClientConnected {
            id,
            role: hello.role,
            label: hello.label,
        });
        self.drain_client_frames(id, events);
    }

    fn service_client(&mut self, id: ClientId, events: &mut Vec<CompositorEvent>) {
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        let mut chunk = [0u8; 8192];
        let read = match (&client.stream).read(&mut chunk) {
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return,
            Err(_) => {
                self.disconnect(id, events);
                return;
            }
        };
        if read == 0 {
            self.disconnect(id, events);
            return;
        }
        client.reader.feed(&chunk[..read]);
        self.drain_client_frames(id, events);
    }

    /// Decodes and dispatches every complete [`ClientRequest`] frame already
    /// sitting in `id`'s [`FrameReader`], stopping at the first incomplete
    /// one. Shared by [`Self::service_client`] (fed by a fresh non-blocking
    /// `read`) and [`Self::finish_handshake`] (fed by whatever arrived
    /// bundled with the hello) — both end up with the same obligation: drain
    /// whatever framing is already buffered before waiting on `poll` again.
    fn drain_client_frames(&mut self, id: ClientId, events: &mut Vec<CompositorEvent>) {
        loop {
            let Some(client) = self.clients.get_mut(&id) else {
                return;
            };
            match client.reader.take_frame() {
                Ok(Some(body)) => match codec::decode::<ClientRequest>(&body) {
                    Ok(request) => self.handle_request(id, request, events),
                    Err(err) => {
                        tracing::warn!(?err, "a client sent an unparseable request");
                        self.disconnect(id, events);
                        return;
                    }
                },
                Ok(None) => return,
                Err(err) => {
                    tracing::warn!(?err, "a client's frame was over the message limit");
                    self.disconnect(id, events);
                    return;
                }
            }
        }
    }

    fn handle_request(
        &mut self,
        id: ClientId,
        request: ClientRequest,
        events: &mut Vec<CompositorEvent>,
    ) {
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        match request {
            ClientRequest::Attach(slot) => {
                if client.surface.attach(slot).is_err() {
                    // A client attaching a slot the host has not released is
                    // exactly the corruption `wl_buffer.release` exists to
                    // prevent (WWW-77) — fail closed rather than let it
                    // retry into the same violation.
                    self.disconnect(id, events);
                }
            }
            ClientRequest::Commit(damage) => match client.surface.commit(damage) {
                Ok(_slot) => {
                    if self.foreground_client() == Some(id) {
                        self.present_foreground(events);
                    }
                }
                Err(_) => self.disconnect(id, events),
            },
        }
    }

    fn present_foreground(&mut self, events: &mut Vec<CompositorEvent>) {
        let Some(id) = self.foreground_client() else {
            return;
        };
        let Some(client) = self.clients.get(&id) else {
            return;
        };
        let Some(committed) = client.surface.current() else {
            return;
        };
        let slot = committed.buffer;

        if self.locked {
            // The lock screen owns the panel while sleeping (WWW-82): still
            // release the slot, so a client that keeps committing does not
            // stall on its two-buffer pool waiting for a presentation that
            // will not happen until `wake`. Nothing reads `slot`'s bytes on
            // this path, so releasing without presenting is safe.
            if let Some(client) = self.clients.get_mut(&id) {
                let _ = client.surface.release(slot);
                notify_release(&client.stream, slot);
            }
            return;
        }

        let descriptor = client.pool.descriptor();
        let result = present_pool_slot(
            self.panel.as_mut(),
            descriptor,
            client.pool.slot(slot),
            Waveform::CONTENT,
        );
        match result {
            Ok(()) => {
                events.push(CompositorEvent::FramePresented { id });
                if let Some(client) = self.clients.get_mut(&id) {
                    let _ = client.surface.release(slot);
                    notify_release(&client.stream, slot);
                }
            }
            Err(err) => tracing::error!(?err, "presenting the foreground client's frame failed"),
        }
    }

    fn disconnect(&mut self, id: ClientId, events: &mut Vec<CompositorEvent>) {
        let Some(client) = self.clients.remove(&id) else {
            return;
        };
        if self.home == Some(id) {
            self.home = None;
        }
        events.push(CompositorEvent::ClientDisconnected {
            id: Some(id),
            label: Some(client.label.clone()),
        });
        // `client` (its `Surface` and `Pool`) and its socket are dropped at
        // the end of this function's scope — the whole of this client's
        // resources, and nothing belonging to any other client or to the
        // listener.

        let was_foreground = matches!(&self.foreground, Foreground::Client(fg) if *fg == id);
        if !was_foreground {
            return;
        }

        let label = client.label;
        // While locked (WWW-82), the lock screen owns the panel: record the
        // state transition but do not paint over it. `wake` re-renders
        // whichever frame `self.foreground` says is current once it clears.
        if self.locked {
            events.push(CompositorEvent::ForegroundStopped {
                label: label.clone(),
            });
        } else {
            match render_stopped_frame(self.panel.as_mut(), &label) {
                Ok(()) => events.push(CompositorEvent::ForegroundStopped {
                    label: label.clone(),
                }),
                Err(err) => tracing::error!(?err, "rendering the stopped frame failed"),
            }
        }
        self.foreground = Foreground::Stopped { label };

        if let Some(home_id) = self.home {
            self.foreground = Foreground::Client(home_id);
            events.push(CompositorEvent::ForegroundHome { id: home_id });
            self.present_foreground(events);
        }
    }

    fn reap_expired_pending(&mut self, events: &mut Vec<CompositorEvent>) {
        let now = Instant::now();
        let expired: Vec<RawFd> = self
            .pending
            .iter()
            .filter(|(_, pending)| now.duration_since(pending.connected_at) >= HELLO_DEADLINE)
            .map(|(&fd, _)| fd)
            .collect();
        for fd in expired {
            self.pending.remove(&fd);
            events.push(CompositorEvent::ClientDisconnected {
                id: None,
                label: None,
            });
        }
    }

    fn next_client_id(&mut self) -> ClientId {
        let id = ClientId(self.next_id);
        self.next_id += 1;
        id
    }
}

/// Best-effort: tells `stream`'s peer that `slot` is free again.
///
/// Best-effort because no real client reads this yet (WWW-81) and because a
/// non-blocking write that cannot complete right now is exactly the
/// "wedged" case this ticket's design tolerates rather than blocks on — a
/// client too far behind to keep up with its own release notifications
/// loses one, which is a backpressure question out of this ticket's scope,
/// not a correctness one.
fn notify_release(stream: &UnixStream, slot: BufferSlot) {
    let mut writer = stream;
    if let Err(err) = codec::write_message(&mut writer, &HostEvent::Released(slot)) {
        tracing::debug!(?err, "could not notify a client its buffer was released");
    }
}
