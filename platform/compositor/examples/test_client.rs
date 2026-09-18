//! A real, killable compositor client, for `tests/crash_and_hang_isolation.rs`.
//!
//! WWW-78's acceptance criteria name `SIGKILL` specifically — this exists so
//! that acceptance criterion is proven against a real process the test can
//! send a real signal to, not just against a dropped in-process `UnixStream`
//! standing in for one.
//!
//! Usage: `test_client <socket-path> <home|app> <label> [--commit]`. Connects,
//! sends a `ClientHello` with a small pool (optionally attaches and commits
//! one frame if `--commit` is given), then blocks forever so the test
//! controls this process's lifetime and death.

use std::env;
use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use paper_compositor::fdpass;
use paper_compositor::wire::{ClientHello, ClientRequest, ClientRole};
use paper_protocol::{BufferSlot, Damage, PixelFormat, ShmPoolDescriptor, Size, SurfaceDescriptor};

fn framed<T: serde::Serialize>(message: &T) -> Vec<u8> {
    let body = paper_protocol::codec::encode(message).expect("message encodes");
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    frame
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let [_, socket_path, role, label] = &args[..4.min(args.len())] else {
        eprintln!("usage: test_client <socket-path> <home|app> <label> [--commit]");
        std::process::exit(2);
    };
    let commit = args.iter().any(|a| a == "--commit");

    let role = match role.as_str() {
        "home" => ClientRole::Home,
        "app" => ClientRole::App,
        other => {
            eprintln!("unknown role {other:?}");
            std::process::exit(2);
        }
    };

    let size = Size::new(4, 4);
    let descriptor = ShmPoolDescriptor {
        buffer: SurfaceDescriptor::packed(size, PixelFormat::Argb8888),
    };
    let pool_file = tempfile::tempfile().expect("tempfile");
    pool_file
        .set_len(descriptor.pool_bytes().expect("valid pool"))
        .expect("sized");

    let stream = UnixStream::connect(socket_path).expect("connect to compositor");
    let hello = ClientHello {
        role,
        label: label.clone(),
        pool: descriptor,
    };
    fdpass::send_with_fd(&stream, &framed(&hello), pool_file.as_raw_fd()).expect("send hello");

    if commit {
        // Fill the buffer with a distinctive, non-zero colour before
        // attaching — a real client draws into the slot it is about to
        // attach, and this makes the presented frame visibly non-blank.
        let mut file: File = pool_file;
        let pixel = 0xFF44_2266u32;
        let bytes: Vec<u8> = std::iter::repeat_with(|| pixel.to_ne_bytes())
            .take((size.width * size.height) as usize)
            .flatten()
            .collect();
        file.write_all(&bytes).expect("fill slot A");

        let mut writer = &stream;
        paper_protocol::codec::write_message(&mut writer, &ClientRequest::Attach(BufferSlot::A))
            .expect("send attach");
        paper_protocol::codec::write_message(&mut writer, &ClientRequest::Commit(Damage::Full))
            .expect("send commit");
    }

    // Held open so the test controls this process's lifetime — killed with
    // `SIGKILL`, or left to be dropped when the test process exits.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
