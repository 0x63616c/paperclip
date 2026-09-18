//! WWW-78's two acceptance criteria, proven end to end against a real
//! `UnixListener` and real client peers:
//!
//! - Killing an app with `SIGKILL` leaves the panel showing a legible frame
//!   and returns to Home (`sigkilling_the_foreground_app_shows_a_stopped_frame_and_returns_to_home`,
//!   the one test here that spawns a real process and sends it a real
//!   signal, per the ticket naming `SIGKILL` specifically).
//! - A client that stops responding does not stall the compositor
//!   (`a_silent_client_does_not_stall_a_concurrently_active_one`,
//!   `an_unregistered_connection_is_reaped_after_the_hello_deadline_without_disturbing_another_client`).
//!
//! Every other test here uses an in-process `UnixStream` end dropped to
//! simulate EOF rather than a real process: the kernel-visible effect on the
//! compositor's socket — the peer's fd closing, `read` returning `Ok(0)` —
//! is identical whichever caused it, and a real `SIGKILL` test for every
//! scenario would mean a process per test for no additional coverage of the
//! code actually under test (`Compositor::disconnect`, which cannot tell the
//! two apart).

#![allow(unsafe_code)]

use std::io::{Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use paper_compositor::wire::{ClientHello, ClientRequest, ClientRole};
use paper_compositor::{Compositor, CompositorEvent, fdpass};
use paper_device::MemoryPanel;
use paper_protocol::{BufferSlot, Damage, PixelFormat, ShmPoolDescriptor, Size, SurfaceDescriptor};

const PANEL_SIZE: Size = Size::new(4, 4);
const TICK: Duration = Duration::from_millis(50);

/// Binds a compositor in a fresh temp directory, short enough to fit
/// `sockaddr_un`'s ~104-byte path limit (a timestamped name directly under
/// the system temp dir does not, on macOS's default `TMPDIR`). The returned
/// `TempDir` must stay alive for as long as the socket path needs to resolve
/// — dropping it deletes the directory a client would otherwise still be
/// able to `connect` to.
fn compositor() -> (Compositor, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sock");
    let panel = Box::new(MemoryPanel::new(PANEL_SIZE));
    (Compositor::bind(&path, panel).unwrap(), path, dir)
}

fn framed<T: serde::Serialize>(message: &T) -> Vec<u8> {
    let body = paper_protocol::codec::encode(message).unwrap();
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    frame
}

fn descriptor() -> ShmPoolDescriptor {
    ShmPoolDescriptor {
        buffer: SurfaceDescriptor::packed(PANEL_SIZE, PixelFormat::Argb8888),
    }
}

/// Connects to `path`, sends a `ClientHello`, and — if `pixel` is given —
/// fills the pool with it, attaches, and commits. Returns the connection and
/// the backing pool file, both of which must stay alive for as long as the
/// caller wants the compositor to still see this client as connected: this
/// mirrors what an app process's own open fds are for real.
fn connect(
    path: &PathBuf,
    role: ClientRole,
    label: &str,
    pixel: Option<u32>,
) -> (UnixStream, std::fs::File) {
    let stream = UnixStream::connect(path).unwrap();
    let mut pool_file = tempfile::tempfile().unwrap();
    let d = descriptor();
    pool_file.set_len(d.pool_bytes().unwrap()).unwrap();

    let hello = ClientHello {
        role,
        label: label.to_owned(),
        pool: d,
    };
    fdpass::send_with_fd(&stream, &framed(&hello), pool_file.as_raw_fd()).unwrap();

    if let Some(pixel) = pixel {
        let bytes: Vec<u8> = std::iter::repeat_with(|| pixel.to_ne_bytes())
            .take((PANEL_SIZE.width * PANEL_SIZE.height) as usize)
            .flatten()
            .collect();
        pool_file.write_all(&bytes).unwrap();

        let mut writer = &stream;
        paper_protocol::codec::write_message(&mut writer, &ClientRequest::Attach(BufferSlot::A))
            .unwrap();
        paper_protocol::codec::write_message(&mut writer, &ClientRequest::Commit(Damage::Full))
            .unwrap();
    }

    (stream, pool_file)
}

fn run_until<F: Fn(&[CompositorEvent]) -> bool>(
    compositor: &mut Compositor,
    deadline: Duration,
    done: F,
) -> Vec<CompositorEvent> {
    let start = Instant::now();
    let mut all = Vec::new();
    loop {
        let events = compositor.run_once(TICK);
        let is_done = done(&events);
        all.extend(events);
        if is_done || start.elapsed() >= deadline {
            return all;
        }
    }
}

/// Runs a fixed number of ticks unconditionally — enough for a just-sent
/// `ClientHello` and any follow-up `Attach`/`Commit` to each be picked up by
/// their own `poll` cycle (the accept, the handshake, and every message
/// after it can each land in a different tick; nothing here assumes they
/// arrive together).
fn pump(compositor: &mut Compositor, ticks: usize) -> Vec<CompositorEvent> {
    let mut all = Vec::new();
    for _ in 0..ticks {
        all.extend(compositor.run_once(TICK));
    }
    all
}

#[test]
fn a_committed_frame_from_the_foreground_app_reaches_the_panel() {
    let (mut compositor, path, _tmp) = compositor();
    let (_home_stream, _home_pool) = connect(&path, ClientRole::Home, "Home", None);
    pump(&mut compositor, 3);

    let pixel = 0xFF11_2233;
    let (_app_stream, _app_pool) = connect(&path, ClientRole::App, "Chess", Some(pixel));
    let events = run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. }))
    });

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. })),
        "{events:?}"
    );
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(readback.iter().all(|&p| p == pixel), "{readback:x?}");
}

