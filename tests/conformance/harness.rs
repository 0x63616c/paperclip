//! Shared fixtures for the conformance suite.
//!
//! The four test targets beside this file are the four layers of §8, one each,
//! named so a failure says which layer let something through:
//!
//! | Target | Layer | What it drives |
//! |---|---|---|
//! | `compile_time` | The Rust SDK | A real app, written against `paper_sdk` alone |
//! | `package_time` | `paperctl check` | Packages on disk, good and hostile |
//! | `launch_time` | The host's `Session` | Message sequences, legal and not |
//! | `runtime` | The host's reader | Raw bytes from something that never linked the SDK |
//!
//! Each layer is tested on the assumption that the ones before it were
//! skipped, because that is the assumption the platform makes.

use std::io::Write;
use std::sync::{Arc, Mutex};

use paper_protocol::{
    AppId, AppPaths, CURRENT, Hello, HostMessage, LaunchReason, PixelFormat, ProtocolVersion,
    SessionId, Size, SurfaceDescriptor, codec,
};

/// The viewport every fixture app draws into.
///
/// Small on purpose: these tests are about the contract, and a 1620×2160
/// allocation per case would make the suite slower for no extra coverage.
pub const VIEWPORT: Size = Size::new(64, 96);

/// A `Write` several places can hold, so a test can read back what an app sent.
#[derive(Debug, Clone, Default)]
pub struct Recorder {
    written: Arc<Mutex<Vec<u8>>>,
}

impl Recorder {
    /// A new, empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything written so far.
    pub fn bytes(&self) -> Vec<u8> {
        self.lock().clone()
    }

    /// Everything written so far, decoded as app messages.
    ///
    /// Panics if a frame does not decode, which in a conformance test is the
    /// failure you want to see rather than one to skip past.
    pub fn messages(&self) -> Vec<paper_protocol::AppMessage> {
        let bytes = self.bytes();
        let mut reader = bytes.as_slice();
        let mut out = Vec::new();
        loop {
            match codec::read_message::<_, paper_protocol::AppMessage>(&mut reader) {
                Ok(message) => out.push(message),
                Err(paper_protocol::CodecError::Closed) => return out,
                Err(error) => panic!("an app wrote something undecodable: {error:?}"),
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<u8>> {
        self.written.lock().expect("recorder poisoned")
    }
}

impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The app id every fixture uses.
pub fn app_id() -> AppId {
    "dev.calum.fixture".parse().expect("a valid fixture id")
}

/// A `Hello` for a fixture app with the given capabilities and storage root.
pub fn hello(root: &std::path::Path, capabilities: Vec<paper_protocol::Capability>) -> Hello {
    hello_speaking(CURRENT, root, capabilities)
}

/// A `Hello` from a host claiming `protocol`.
pub fn hello_speaking(
    protocol: ProtocolVersion,
    root: &std::path::Path,
    capabilities: Vec<paper_protocol::Capability>,
) -> Hello {
    Hello {
        protocol,
        session: SessionId::new(0x5eed),
        app: app_id(),
        version: semver::Version::parse("0.1.0").expect("a valid fixture version"),
        launch: LaunchReason::Fresh,
        surface: SurfaceDescriptor::packed(VIEWPORT, PixelFormat::Argb8888),
        capabilities,
        paths: AppPaths {
            assets: root.join("assets"),
            private: root.join("private"),
            temp: root.join("temp"),
            shared: Vec::new(),
        },
    }
}

/// Encodes a script of host messages into the bytes an app will read.
pub fn script(messages: &[HostMessage]) -> Vec<u8> {
    let mut wire = Vec::new();
    for message in messages {
        codec::write_message(&mut wire, message).expect("a fixture script encodes");
    }
    wire
}

/// Creates the three storage directories a `Hello` points at.
pub fn storage_root(root: &std::path::Path) {
    for name in ["assets", "private", "temp"] {
        std::fs::create_dir_all(root.join(name)).expect("fixture storage is creatable");
    }
}
