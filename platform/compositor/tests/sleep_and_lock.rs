//! WWW-52/WWW-82's acceptance criterion, proven end to end against a real
//! `UnixListener` and real client peers: "a basic lock screen appears on
//! sleep and clears on wake."

use std::io::{Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use paper_compositor::wire::{ClientHello, ClientRequest, ClientRole};
use paper_compositor::{Compositor, CompositorEvent, fdpass};
use paper_device::MemoryPanel;
use paper_protocol::{BufferSlot, Damage, PixelFormat, ShmPoolDescriptor, Size, SurfaceDescriptor};
use paper_sdk::palette;

const PANEL_SIZE: Size = Size::new(4, 4);
const TICK: Duration = Duration::from_millis(50);

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
        write_and_commit(&stream, &mut pool_file, BufferSlot::A, pixel);
    }

    (stream, pool_file)
}

/// Fills `slot` with `pixel` and attaches/commits it over `stream`.
fn write_and_commit(
    stream: &UnixStream,
    pool_file: &mut std::fs::File,
    slot: BufferSlot,
    pixel: u32,
) {
    let offset = descriptor().slot_offset(slot).unwrap();
    let bytes: Vec<u8> = std::iter::repeat_with(|| pixel.to_ne_bytes())
        .take((PANEL_SIZE.width * PANEL_SIZE.height) as usize)
        .flatten()
        .collect();
    pool_file.seek(std::io::SeekFrom::Start(offset)).unwrap();
    pool_file.write_all(&bytes).unwrap();

    let mut writer = stream;
    paper_protocol::codec::write_message(&mut writer, &ClientRequest::Attach(slot)).unwrap();
    paper_protocol::codec::write_message(&mut writer, &ClientRequest::Commit(Damage::Full))
        .unwrap();
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

fn pump(compositor: &mut Compositor, ticks: usize) -> Vec<CompositorEvent> {
    let mut all = Vec::new();
    for _ in 0..ticks {
        all.extend(compositor.run_once(TICK));
    }
    all
}

fn locked_pixel() -> u32 {
    0xFF00_0000 | palette::INK.to_argb()
}

#[test]
fn sleeping_draws_the_lock_screen_over_a_running_apps_frame() {
    let (mut compositor, path, _tmp) = compositor();
    let app_pixel = 0xFF11_2233;
    let (_app_stream, _app_pool) = connect(&path, ClientRole::App, "Chess", Some(app_pixel));
    run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. }))
    });
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(readback.iter().all(|&p| p == app_pixel), "{readback:x?}");

    let mut events = Vec::new();
    compositor.sleep(&mut events);

    assert!(compositor.is_locked());
    assert_eq!(events, vec![CompositorEvent::Locked]);
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(
        readback.iter().all(|&p| p == locked_pixel()),
        "{readback:x?}"
    );
}

#[test]
fn a_frame_committed_while_locked_never_reaches_the_panel_until_wake() {
    let (mut compositor, path, _tmp) = compositor();
    let (app_stream, mut app_pool) = connect(&path, ClientRole::App, "Chess", Some(0xFF11_2233));
    run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. }))
    });

    let mut events = Vec::new();
    compositor.sleep(&mut events);
    assert!(compositor.is_locked());

    // The client keeps drawing while locked — a live app does not know or
    // care that the panel is covered — and attaching/committing a second
    // buffer must still succeed rather than stall on the first slot never
    // being released.
    let second_pixel = 0xFF66_9900;
    write_and_commit(&app_stream, &mut app_pool, BufferSlot::B, second_pixel);
    let events = pump(&mut compositor, 5);

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. })),
        "a commit while locked must not present: {events:?}"
    );
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(
        readback.iter().all(|&p| p == locked_pixel()),
        "the lock screen must still be showing: {readback:x?}"
    );

    let mut events = Vec::new();
    compositor.wake(&mut events);

    assert!(!compositor.is_locked());
    assert!(events.contains(&CompositorEvent::Unlocked));
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(
        readback.iter().all(|&p| p == second_pixel),
        "waking should reveal the latest committed frame, not a stale one: {readback:x?}"
    );
}

#[test]
fn waking_when_not_locked_is_a_no_op() {
    let (mut compositor, path, _tmp) = compositor();
    let app_pixel = 0xFF44_5566;
    let (_app_stream, _app_pool) = connect(&path, ClientRole::App, "Chess", Some(app_pixel));
    run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::FramePresented { .. }))
    });

    let mut events = Vec::new();
    compositor.wake(&mut events);

    assert!(events.is_empty());
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(readback.iter().all(|&p| p == app_pixel), "{readback:x?}");
}

/// The foreground app dying while the panel is locked must not reveal a
/// "stopped" frame through the lock screen — the state transition is
/// recorded, but the panel does not change until `wake`.
#[test]
fn a_foreground_crash_while_locked_does_not_show_through_the_lock_screen_until_wake() {
    let (mut compositor, path, _tmp) = compositor();
    let home_pixel = 0xFF00_FF00;
    let (_home_stream, _home_pool) = connect(&path, ClientRole::Home, "Home", Some(home_pixel));
    pump(&mut compositor, 5);
    let home_id = compositor.foreground_client().expect("home is foreground");

    let (app_stream, _app_pool) = connect(&path, ClientRole::App, "Chess", Some(0xFF22_3344));
    pump(&mut compositor, 5);
    let app_id = compositor.foreground_client().expect("app is foreground");
    assert_ne!(app_id, home_id);

    let mut events = Vec::new();
    compositor.sleep(&mut events);
    assert!(compositor.is_locked());

    drop(app_stream); // the kernel-visible effect of the app process dying
    let events = run_until(&mut compositor, Duration::from_secs(1), |events| {
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ForegroundStopped { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CompositorEvent::ForegroundStopped { label } if label == "Chess")),
        "{events:?}"
    );
    // The state moved on, but the lock screen must still be what is on the
    // glass — nothing about the crash may repaint the panel while locked.
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(
        readback.iter().all(|&p| p == locked_pixel()),
        "{readback:x?}"
    );

    let mut events = Vec::new();
    compositor.wake(&mut events);
    assert!(!compositor.is_locked());
    assert_eq!(compositor.foreground_client(), Some(home_id));
    let readback = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert!(readback.iter().all(|&p| p == home_pixel), "{readback:x?}");
}

#[test]
fn sleeping_twice_in_a_row_does_not_redraw() {
    let (mut compositor, path, _tmp) = compositor();
    let (_app_stream, _app_pool) = connect(&path, ClientRole::App, "Chess", Some(0xFF11_2233));
    pump(&mut compositor, 5);

    let mut events = Vec::new();
    compositor.sleep(&mut events);
    assert_eq!(events, vec![CompositorEvent::Locked]);
    let swaps_after_first_sleep = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();

    let mut events = Vec::new();
    compositor.sleep(&mut events);
    assert!(
        events.is_empty(),
        "a second sleep() should be a no-op: {events:?}"
    );
    let swaps_after_second_sleep = compositor
        .panel()
        .readback(paper_device::Plane::Front)
        .unwrap();
    assert_eq!(swaps_after_first_sleep, swaps_after_second_sleep);
}