/// WWW-77 built `Surface::release` and proved it against direct calls
/// (`surface.rs`'s own tests); this is the first time it runs over the real
/// wire this ticket adds. After a frame presents, the compositor must have
/// both released the slot internally (so the client may attach it again)
/// and told the client so via `HostEvent::Released` — proven by reading
/// that message back off the client's own stream, then actually reusing the
/// slot for a second, different frame.
#[test]
fn a_presented_buffer_is_released_and_notified_over_the_wire() {
    let (mut compositor, path, _tmp) = compositor();
    let (_home_stream, _home_pool) = connect(&path, ClientRole::Home, "Home", None);
    pump(&mut compositor, 3);

    let first_pixel = 0xFF11_2233;
    let (app_stream, mut app_pool) = connect(&path, ClientRole::App, "Chess", Some(first_pixel));
    run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. }))
    });

    let released: paper_compositor::HostEvent =
        paper_protocol::codec::read_message(&mut &app_stream)
            .expect("the client should have been told its buffer was released");
    assert_eq!(
        released,
        paper_compositor::HostEvent::Released(BufferSlot::A)
    );

    // Reuse the now-released slot for a second, different frame — refused
    // by `Surface::attach` if the earlier release never actually happened.
    let second_pixel = 0xFF66_9900u32;
    let bytes: Vec<u8> = std::iter::repeat_with(|| second_pixel.to_ne_bytes())
        .take((PANEL_SIZE.width * PANEL_SIZE.height) as usize)
        .flatten()
        .collect();
    app_pool.seek(std::io::SeekFrom::Start(0)).unwrap();
    app_pool.write_all(&bytes).unwrap();
    let mut writer = &app_stream;
    paper_protocol::codec::write_message(&mut writer, &ClientRequest::Attach(BufferSlot::A))
        .unwrap();
    paper_protocol::codec::write_message(&mut writer, &ClientRequest::Commit(Damage::Full))
        .unwrap();

    let events = run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. })),
        "{events:?}"
    );
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(readback.iter().all(|&p| p == second_pixel), "{readback:x?}");
}

