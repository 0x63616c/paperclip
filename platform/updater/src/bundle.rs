//! Building a platform bundle. Publishing machines only (§12, §13).
//!
//! A bundle is a gzipped tar holding exactly one platform release:
//!
//! ```text
//! platform.toml        the manifest, signed
//! platform.toml.sig    its detached signature
//! bin/paperclip-host
//! bin/home
//! bin/app-store
//! bin/settings
//! ```
//!
//! Deterministic, for the same reason app packages are: entries sorted,
//! ownership and timestamps zeroed, permissions assigned rather than copied
//! off the build machine. Building the same tree twice produces the same
//! bytes, which is what makes "this version was published with these bytes" a
//! question with an answer.
//!
//! This whole module is behind `publishing`, so the device build cannot
//! contain it. The tablet verifies; it does not produce.

use std::fs;
use std::io::Write;
use std::path::Path;

use paper_packages::Digest;
use paper_packages::signing::Signature;
use paper_protocol::ProtocolVersion;
use semver::Version;

use crate::error::UpdateError;
use crate::manifest::{
    Component, MANIFEST_FILE_NAME, PlatformManifest, REQUIRED_COMPONENTS, SIGNATURE_FILE_NAME,
};

/// Everything about a release that is not read off the disk.
///
/// A struct rather than eight parameters. Two of the fields are `u32`s that
/// mean different things — `state_version` and `rollback_to_state` — and a
/// positional call site is one editing mistake away from swapping them, which
/// would produce a release that silently claims rollback is safe when it is
/// not.
#[derive(Debug, Clone)]
pub struct Description<'a> {
    /// The release version.
    pub version: Version,
    /// The protocol this platform speaks.
    pub protocol: ProtocolVersion,
    /// When it was published, seconds since the epoch.
    pub published: u64,
    /// The persistent state version this release writes.
    pub state_version: u32,
    /// The lowest state version that can still read what this release writes.
    pub rollback_to_state: u32,
    /// Release notes.
    pub notes: String,
    /// Extra files to ship beyond the four required components, relative to
    /// the source directory.
    pub extras: &'a [&'a str],
}

/// Measures the components in `source` and describes them as a manifest.
///
/// Every required component is expected at `bin/<name>`. Sizes and digests are
/// read from the files on disk, so a manifest can never describe a build that
/// is not the one in the directory.
///
/// # Errors
///
/// A missing or unreadable component, or a manifest that does not satisfy
/// [`PlatformManifest::new`].
pub fn describe(
    source: &Path,
    description: &Description<'_>,
) -> Result<PlatformManifest, UpdateError> {
    let mut components =
        Vec::with_capacity(REQUIRED_COMPONENTS.len() + description.extras.len());
    for name in REQUIRED_COMPONENTS {
        let relative = format!("bin/{name}");
        components.push(measure(source, name, &relative)?);
    }
    // Anything else the release ships. Covered by the same digests; not
    // required to be an executable, because not everything in a release is
    // one.
    for extra in description.extras {
        components.push(measure(source, extra, extra)?);
    }
    PlatformManifest::new(
        description.version.clone(),
        description.protocol,
        description.published,
        description.state_version,
        description.rollback_to_state,
        description.notes.clone(),
        components,
    )
}

/// Reads one component and records its size and digest.
fn measure(source: &Path, name: &str, relative: &str) -> Result<Component, UpdateError> {
    let path = source.join(relative);
    let bytes = fs::read(&path).map_err(|source| UpdateError::Component {
        name: name.to_owned(),
        reason: format!("{}: {source}", path.display()),
    })?;
    Ok(Component::new(
        name,
        relative,
        bytes.len() as u64,
        Digest::of_bytes(&bytes),
    ))
}

/// Writes the bundle for `manifest`, reading the components out of `source`.
///
/// `document` must be the exact bytes `signature` was made over — not a
/// re-render of the manifest. Passing the rendered bytes through rather than
/// rendering them again here is the same discipline
/// [`paper_packages::release`] follows: TOML has no canonical form, and a
/// scheme in which two renders must agree is a scheme that breaks on a patch
/// release of a dependency.
///
/// # Errors
///
/// Any component that cannot be read, or a sink that cannot be written.
pub fn build(
    source: &Path,
    manifest: &PlatformManifest,
    document: &[u8],
    signature: &Signature,
    sink: impl Write,
) -> Result<(), UpdateError> {
    let encoder = flate2::write::GzEncoder::new(sink, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);

    append(&mut archive, MANIFEST_FILE_NAME, document, 0o644)?;
    append(
        &mut archive,
        SIGNATURE_FILE_NAME,
        signature.to_armoured().as_bytes(),
        0o644,
    )?;

    // Sorted, so the bytes do not depend on directory order.
    let mut components: Vec<_> = manifest.components().iter().collect();
    components.sort_by(|left, right| left.path().cmp(right.path()));
    for component in components {
        let path = source.join(component.path());
        let bytes = fs::read(&path).map_err(|error| UpdateError::io(&path, error))?;
        let mode = if component.path().starts_with("bin/") {
            0o755
        } else {
            0o644
        };
        append(&mut archive, component.path(), &bytes, mode)?;
    }

    let encoder = archive
        .into_inner()
        .map_err(|error| UpdateError::io(source, error))?;
    encoder
        .finish()
        .map_err(|error| UpdateError::io(source, error))?;
    Ok(())
}

/// Appends one file with everything machine-specific zeroed out.
fn append<W: Write>(
    archive: &mut tar::Builder<W>,
    path: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<(), UpdateError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    archive
        .append_data(&mut header, path, bytes)
        .map_err(|error| UpdateError::io(path, error))
}
