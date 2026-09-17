//! Layer 2: `paperctl check` refuses a package that could not run.
//!
//! Everything here is decidable on a Mac with the files in front of you, which
//! is the point of the layer: a package that fails these fails on a developer's
//! machine rather than on the tablet.
//!
//! The layer assumes it will be skipped — a package can be copied onto the
//! device by hand — so nothing downstream relies on it having run. What it
//! buys is that the failures it *does* catch are caught where the error
//! message can name a line of `paper.toml`.

use std::path::Path;

use paper_packages::{BinaryError, CheckError, MACHINE_AARCH64, MAX_ASSET_BYTES, PackageCheck};

/// The first 20 bytes of an aarch64 ELF header — all `paperctl check` reads.
fn aarch64_elf() -> Vec<u8> {
    elf_header(2, 1, 3, MACHINE_AARCH64)
}

fn elf_header(class: u8, data: u8, kind: u16, machine: u16) -> Vec<u8> {
    let mut bytes = vec![0x7f, b'E', b'L', b'F', class, data];
    bytes.resize(16, 0);
    bytes.extend_from_slice(&kind.to_le_bytes());
    bytes.extend_from_slice(&machine.to_le_bytes());
    bytes
}

const GOOD_MANIFEST: &str = r#"
[app]
id = "dev.calum.chess"
name = "Chess"
version = "0.1.0"
protocol = "1.0"
entrypoint = "bin/chess"
assets = ["assets/board.toml"]
"#;

/// Lays down a package that passes, so each test can break exactly one thing.
fn good_package(root: &Path) {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("assets")).unwrap();
    std::fs::write(root.join("paper.toml"), GOOD_MANIFEST).unwrap();
    std::fs::write(root.join("bin/chess"), aarch64_elf()).unwrap();
    std::fs::write(root.join("assets/board.toml"), b"squares = 64").unwrap();
}

#[test]
fn a_well_formed_package_passes_every_package_time_check() {
    let dir = tempfile::tempdir().unwrap();
    good_package(dir.path());

    let check = PackageCheck::run(dir.path()).expect("a good package passes");
    assert_eq!(check.manifest().id().as_str(), "dev.calum.chess");
    assert_eq!(check.target().machine, MACHINE_AARCH64);
    assert!(check.target().runs_on_device());
    assert!(check.total_bytes() > 0);
}

/// The hostile case by name. Every other check passes for a shell script: the
/// path is lexically safe, the file exists, it is not a link, it is the right
/// size. Only the ELF header says it could not run.
#[test]
fn an_invalid_binary_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    good_package(dir.path());
    std::fs::write(dir.path().join("bin/chess"), b"#!/bin/sh\nexec /bin/sh\n").unwrap();

    let error = PackageCheck::run(dir.path()).unwrap_err();
    assert!(
        matches!(error, CheckError::Binary(BinaryError::NotElf { .. })),
        "{error:?}"
    );
}

/// The realistic version of the same mistake: packaging the binary the Mac
/// just built instead of the cross-compiled one.
#[test]
fn a_binary_for_the_wrong_architecture_is_refused() {
    const EM_X86_64: u16 = 0x3E;
    let dir = tempfile::tempdir().unwrap();
    good_package(dir.path());
    std::fs::write(dir.path().join("bin/chess"), elf_header(2, 1, 2, EM_X86_64)).unwrap();

    let error = PackageCheck::run(dir.path()).unwrap_err();
    assert!(
        matches!(
            error,
            CheckError::Binary(BinaryError::WrongMachine { found, .. }) if found == EM_X86_64
        ),
        "{error:?}"
    );
}