/// The first acceptance criterion this ticket names: killing an app leaves
/// the panel showing a legible frame and returns to Home.
///
/// Uses a dropped in-process `UnixStream` to simulate the kernel closing a
/// killed client's socket — see the module doc for why that is equivalent
/// to what `Compositor::disconnect` sees from a real `SIGKILL`. The
/// process-level version of this same scenario is
/// `sigkilling_the_foreground_app_shows_a_stopped_frame_and_returns_to_home`,
/// below.
#[test]
fn eof_on_the_foreground_app_shows_a_stopped_frame_and_returns_to_home() {
    let (mut compositor, path, _tmp) = compositor();
    let home_pixel = 0xFF00_FF00;
    let (_home_stream, _home_pool) = connect(&path, ClientRole::Home, "Home", Some(home_pixel));
    pump(&mut compositor, 5);
    let home_id = compositor.foreground_client().expect("home is foreground");

    let (app_stream, _app_pool) = connect(&path, ClientRole::App, "Chess", Some(0xFF44_2266));
    pump(&mut compositor, 5);
    let app_id = compositor.foreground_client().expect("app is foreground");
    assert_ne!(app_id, home_id);

    drop(app_stream); // the kernel-visible effect of the app process dying

    let events = run_until(&mut compositor, Duration::from_secs(2), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ForegroundHome { .. }))
    });

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ClientDisconnected { id: Some(id), .. } if *id == app_id)),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ForegroundStopped { label } if label == "Chess")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ForegroundHome { id } if *id == home_id)),
        "{events:?}"
    );
    assert!(!compositor.is_connected(app_id));
    assert_eq!(compositor.foreground_client(), Some(home_id));

    // The panel's last swap is Home's frame: the stopped frame is what the
    // user saw in between, and this proves the "returns to Home" half is
    // not just a state-machine transition with nothing behind it.
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(readback.iter().all(|&p| p == home_pixel), "{readback:x?}");
}

/// If Home itself is the one that dies, there is nowhere left to fall back
/// to — the compositor should not crash or panic, just have no foreground.
#[test]
fn eof_on_home_itself_leaves_no_foreground_to_fall_back_to() {
    let (mut compositor, path, _tmp) = compositor();
    let (home_stream, _home_pool) = connect(&path, ClientRole::Home, "Home", None);
    pump(&mut compositor, 3);
    let home_id = compositor.foreground_client().expect("home is foreground");

    drop(home_stream);
    let events = run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ClientDisconnected { .. }))
    });

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ForegroundStopped { label } if label == "Home")),
        "{events:?}"
    );
    assert!(!compositor.is_connected(home_id));
    assert_eq!(compositor.home_client(), None);
    assert_eq!(compositor.foreground_client(), None);
}

/// A background client dying — not the one currently foreground — must not
/// disturb what is on the panel.
#[test]
fn eof_on_a_background_client_does_not_touch_the_foreground() {
    let (mut compositor, path, _tmp) = compositor();
    let (home_stream, _home_pool) = connect(&path, ClientRole::Home, "Home", None);
    pump(&mut compositor, 3);

    let app_pixel = 0xFF99_1122;
    let (_app_stream, _app_pool) = connect(&path, ClientRole::App, "Chess", Some(app_pixel));
    pump(&mut compositor, 5);
    let app_id = compositor.foreground_client().unwrap();

    drop(home_stream); // Home is connected but not foreground right now
    run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ClientDisconnected { .. }))
    });

    assert_eq!(compositor.foreground_client(), Some(app_id));
    assert_eq!(compositor.home_client(), None);
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(readback.iter().all(|&p| p == app_pixel), "{readback:x?}");
}

