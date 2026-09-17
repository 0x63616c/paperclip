//! The catalog path, driven through the real command line, with an app that is
//! not Chess (§12, WWW-39).
//!
//! Every other test of installing builds its manifest inline, and every one of
//! them calls the app `dev.calum.chess`. That leaves a gap exactly the shape of
//! this issue: a bug in packaging or installing *a second app* — an id that is
//! not the one hard-coded in the fixtures, a store path derived from the wrong
//! name, an entrypoint the archive builder assumed — has nowhere to show up.
//!
//! So this runs the sequence a person would type, against the `paper.toml`
//! Sudoku actually ships, and asserts the store it lands in holds Sudoku at
//! Sudoku's own version. It reads the manifest rather than repeating it: a
//! version bump in `apps/sudoku/paper.toml` must not need an edit here, and a
//! test that hard-codes what it is checking stops checking it.
//!
//! Software only. A scratch `--root` on a Mac is a real store with a real
//! install transaction in it; it is not the tablet.

#![cfg(feature = "publishing")]

use std::path::Path;
use std::process::Command;

use paper_packages::Manifest;

/// The manifest Sudoku ships, not a copy of it.
const SUDOKU_MANIFEST: &str = include_str!("../../../apps/sudoku/paper.toml");

/// Runs `paperctl`, and fails the test with everything it said if it refused.
fn paperctl(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_paperctl"))
        .args(args)
        .output()
        .expect("paperctl runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "paperctl {}\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// A minimal aarch64 ELF header: enough for the package-time check that an
/// entrypoint is a program for the device and not a script or a Mac binary.
///
/// No app crate in this tree has a binary target yet — apps are linked into
/// `paperctl` and run in-process — so there is no real `bin/sudoku` to
/// package. That limits what this proves to the catalog path, which is what it
/// is for; it proves nothing about a binary that starts.
fn device_entrypoint() -> Vec<u8> {
    let mut bytes = vec![0x7f, b'E', b'L', b'F', 2, 1];
    bytes.resize(16, 0);
    bytes.extend_from_slice(&3u16.to_le_bytes()); // ET_DYN: a PIE.
    bytes.extend_from_slice(&paper_packages::MACHINE_AARCH64.to_le_bytes());
    bytes
}

/// Lays out the package source a `cargo build` for the device would leave
/// behind: the shipped manifest, and the entrypoint it declares.
fn package_source(at: &Path, manifest: &Manifest) {
    std::fs::write(at.join(paper_packages::MANIFEST_FILE_NAME), SUDOKU_MANIFEST)
        .expect("the manifest");
    let entrypoint = at.join(manifest.entrypoint().as_str());
    std::fs::create_dir_all(entrypoint.parent().expect("the entrypoint has a directory"))
        .expect("the bin directory");
    std::fs::write(&entrypoint, device_entrypoint()).expect("the entrypoint");
}

#[test]
fn a_second_app_goes_through_the_catalog_from_source_to_installed() {
    let manifest = Manifest::parse(SUDOKU_MANIFEST).expect("Sudoku's manifest parses");
    let app = manifest.id().to_string();
    let version = manifest.version().to_string();
    assert_ne!(app, "dev.calum.chess", "this test exists to not be Chess");

    let work = tempfile::tempdir().expect("a scratch directory");
    let source = work.path().join("source");
    std::fs::create_dir_all(&source).expect("the source directory");
    package_source(&source, &manifest);

    let keys = work.path().join("keys");
    let catalog = work.path().join("catalog");
    let root = work.path().join("root");
    let archive = work.path().join("sudoku.paperpkg");
    let secret = keys.join("paperclip.key");
    let public = keys.join("paperclip.pub");
    let (source, catalog, root, archive, secret, public) = (
        source.display().to_string(),
        catalog.display().to_string(),
        root.display().to_string(),
        archive.display().to_string(),
        secret.display().to_string(),
        public.display().to_string(),
    );

    // Could this run? The package-time layer, before anyone has signed it:
    // the id, the protocol the platform speaks, and an entrypoint that is a
    // program for the tablet rather than for the Mac it was built on.
    let checked = paperctl(&["check", &source]);
    for expected in [app.as_str(), "(platform speaks", "aarch64 ELF"] {
        assert!(
            checked.contains(expected),
            "check did not report `{expected}`:\n{checked}"
        );
    }

    paperctl(&["key", "generate", "--out-dir", &keys.display().to_string()]);
    let packaged = paperctl(&["package", &source, "--out", &archive]);
    assert!(
        packaged.contains(&app) && packaged.contains(&version),
        "package did not report {app} {version}:\n{packaged}"
    );

    let published = paperctl(&["publish", &archive, "--catalog", &catalog, "--key", &secret]);
    assert!(
        published.contains(&app) && published.contains(&version),
        "publish did not report {app} {version}:\n{published}"
    );

    // Who vouched for these bytes? The other half of `check`, over the catalog.
    let verified = paperctl(&["check", &catalog, "--trust", &public]);
    assert!(
        verified.contains("verified") && verified.contains(&app),
        "the catalog does not verify {app}:\n{verified}"
    );

    let installed = paperctl(&[
        "install",
        &app,
        "--catalog",
        &catalog,
        "--trust",
        &public,
        "--root",
        &root,
    ]);
    assert!(
        installed.contains(&format!("installed  {app} {version}")),
        "install did not land {app} {version}:\n{installed}"
    );
    assert!(
        installed.contains("selected   yes"),
        "the release was committed but never selected:\n{installed}"
    );
    // `paperctl install` installs under a deny-all policy and has no flag to
    // widen it (documented in `docs/packaging.md`). A grant appearing here
    // would mean a manifest had talked the host into one.
    assert!(
        installed.contains("granted    nothing"),
        "a catalog install granted a capability:\n{installed}"
    );

    let listed = paperctl(&["list", "--root", &root]);
    assert!(
        listed.contains(&app) && listed.contains(manifest.name().as_str()),
        "the store does not show {app}:\n{listed}"
    );
    assert!(
        !listed.contains("dev.calum.chess"),
        "installing Sudoku put Chess in the store:\n{listed}"
    );

    // Installing the same release again is the ordinary case — an App Store
    // tap on an app already installed — and must change nothing.
    let again = paperctl(&[
        "install",
        &app,
        "--catalog",
        &catalog,
        "--trust",
        &public,
        "--root",
        &root,
    ]);
    assert!(
        again.contains("already installed, unchanged"),
        "a second install of the same version was not idempotent:\n{again}"
    );
}