/// Unsupported protocol, caught at package time. The App Store still has to be
/// able to *display* this app — that is why `Manifest::parse` does not check
/// the protocol — but `check` is asking whether it could run here, and it
/// could not.
#[test]
fn an_unsupported_protocol_is_refused_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    good_package(dir.path());
    let future = format!(
        "{}\nprotocol_note = 0",
        GOOD_MANIFEST.replace(
            "protocol = \"1.0\"",
            &format!("protocol = \"{}.0\"", paper_protocol::CURRENT.major() + 1)
        )
    );
    // The stray key would be its own error; keep the manifest otherwise exact.
    let future = future.replace("\nprotocol_note = 0", "");
    std::fs::write(dir.path().join("paper.toml"), future).unwrap();

    let error = PackageCheck::run(dir.path()).unwrap_err();
    let CheckError::Manifest { source, .. } = &error else {
        panic!("expected a manifest error, got {error:?}");
    };
    assert!(
        matches!(
            source,
            paper_packages::ManifestError::UnsupportedProtocol { .. }
        ),
        "{source:?}"
    );

    // And the same manifest still parses, because the App Store has to be able
    // to show an app this device cannot run.
    assert!(
        paper_packages::Manifest::read_package(dir.path()).is_ok(),
        "an unrunnable manifest is still a readable one"
    );
}

/// An oversized asset is refused with the declared path named, so the answer
/// is "which file" rather than "something is too big".
#[test]
fn an_oversized_asset_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    good_package(dir.path());

    let big = dir.path().join("assets/board.toml");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(MAX_ASSET_BYTES + 1).unwrap();
    drop(file);

    let error = PackageCheck::run(dir.path()).unwrap_err();
    assert!(
        matches!(error, CheckError::TooLarge { ref path, .. } if path == "assets/board.toml"),
        "{error:?}"
    );
}

/// A package that declares a file it does not ship is not a package.
#[test]
fn a_missing_asset_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    good_package(dir.path());
    std::fs::remove_file(dir.path().join("assets/board.toml")).unwrap();

    let error = PackageCheck::run(dir.path()).unwrap_err();
    assert!(matches!(error, CheckError::Payload { .. }), "{error:?}");
}

/// A path that is a safe *string* and an escape on *disk*. `bin/chess` passes
/// every lexical check there is; the payload walk is what notices it is a link
/// to something outside the package.
#[cfg(unix)]
#[test]
fn a_symlinked_entrypoint_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    good_package(dir.path());
    let entrypoint = dir.path().join("bin/chess");
    std::fs::remove_file(&entrypoint).unwrap();
    std::os::unix::fs::symlink("/bin/sh", &entrypoint).unwrap();

    let error = PackageCheck::run(dir.path()).unwrap_err();
    assert!(matches!(error, CheckError::Payload { .. }), "{error:?}");
}

/// This layer says nothing about authenticity, and there is no accessor on
/// `PackageCheck` that could be mistaken for one. Who built a package is
/// decided over the archive's bytes, by `paper_packages::signing` (WWW-7).
#[test]
fn the_package_time_layer_makes_no_claim_about_who_built_this() {
    let dir = tempfile::tempdir().unwrap();
    good_package(dir.path());
    let check = PackageCheck::run(dir.path()).expect("a good package passes");

    // An edited asset still passes: integrity of a *directory* is not this
    // layer's question, and pretending otherwise would be a tick nobody
    // earned. The archive digest is what a signature covers.
    std::fs::write(dir.path().join("assets/board.toml"), b"squares = 65").unwrap();
    let again = PackageCheck::run(dir.path()).expect("still a runnable package");
    assert_eq!(again.manifest().version(), check.manifest().version());
}

/// A manifest that awards itself capabilities is refused by name, and the
/// message says whose decision it is.
#[test]
fn a_self_granted_capability_is_refused_at_package_time() {
    let dir = tempfile::tempdir().unwrap();
    good_package(dir.path());
    std::fs::write(
        dir.path().join("paper.toml"),
        format!("{GOOD_MANIFEST}\n[capabilities]\nnetwork = true\n"),
    )
    .unwrap();

    let error = PackageCheck::run(dir.path()).unwrap_err();
    let CheckError::Manifest { source, .. } = &error else {
        panic!("expected a manifest error, got {error:?}");
    };
    assert!(
        matches!(
            source,
            paper_packages::ManifestError::SelfGrantedCapabilities { .. }
        ),
        "{source:?}"
    );
}
