//! Manifest parsing and validation, exercised through the public API only.

use std::fs;

use paper_packages::{
    Capability, InstallPolicy, InstalledApp, ManifestError, PathError, PayloadError,
};
use paper_packages::{MANIFEST_FILE_NAME, MAX_ASSETS, MAX_MANIFEST_BYTES, Manifest};
use std::os::unix::fs::symlink;

const VALID: &str = r#"
[app]
id = "dev.calum.chess"
name = "Chess"
version = "0.2.1-rc.1+build.7"
protocol = "1.0"
entrypoint = "bin/chess"
assets = ["assets/board.toml", "icon.png"]
"#;

#[test]
fn parses_a_valid_manifest() {
    let manifest = Manifest::parse(VALID).expect("valid manifest");

    assert_eq!(manifest.id().as_str(), "dev.calum.chess");
    assert_eq!(manifest.name().as_str(), "Chess");
    assert_eq!(manifest.version().major, 0);
    assert_eq!(manifest.version().minor, 2);
    assert_eq!(manifest.version().patch, 1);
    assert_eq!(manifest.version().pre.as_str(), "rc.1");
    assert_eq!(manifest.protocol().to_string(), "1.0");
    assert_eq!(manifest.entrypoint().as_str(), "bin/chess");
    assert_eq!(
        manifest
            .assets()
            .iter()
            .map(|a| a.as_str())
            .collect::<Vec<_>>(),
        vec!["assets/board.toml", "icon.png"]
    );
    manifest
        .ensure_runnable()
        .expect("1.0 runs on this platform");
}

#[test]
fn assets_default_to_empty() {
    let manifest = Manifest::parse(
        r#"
        [app]
        id = "dev.calum.home"
        name = "Home"
        version = "1.0.0"
        protocol = "1.0"
        entrypoint = "bin/home"
        "#,
    )
    .expect("assets are optional");
    assert!(manifest.assets().is_empty());
}

#[test]
fn rejects_malformed_semver() {
    for bad in ["1.0", "v1.0.0", "latest", "1.0.0.0", ""] {
        let text = VALID.replace("0.2.1-rc.1+build.7", bad);
        match Manifest::parse(&text) {
            Err(ManifestError::Version { value, .. }) => assert_eq!(value, bad),
            other => panic!("`{bad}` should be rejected as SemVer, got {other:?}"),
        }
    }
}

#[test]
fn rejects_absolute_entrypoints() {
    let text = VALID.replace("bin/chess", "/bin/sh");
    match Manifest::parse(&text) {
        Err(ManifestError::Entrypoint { value, source }) => {
            assert_eq!(value, "/bin/sh");
            assert_eq!(source, PathError::Absolute);
        }
        other => panic!("absolute entrypoint should be rejected, got {other:?}"),
    }
}

#[test]
fn rejects_escaping_entrypoints() {
    for (bad, expected) in [
        ("../../usr/bin/env", PathError::ParentEscape),
        ("~/bin/chess", PathError::HomeExpansion),
        ("./bin/chess", PathError::EmptyComponent),
    ] {
        let text = VALID.replace("bin/chess", bad);
        match Manifest::parse(&text) {
            Err(ManifestError::Entrypoint { value, source }) => {
                assert_eq!(value, bad);
                assert_eq!(source, expected);
            }
            other => panic!("`{bad}` should be rejected as an entrypoint, got {other:?}"),
        }
    }
}

#[test]
fn rejects_windows_style_entrypoints() {
    // A TOML literal string, so the backslash reaches the validator intact
    // rather than being rejected earlier as a bad TOML escape.
    let text = VALID.replace(
        "entrypoint = \"bin/chess\"",
        "entrypoint = 'bin\\chess.exe'",
    );
    match Manifest::parse(&text) {
        Err(ManifestError::Entrypoint { value, source }) => {
            assert_eq!(value, "bin\\chess.exe");
            assert_eq!(source, PathError::NotPosix);
        }
        other => panic!("a `\\`-separated entrypoint should be rejected, got {other:?}"),
    }
}

