//! `bin/render-test-card`, the compiled entrypoint, not just the linked
//! `App` (§8's launch-time layer, exercised across a real process boundary).
//!
//! Mirrors `apps/sudoku/tests/entrypoint.rs`: spawn the built binary, send it
//! a real `Hello` on its stdin, and read a real `Ready` off its stdout — the
//! artefact `apps/render-test-card/paper.toml` names as its entrypoint, not
//! just the `RenderTestCardApp` type behind it.
//!
//! Unlike Chess and Sudoku, this entrypoint is a compositor client (WWW-81):
//! its pixels go through `paper_compositor::CompositorSurfaces`, not
//! `LocalSurfaces`, so this test also has to be the thing that binds a real
//! compositor and points the spawned process at it via
//! [`paper_compositor::SOCKET_ENV`] — otherwise the entrypoint's first
//! `SurfaceProvider::open` call fails before it ever answers `Hello`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use paper_compositor::{Compositor, SOCKET_ENV};
use paper_device::MemoryPanel;
use paper_protocol::{
    AppId, AppMessage, AppPaths, CURRENT, Capability, ExitReason, Hello, HostMessage, LaunchReason,
    LifecycleEvent, PixelFormat, Saved, SessionId, Size, SurfaceDescriptor, codec,
};

/// Binds a compositor to a temp socket and drives it on a background thread
/// until `stop` is set. Returns the socket path and a handle to stop it.
fn spawn_compositor() -> (
    std::path::PathBuf,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let path = std::env::temp_dir().join(format!(
        "paper-render-test-card-entrypoint-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = Arc::clone(&stop);
        let path = path.clone();
        std::thread::spawn(move || {
            // `Compositor` holds a `Pool` once a client connects, which
            // wraps a raw `NonNull` mapping and is therefore `!Send` — it
            // has to be built and driven entirely on this thread, not
            // constructed on the caller's and moved in.
            let mut compositor =
                Compositor::bind(&path, Box::new(MemoryPanel::new(Size::new(64, 96))))
                    .expect("binds a compositor socket");
            while !stop.load(Ordering::SeqCst) {
                compositor.run_once(Duration::from_millis(20));
            }
            let _ = std::fs::remove_file(&path);
        })
    };
    // `Compositor::bind` runs on the thread above; wait for its socket to
    // actually exist rather than assuming the thread got there first.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(path.exists(), "the compositor never bound its socket");
    (path, stop, handle)
}

fn hello(storage: &Path) -> Hello {
    let private = storage.join("private");
    std::fs::create_dir_all(&private).expect("a private directory");
    Hello {
        protocol: CURRENT,
        session: SessionId::new(1),
        app: "dev.calum.render-test-card"
            .parse::<AppId>()
            .expect("a valid app id"),
        version: "0.0.0".parse().expect("a valid semver"),
        launch: LaunchReason::Fresh,
        surface: SurfaceDescriptor::packed(Size::new(64, 96), PixelFormat::Argb8888),
        capabilities: vec![Capability::Storage],
        paths: AppPaths {
            assets: storage.join("assets"),
            private,
            temp: storage.join("temp"),
            shared: Vec::new(),
        },
    }
}

#[test]
fn the_built_entrypoint_answers_hello_with_ready_saves_and_exits_cleanly() {
    let storage = tempfile::tempdir().expect("a scratch storage directory");
    let (socket, stop_compositor, compositor_thread) = spawn_compositor();

    let mut child = Command::new(env!("CARGO_BIN_EXE_render-test-card"))
        .env(SOCKET_ENV, &socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the built entrypoint spawns");

    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut stdout = child.stdout.take().expect("stdout is piped");

    codec::write_message(&mut stdin, &HostMessage::Hello(hello(storage.path())))
        .expect("writes hello");
    match codec::read_message::<_, AppMessage>(&mut stdout).expect("reads a reply") {
        AppMessage::Ready(ready) => assert_eq!(ready.protocol, CURRENT),
        other => panic!("expected Ready, got {other:?}"),
    }

    let exit = LifecycleEvent::prepare_to_exit(ExitReason::ReturnToStock, Duration::from_secs(3));
    codec::write_message(&mut stdin, &HostMessage::Lifecycle(exit))
        .expect("writes prepare-to-exit");
    match codec::read_message::<_, AppMessage>(&mut stdout).expect("reads a reply") {
        AppMessage::Saved(Saved { ok, .. }) => {
            assert!(ok, "this app has nothing to save and cannot fail at it");
        }
        other => panic!("expected Saved, got {other:?}"),
    }

    drop(stdin);
    let status = child.wait().expect("the process exits");
    assert!(status.success(), "the entrypoint exited {status:?}");

    stop_compositor.store(true, Ordering::SeqCst);
    compositor_thread
        .join()
        .expect("the compositor thread exits");
}
