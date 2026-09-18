//! `bin/counter`, the compiled entrypoint, not just the linked `App` (§8's
//! launch-time layer, exercised across a real process boundary).
//!
//! Mirrors `apps/sudoku/tests/entrypoint.rs`: spawn the built binary, send
//! it a real `Hello` on its stdin, and read a real `Ready` off its stdout —
//! the artefact `paper.toml` names as its entrypoint, not just the
//! `CounterApp` type behind it.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use paper_protocol::{
    AppId, AppMessage, AppPaths, CURRENT, Capability, ExitReason, Hello, HostMessage, LaunchReason,
    LifecycleEvent, PixelFormat, Saved, SessionId, Size, SurfaceDescriptor, codec,
};

fn hello(storage: &Path) -> Hello {
    let private = storage.join("private");
    std::fs::create_dir_all(&private).expect("a private directory");
    Hello {
        protocol: CURRENT,
        session: SessionId::new(1),
        app: "dev.calum.counter"
            .parse::<AppId>()
            .expect("a valid app id"),
        version: "0.1.0".parse().expect("a valid semver"),
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

    let mut child = Command::new(env!("CARGO_BIN_EXE_counter"))
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
            assert!(ok, "a fresh count with a writable private dir should save");
        }
        other => panic!("expected Saved, got {other:?}"),
    }

    drop(stdin);
    let status = child.wait().expect("the process exits");
    assert!(status.success(), "the entrypoint exited {status:?}");
}