#[test]
fn rejects_unsafe_assets_and_reports_the_index() {
    let text = VALID.replace("\"icon.png\"", "\"../../../etc/passwd\"");
    match Manifest::parse(&text) {
        Err(ManifestError::Asset {
            index,
            value,
            source,
        }) => {
            assert_eq!(index, 1);
            assert_eq!(value, "../../../etc/passwd");
            assert_eq!(source, PathError::ParentEscape);
        }
        other => panic!("escaping asset should be rejected, got {other:?}"),
    }
}

#[test]
fn rejects_duplicate_assets() {
    let text = VALID.replace("\"icon.png\"", "\"assets/board.toml\"");
    assert!(matches!(
        Manifest::parse(&text),
        Err(ManifestError::DuplicateAsset { .. })
    ));
}

#[test]
fn rejects_bad_ids_and_names() {
    assert!(matches!(
        Manifest::parse(&VALID.replace("dev.calum.chess", "Chess")),
        Err(ManifestError::AppId { .. })
    ));
    assert!(matches!(
        Manifest::parse(&VALID.replace("name = \"Chess\"", "name = \"   \"")),
        Err(ManifestError::DisplayName { .. })
    ));
}

#[test]
fn rejects_a_manifest_that_grants_itself_capabilities() {
    let cases = [
        (
            r#"
            [app]
            id = "dev.calum.chess"
            name = "Chess"
            version = "1.0.0"
            protocol = "1.0"
            entrypoint = "bin/chess"
            capabilities = ["network", "storage"]
            "#,
            "app.capabilities",
        ),
        (
            r#"
            [app]
            id = "dev.calum.chess"
            name = "Chess"
            version = "1.0.0"
            protocol = "1.0"
            entrypoint = "bin/chess"
            permissions = ["network"]
            "#,
            "app.permissions",
        ),
        (
            r#"
            [app]
            id = "dev.calum.chess"
            name = "Chess"
            version = "1.0.0"
            protocol = "1.0"
            entrypoint = "bin/chess"

            [capabilities]
            network = true
            "#,
            "capabilities",
        ),
    ];

    for (text, expected_key) in cases {
        match Manifest::parse(text) {
            Err(ManifestError::SelfGrantedCapabilities { key }) => {
                assert_eq!(key, expected_key);
            }
            other => panic!("self-granted capabilities should be rejected, got {other:?}"),
        }
    }
}

#[test]
fn capabilities_come_only_from_install_policy() {
    let manifest = Manifest::parse(VALID).expect("valid manifest");

    let denied = InstalledApp::install(manifest.clone(), &InstallPolicy::deny_all());
    assert!(denied.capabilities().is_empty());

    let mut policy = InstallPolicy::deny_all();
    policy.allow(manifest.id(), Capability::Storage);
    let installed = InstalledApp::install(manifest, &policy);

    assert!(installed.capabilities().holds(Capability::Storage));
    assert!(!installed.capabilities().holds(Capability::Network));
}

#[test]
fn rejects_unknown_manifest_keys() {
    let text = format!("{VALID}\nsandbox = \"none\"\n");
    assert!(matches!(
        Manifest::parse(&text),
        Err(ManifestError::Schema(_))
    ));

    let text = VALID.replace("entrypoint =", "sandbox = \"none\"\nentrypoint =");
    assert!(matches!(
        Manifest::parse(&text),
        Err(ManifestError::Schema(_))
    ));
}

#[test]
fn rejects_broken_toml() {
    assert!(matches!(
        Manifest::parse("[app\nid = "),
        Err(ManifestError::Syntax(_))
    ));
}

#[test]
fn rejects_an_unreadable_protocol_version() {
    // A typed protocol error, not a faked schema error: the caller can tell
    // "that is not a protocol version" from "your TOML is the wrong shape".
    match Manifest::parse(&VALID.replace("protocol = \"1.0\"", "protocol = \"1.0.0\"")) {
        Err(ManifestError::Protocol { value, source }) => {
            assert_eq!(value, "1.0.0");
            assert!(matches!(source, paper_protocol::ParseError::Shape(_)));
        }
        other => panic!("expected a protocol error, got {other:?}"),
    }
}