/// The second acceptance criterion: a client that stops responding does not
/// stall the compositor. A registered client that completes its handshake
/// and then simply never sends anything else is exactly the "wedged" case —
/// `run_once` must keep servicing a concurrently active client at normal
/// speed regardless.
#[test]
fn a_silent_client_does_not_stall_a_concurrently_active_one() {
    let (mut compositor, path, _tmp) = compositor();
    let (_silent_stream, _silent_pool) = connect(&path, ClientRole::Home, "Silent", None);
    pump(&mut compositor, 3);

    let pixel = 0xFF55_AA33;
    let (_active_stream, _active_pool) = connect(&path, ClientRole::App, "Active", Some(pixel));

    let started = Instant::now();
    let events = run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. }))
    });
    let elapsed = started.elapsed();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. })),
        "{events:?}"
    );
    // Generous bound: the point is not a tight latency budget, it is that
    // the silent client's fd never blocked a `read` the active client's
    // progress was waiting behind. A stalled loop would still be sitting in
    // a blocking read on the silent client's fd when this deadline passes,
    // since nothing anywhere ever writes to it.
    assert!(
        elapsed < Duration::from_millis(800),
        "the active client's commit took {elapsed:?} to be observed"
    );
}

/// A connection that never completes its `ClientHello` is reaped once it has
/// outlived `HELLO_DEADLINE`, and doing so does not disturb another client
/// that is actively making progress at the same time.
#[test]
fn an_unregistered_connection_is_reaped_after_the_hello_deadline_without_disturbing_another_client()
{
    let (mut compositor, path, _tmp) = compositor();
    let _silent_raw = UnixStream::connect(&path).unwrap(); // never sends a byte

    let pixel = 0xFF00_1122;
    let (_active_stream, _active_pool) = connect(&path, ClientRole::App, "Active", Some(pixel));

    let events = run_until(
        &mut compositor,
        paper_compositor::HELLO_DEADLINE + Duration::from_secs(1),
        |events| {
            events
                .iter()
                .any(|e| matches!(e, CompositorEvent::ClientDisconnected { id: None, .. }))
        },
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ClientDisconnected { id: None, .. })),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. })),
        "the active client's commit should still have been serviced: {events:?}"
    );
}

fn kill_on_drop(child: Child) -> impl Drop {
    struct KillOnDrop(Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    KillOnDrop(child)
}

fn test_client_binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // the test binary's own file name
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("examples");
    path.push("test_client");
    path
}

/// WWW-78's acceptance criterion, literally: `SIGKILL`-ing an app process
/// leaves the panel showing a legible frame and returns to Home.
#[test]
fn sigkilling_the_foreground_app_shows_a_stopped_frame_and_returns_to_home() {
    let (mut compositor, path, _tmp) = compositor();
    let home_pixel = 0xFF00_AAFF;
    let (_home_stream, _home_pool) = connect(&path, ClientRole::Home, "Home", Some(home_pixel));
    pump(&mut compositor, 5);
    let home_id = compositor.foreground_client().expect("home is foreground");

    let child = Command::new(test_client_binary())
        .arg(&path)
        .arg("app")
        .arg("Chess")
        .arg("--commit")
        .spawn()
        .expect("spawn test_client — run `cargo build -p paper-compositor --examples` first");
    let pid = child.id();
    let _guard = kill_on_drop(child);

    run_until(&mut compositor, Duration::from_secs(2), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. }))
    });
    let app_id = compositor
        .foreground_client()
        .expect("the spawned app is foreground");
    assert_ne!(app_id, home_id);

    // SAFETY: `pid` is this test's own freshly spawned child (`kill_on_drop`
    // also signals it, but a real `SIGKILL` proves the acceptance criterion
    // literally rather than through `Child::kill`'s cross-platform wrapper).
    let killed = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    assert_eq!(killed, 0, "{}", std::io::Error::last_os_error());

    let events = run_until(&mut compositor, Duration::from_secs(2), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ForegroundHome { .. }))
    });

    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ForegroundStopped { label } if label == "Chess")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ForegroundHome { id } if *id == home_id)),
        "{events:?}"
    );
    assert_eq!(compositor.foreground_client(), Some(home_id));
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(readback.iter().all(|&p| p == home_pixel), "{readback:x?}");
}
