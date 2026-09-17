//! Manifest parsing and validation, exercised through the public API only.

use std::fs;

use paper_packages::{
    Capability, InstallPolicy, InstalledApp, ManifestError, PathError, PayloadError,
};
use paper_packages::{MANIFEST_FILE_NAME, Manifest};

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
    assert!(matches!(
        Manifest::parse(&VALID.replace("protocol = \"1.0\"", "protocol = \"1.0.0\"")),
        Err(ManifestError::Schema(_))
    ));
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
