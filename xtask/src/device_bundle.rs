//! Builds `paperctl` for the tablet and stages it as a bundle ready to `scp`.
//!
//! `tools/cross/build-device.sh --bin paperctl` already does the hard part —
//! the Docker container, the GCC toolchain the vendor C++ ABI needs (see its
//! own header comment) — and this does not duplicate that. What it adds is
//! the part worth not doing by hand every time: staging the binary under a
//! fixed path with its SHA-256 alongside it, so "this is the bundle I built
//! this morning" is a byte comparison rather than a memory.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// Where `build-device.sh` leaves the cross-compiled binary.
const BUILD_SCRIPT: &str = "tools/cross/build-device.sh";

/// Where a bundle is staged, under the workspace `target/` directory.
const BUNDLE_DIR: &str = "target/device-bundle";

#[derive(Debug, thiserror::Error)]
pub(crate) enum DeviceBundleError {
    #[error("cannot run {BUILD_SCRIPT}")]
    Spawn(#[source] std::io::Error),
    #[error("{BUILD_SCRIPT} exited with {0}")]
    BuildFailed(std::process::ExitStatus),
    #[error("cannot create the bundle directory {path}")]
    CreateDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read the built binary at {path}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// What `run` produced: a staged binary and the digest that names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bundle {
    pub binary: PathBuf,
    pub digest_hex: String,
}

/// The digest of `bytes`, hex-encoded — same algorithm and spelling as every
/// other digest in the repository (§12).
fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Runs `tools/cross/build-device.sh --bin paperctl` under `repo_root`, then
/// stages the result into `target/device-bundle/`.
///
/// Requires everything `build-device.sh` requires — Docker, and the Qt
/// headers and vendor library it names — and fails with that script's own
/// message if they are missing. Nothing here talks to a tablet.
pub(crate) fn run(repo_root: &Path) -> Result<Bundle, DeviceBundleError> {
    let status = Command::new(repo_root.join(BUILD_SCRIPT))
        .arg("--bin")
        .arg("paperctl")
        .current_dir(repo_root)
        .status()
        .map_err(DeviceBundleError::Spawn)?;
    if !status.success() {
        return Err(DeviceBundleError::BuildFailed(status));
    }

    let built = repo_root.join("target/device-container/release/paperctl");
    let mut file = fs::File::open(&built).map_err(|source| DeviceBundleError::Read {
        path: built.display().to_string(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| DeviceBundleError::Read {
            path: built.display().to_string(),
            source,
        })?;

    let bundle_dir = repo_root.join(BUNDLE_DIR);
    fs::create_dir_all(&bundle_dir).map_err(|source| DeviceBundleError::CreateDir {
        path: bundle_dir.display().to_string(),
        source,
    })?;

    let staged = bundle_dir.join("paperctl");
    fs::write(&staged, &bytes).map_err(|source| DeviceBundleError::Write {
        path: staged.display().to_string(),
        source,
    })?;

    let digest_hex = hex_digest(&bytes);
    let digest_path = bundle_dir.join("paperctl.sha256");
    fs::write(&digest_path, format!("{digest_hex}  paperctl\n")).map_err(|source| {
        DeviceBundleError::Write {
            path: digest_path.display().to_string(),
            source,
        }
    })?;

    Ok(Bundle {
        binary: staged,
        digest_hex,
    })
}

#[cfg(test)]
mod tests {
    use super::hex_digest;

    #[test]
    fn digests_are_lowercase_hex_sha256() {
        // Empty-input SHA-256, a well-known constant, so this catches a
        // transposed byte order without needing a second implementation to
        // compare against.
        assert_eq!(
            hex_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