#[test]
fn rejects_a_protocol_this_platform_cannot_run() {
    let manifest = Manifest::parse(&VALID.replace("protocol = \"1.0\"", "protocol = \"9.0\""))
        .expect("a future protocol still parses");
    match manifest.ensure_runnable() {
        Err(ManifestError::UnsupportedProtocol { declared, current }) => {
            assert_eq!(declared.to_string(), "9.0");
            assert_eq!(current, paper_protocol::CURRENT);
        }
        other => panic!("future protocol should not be runnable, got {other:?}"),
    }
}

#[test]
fn payload_validation_accepts_a_complete_package() {
    let package = tempfile::tempdir().expect("tempdir");
    let root = package.path();
    fs::write(root.join(MANIFEST_FILE_NAME), VALID).expect("write manifest");
    fs::create_dir_all(root.join("bin")).expect("bin");
    fs::create_dir_all(root.join("assets")).expect("assets");
    fs::write(root.join("bin/chess"), b"#!/bin/sh\n").expect("entrypoint");
    fs::write(root.join("assets/board.toml"), b"").expect("asset");
    fs::write(root.join("icon.png"), b"").expect("asset");

    let manifest = Manifest::read_package(root).expect("manifest reads from the package root");
    manifest
        .validate_payload(root)
        .expect("payload is complete");
}

#[test]
fn payload_validation_reports_a_missing_declared_asset() {
    let package = tempfile::tempdir().expect("tempdir");
    let root = package.path();
    fs::write(root.join(MANIFEST_FILE_NAME), VALID).expect("write manifest");
    fs::create_dir_all(root.join("bin")).expect("bin");
    fs::create_dir_all(root.join("assets")).expect("assets");
    fs::write(root.join("bin/chess"), b"#!/bin/sh\n").expect("entrypoint");
    fs::write(root.join("assets/board.toml"), b"").expect("asset");

    let manifest = Manifest::read_package(root).expect("manifest");
    match manifest.validate_payload(root) {
        Err(PayloadError::MissingAsset { path }) => assert_eq!(path, "icon.png"),
        other => panic!("missing asset should be reported, got {other:?}"),
    }
}

#[test]
fn payload_validation_reports_a_missing_entrypoint() {
    let package = tempfile::tempdir().expect("tempdir");
    let root = package.path();
    fs::write(root.join(MANIFEST_FILE_NAME), VALID).expect("write manifest");

    let manifest = Manifest::read_package(root).expect("manifest");
    match manifest.validate_payload(root) {
        Err(PayloadError::MissingEntrypoint { path }) => assert_eq!(path, "bin/chess"),
        other => panic!("missing entrypoint should be reported, got {other:?}"),
    }
}

#[test]
fn payload_validation_rejects_a_directory_entrypoint() {
    let package = tempfile::tempdir().expect("tempdir");
    let root = package.path();
    fs::write(root.join(MANIFEST_FILE_NAME), VALID).expect("write manifest");
    fs::create_dir_all(root.join("bin/chess")).expect("directory where a binary belongs");
    fs::create_dir_all(root.join("assets")).expect("assets");
    fs::write(root.join("assets/board.toml"), b"").expect("asset");
    fs::write(root.join("icon.png"), b"").expect("asset");

    let manifest = Manifest::read_package(root).expect("manifest");
    assert!(matches!(
        manifest.validate_payload(root),
        Err(PayloadError::EntrypointNotAFile { .. })
    ));
}

#[test]
fn reading_a_package_without_a_manifest_says_so() {
    let package = tempfile::tempdir().expect("tempdir");
    assert!(matches!(
        Manifest::read_package(package.path()),
        Err(ManifestError::Read { .. })
    ));
}

#[test]
fn a_symlinked_entrypoint_does_not_pass_as_the_package_s_own_executable() {
    // The attack `RelativePath` cannot see: `bin/chess` is a perfectly safe
    // string, and on disk it points at the host's shell.
    let package = tempfile::tempdir().expect("tempdir");
    let root = package.path();
    fs::write(root.join(MANIFEST_FILE_NAME), VALID).expect("write manifest");
    fs::create_dir_all(root.join("bin")).expect("bin");
    fs::create_dir_all(root.join("assets")).expect("assets");
    fs::write(root.join("assets/board.toml"), b"").expect("asset");
    fs::write(root.join("icon.png"), b"").expect("asset");
    symlink("/bin/sh", root.join("bin/chess")).expect("symlink");

    let manifest = Manifest::read_package(root).expect("manifest");
    match manifest.validate_payload(root) {
        Err(PayloadError::SymlinkedPath { path, component }) => {
            assert_eq!(path, "bin/chess");
            assert_eq!(component, "chess");
        }
        other => panic!("a symlinked entrypoint must be refused, got {other:?}"),
    }
}

#[test]
fn a_symlinked_directory_on_the_way_to_an_asset_is_refused_too() {
    // Checking only the leaf would wave this through: `assets` itself is the
    // link, and `assets/board.toml` resolves outside the package.
    let package = tempfile::tempdir().expect("tempdir");
    let root = package.path();
    let outside = tempfile::tempdir().expect("outside");
    fs::write(outside.path().join("board.toml"), b"").expect("outside asset");

    fs::write(root.join(MANIFEST_FILE_NAME), VALID).expect("write manifest");
    fs::create_dir_all(root.join("bin")).expect("bin");
    fs::write(root.join("bin/chess"), b"").expect("entrypoint");
    fs::write(root.join("icon.png"), b"").expect("asset");
    symlink(outside.path(), root.join("assets")).expect("symlink");

    let manifest = Manifest::read_package(root).expect("manifest");
    match manifest.validate_payload(root) {
        Err(PayloadError::SymlinkedPath { path, component }) => {
            assert_eq!(path, "assets/board.toml");
            assert_eq!(component, "assets");
        }
        other => panic!("a symlinked directory must be refused, got {other:?}"),
    }
}

#[test]
fn an_oversized_manifest_is_refused_before_it_is_parsed() {
    let padded = format!("{VALID}\n# {}\n", "p".repeat(MAX_MANIFEST_BYTES as usize));
    match Manifest::parse(&padded) {
        Err(ManifestError::TooLarge { len, max, .. }) => {
            assert!(len > max);
            assert_eq!(max, MAX_MANIFEST_BYTES);
        }
        other => panic!("an oversized manifest must be refused, got {other:?}"),
    }

    let package = tempfile::tempdir().expect("tempdir");
    fs::write(package.path().join(MANIFEST_FILE_NAME), &padded).expect("write manifest");
    assert!(matches!(
        Manifest::read_package(package.path()),
        Err(ManifestError::TooLarge { .. })
    ));
}

#[test]
fn an_unbounded_asset_list_is_refused() {
    let assets: Vec<String> = (0..=MAX_ASSETS).map(|i| format!("\"a{i}.bin\"")).collect();
    let text = VALID.replace(
        "assets = [\"assets/board.toml\", \"icon.png\"]",
        &format!("assets = [{}]", assets.join(", ")),
    );
    match Manifest::parse(&text) {
        Err(ManifestError::TooManyAssets { declared, max }) => {
            assert_eq!(declared, MAX_ASSETS + 1);
            assert_eq!(max, MAX_ASSETS);
        }
        other => panic!("an unbounded asset list must be refused, got {other:?}"),
    }
}

#[test]
fn a_top_level_grants_table_is_refused_by_name() {
    // `capabilities` and `permissions` already were; `grants` fell through to
    // a generic unknown-key error, which says the wrong thing about why.
    match Manifest::parse(&format!("{VALID}\n[grants]\nnetwork = true\n")) {
        Err(ManifestError::SelfGrantedCapabilities { key }) => assert_eq!(key, "grants"),
        other => panic!("a top-level `grants` table must be named, got {other:?}"),
    }
}
