//! The signed platform manifest: one version of the whole platform (§13).
//!
//! # One document, one signature, four required components
//!
//! §13 says the Host, protocol support, platform integration, Home and the App
//! Store form **one tested platform release**, and this project's own decision
//! adds Settings to the set that always ships. So a platform release is not
//! four independently versioned things that happen to be installed together —
//! it is one version, and the manifest names every binary in it with its size
//! and digest.
//!
//! The signature covers the manifest bytes, exactly as
//! [`paper_packages::release`] does it and for the same reason: the bytes are
//! rendered once by the publisher, written, and signed. Verification never
//! re-renders. A scheme that depends on two serialisers agreeing breaks on a
//! patch release of `toml`.
//!
//! The domain separator is [`Domain::PLATFORM`], not
//! [`Domain::RELEASE`](paper_packages::signing::Domain::RELEASE). That is what
//! makes "publishing an app produced a document that verifies as a replacement
//! Host" impossible rather than unlikely.
//!
//! # The two state versions, and what rollback needs from them
//!
//! `state_version` is the version of the platform's persistent state this
//! release writes. `rollback_to_state` is the *lowest* state version that can
//! still read what this release writes.
//!
//! Those are different questions and §13 needs the second one. Rolling back is
//! only a symlink swap when the older release can read the state the newer one
//! left behind. When it cannot, the updater takes a snapshot before it
//! activates, and rollback restores it. A platform that "rolled back" by
//! swapping executables over state they cannot parse has not rolled back; it
//! has produced a second failure with the first one's evidence destroyed.

use std::collections::BTreeSet;
use std::path::Path;

use paper_packages::signing::{Domain, KeyId, Verified};
use paper_packages::{Digest, ExecutableTarget, inspect_entrypoint};
use paper_protocol::ProtocolVersion;
use semver::Version;
use serde::Deserialize;

use crate::error::UpdateError;

/// The file name a platform manifest is published under.
pub const MANIFEST_FILE_NAME: &str = "platform.toml";

/// Its detached signature.
pub const SIGNATURE_FILE_NAME: &str = "platform.toml.sig";

/// Largest platform manifest that will be read, in bytes.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// The components every platform release must carry.
///
/// Not configurable, and not `#[non_exhaustive]`. §13 makes these one tested
/// release; a manifest that omits one is describing something that has not
/// been tested as a platform, whatever else it is.
pub const REQUIRED_COMPONENTS: [&str; 4] = ["paperclip-host", "home", "app-store", "settings"];

/// One binary in a platform release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    name: String,
    path: String,
    size: u64,
    digest: Digest,
}

impl Component {
    /// Describes a component. Publisher side.
    pub fn new(
        name: impl Into<String>,
        path: impl Into<String>,
        size: u64,
        digest: Digest,
    ) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            size,
            digest,
        }
    }

    /// The component's name, one of [`REQUIRED_COMPONENTS`] or an extra.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where it sits inside the release directory.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Its exact size.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Its digest.
    pub fn digest(&self) -> Digest {
        self.digest
    }
}

/// What a component has to satisfy to be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentPolicy {
    /// The ELF `e_machine` every component under `bin/` must be built for.
    ///
    /// Only `bin/`: a platform release may carry assets — a font, a script a
    /// release ships for its own use — and every component is covered by the
    /// manifest's digests whether or not it is an executable. Running the ELF
    /// check over a font would refuse a valid release for not being a program.
    ///
    /// `None` accepts any machine and exists for unit tests that verify the
    /// transaction rather than the binaries. Nothing that runs on a device
    /// constructs it that way; [`Self::for_this_machine`] is what `paperctl`
    /// uses, and on the tablet that is aarch64.
    pub machine: Option<u16>,
}

impl ComponentPolicy {
    /// Require every component to be built for the machine the updater is
    /// running on.
    ///
    /// The updater runs *on the device*, so "this machine" is the right
    /// question — and it is the one question that stays right in the VM
    /// harness, where a hardcoded aarch64 would fail every run on an x86 host.
    pub fn for_this_machine() -> Self {
        Self {
            machine: Some(this_machine()),
        }
    }

    /// Accept any machine. Unit tests only; see [`Self::machine`].
    pub fn any_machine() -> Self {
        Self { machine: None }
    }
}

/// This build's ELF `e_machine`.
const fn this_machine() -> u16 {
    if cfg!(target_arch = "aarch64") {
        paper_packages::MACHINE_AARCH64
    } else if cfg!(target_arch = "x86_64") {
        0x3E
    } else {
        0
    }
}

/// One version of the whole platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformManifest {
    version: Version,
    protocol: ProtocolVersion,
    published: u64,
    state_version: u32,
    rollback_to_state: u32,
    notes: String,
    components: Vec<Component>,
}

impl PlatformManifest {
    /// Describes a platform release. Publisher side.
    ///
    /// # Errors
    ///
    /// A missing required component, a duplicate name, or
    /// `rollback_to_state > state_version`, which would say the release cannot
    /// read its own writes.
    pub fn new(
        version: Version,
        protocol: ProtocolVersion,
        published: u64,
        state_version: u32,
        rollback_to_state: u32,
        notes: impl Into<String>,
        components: Vec<Component>,
    ) -> Result<Self, UpdateError> {
        let manifest = Self {
            version,
            protocol,
            published,
            state_version,
            rollback_to_state,
            notes: notes.into(),
            components,
        };
        manifest.check()?;
        Ok(manifest)
    }

    fn check(&self) -> Result<(), UpdateError> {
        if self.rollback_to_state > self.state_version {
            return Err(UpdateError::Manifest {
                reason: format!(
                    "rollback_to_state {} is above state_version {}; a release must be able \
                     to read what it writes",
                    self.rollback_to_state, self.state_version
                ),
            });
        }
        let mut seen = BTreeSet::new();
        for component in &self.components {
            if !seen.insert(component.name.as_str()) {
                return Err(UpdateError::Manifest {
                    reason: format!("component `{}` is listed twice", component.name),
                });
            }
            if component.path.starts_with('/') || component.path.contains("..") {
                return Err(UpdateError::Manifest {
                    reason: format!(
                        "component `{}` has path `{}`, which escapes the release directory",
                        component.name, component.path
                    ),
                });
            }
        }
        for required in REQUIRED_COMPONENTS {
            if !seen.contains(required) {
                return Err(UpdateError::Manifest {
                    reason: format!(
                        "no `{required}` component; §13 makes the host, Home, the App Store and \
                         Settings one tested release"
                    ),
                });
            }
        }
        Ok(())
    }

    /// The release version.
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The protocol this platform speaks.
    pub fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// When it was published, seconds since the epoch.
    pub fn published(&self) -> u64 {
        self.published
    }

    /// The persistent state version this release writes.
    pub fn state_version(&self) -> u32 {
        self.state_version
    }

    /// The lowest state version that can still read what this release writes.
    pub fn rollback_to_state(&self) -> u32 {
        self.rollback_to_state
    }

    /// Release notes.
    pub fn notes(&self) -> &str {
        &self.notes
    }

    /// Every component.
    pub fn components(&self) -> &[Component] {
        &self.components
    }

    /// One component by name.
    pub fn component(&self, name: &str) -> Option<&Component> {
        self.components
            .iter()
            .find(|component| component.name == name)
    }

    /// Whether a release at `older` state version can read what this release
    /// writes — the question rollback actually asks.
    pub fn readable_by_state_version(&self, older: u32) -> bool {
        older >= self.rollback_to_state
    }

    /// Checks every component against the bytes in `release`.
    ///
    /// Size first, then digest, then the ELF header. Size first because it is
    /// the cheap one and because a size mismatch tells whoever reads the error
    /// something a digest mismatch does not: that the file is the wrong file,
    /// not a corrupted one.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Component`] naming the first component that disagrees.
    pub fn verify_payload(
        &self,
        release: &Path,
        policy: ComponentPolicy,
    ) -> Result<(), UpdateError> {
        for component in &self.components {
            let path = release.join(&component.path);
            let metadata = std::fs::metadata(&path).map_err(|source| UpdateError::Component {
                name: component.name.clone(),
                reason: format!("{}: {source}", path.display()),
            })?;
            if metadata.len() != component.size {
                return Err(UpdateError::Component {
                    name: component.name.clone(),
                    reason: format!(
                        "the manifest says {} bytes, the file is {}",
                        component.size,
                        metadata.len()
                    ),
                });
            }
            let bytes = std::fs::read(&path).map_err(|source| UpdateError::Component {
                name: component.name.clone(),
                reason: format!("{}: {source}", path.display()),
            })?;
            let digest = Digest::of_bytes(&bytes);
            if digest != component.digest {
                return Err(UpdateError::Component {
                    name: component.name.clone(),
                    reason: format!(
                        "the manifest says {}, the file is {digest}",
                        component.digest
                    ),
                });
            }
            if let Some(machine) = policy.machine
                && component.path.starts_with("bin/")
            {
                let target: ExecutableTarget =
                    inspect_entrypoint(&path).map_err(|source| UpdateError::Component {
                        name: component.name.clone(),
                        reason: format!("{}: {source}", path.display()),
                    })?;
                if target.machine != machine {
                    return Err(UpdateError::Component {
                        name: component.name.clone(),
                        reason: format!(
                            "built for ELF machine {:#x}, this platform needs {machine:#x}",
                            target.machine
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    /// Renders the document that gets signed. Publisher side.
    ///
    /// Written once, by the publisher, and never again. Verification parses
    /// these bytes; it does not re-render them.
    #[cfg(feature = "publishing")]
    pub fn to_document(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let _ = writeln!(out, "version = \"{}\"", self.version);
        let _ = writeln!(out, "protocol = \"{}\"", self.protocol);
        let _ = writeln!(out, "published = {}", self.published);
        let _ = writeln!(out, "state_version = {}", self.state_version);
        let _ = writeln!(out, "rollback_to_state = {}", self.rollback_to_state);
        if !self.notes.is_empty() {
            let _ = writeln!(out, "notes = \"{}\"", escape(&self.notes));
        }
        for component in &self.components {
            let _ = writeln!(out, "\n[[component]]");
            let _ = writeln!(out, "name = \"{}\"", escape(&component.name));
            let _ = writeln!(out, "path = \"{}\"", escape(&component.path));
            let _ = writeln!(out, "size = {}", component.size);
            let _ = writeln!(out, "digest = \"{}\"", component.digest);
        }
        out
    }
}

/// TOML escaping for the few characters that can appear in a name or a note.
#[cfg(feature = "publishing")]
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A platform manifest whose signature has been checked.
///
/// Cannot be built any other way. Every path that acts on a manifest takes one
/// of these, so there is no code path that parses first and looks for a
/// signature afterwards — the ordering mistake that quietly makes a signature
/// scheme decorative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPlatform {
    manifest: PlatformManifest,
    signer: KeyId,
}

impl VerifiedPlatform {
    /// The manifest.
    pub fn manifest(&self) -> &PlatformManifest {
        &self.manifest
    }

    /// Who signed it.
    pub fn signer(&self) -> KeyId {
        self.signer
    }
}

/// Parses a platform manifest from bytes that have already been verified.
///
/// # Errors
///
/// A document that is not TOML, is missing a field, or does not satisfy
/// [`PlatformManifest::new`]'s rules.
pub fn parse(verified: Verified<'_>) -> Result<VerifiedPlatform, UpdateError> {
    debug_assert_eq!(verified.domain(), Domain::PLATFORM);
    let text = std::str::from_utf8(verified.bytes()).map_err(|_| UpdateError::Manifest {
        reason: "not UTF-8".to_owned(),
    })?;
    let raw: RawManifest = toml::from_str(text).map_err(|source| UpdateError::Manifest {
        reason: source.to_string(),
    })?;

    let version = Version::parse(&raw.version).map_err(|source| UpdateError::Manifest {
        reason: format!("version `{}`: {source}", raw.version),
    })?;
    let protocol =
        raw.protocol
            .parse::<ProtocolVersion>()
            .map_err(|source| UpdateError::Manifest {
                reason: format!("protocol `{}`: {source}", raw.protocol),
            })?;
    let mut components = Vec::with_capacity(raw.component.len());
    for raw_component in raw.component {
        let digest =
            raw_component
                .digest
                .parse::<Digest>()
                .map_err(|source| UpdateError::Manifest {
                    reason: format!("component `{}` digest: {source}", raw_component.name),
                })?;
        components.push(Component::new(
            raw_component.name,
            raw_component.path,
            raw_component.size,
            digest,
        ));
    }

    let manifest = PlatformManifest::new(
        version,
        protocol,
        raw.published,
        raw.state_version,
        raw.rollback_to_state,
        raw.notes.unwrap_or_default(),
        components,
    )?;
    Ok(VerifiedPlatform {
        manifest,
        signer: verified.signer(),
    })
}

/// Reads and verifies the manifest of a release directory already on disk.
///
/// # Errors
///
/// A missing manifest or signature, an untrusted signer, or an unusable
/// document.
pub fn read_release(
    release: &Path,
    keys: &paper_packages::signing::TrustedKeys,
) -> Result<VerifiedPlatform, UpdateError> {
    let manifest_path = release.join(MANIFEST_FILE_NAME);
    let signature_path = release.join(SIGNATURE_FILE_NAME);
    let document =
        std::fs::read(&manifest_path).map_err(|source| UpdateError::io(&manifest_path, source))?;
    if document.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(UpdateError::Manifest {
            reason: format!(
                "{} bytes, over the {MAX_MANIFEST_BYTES} limit",
                document.len()
            ),
        });
    }
    let armoured = std::fs::read_to_string(&signature_path)
        .map_err(|source| UpdateError::io(&signature_path, source))?;
    let signature = armoured.trim().parse()?;
    let verified = keys.verify(Domain::PLATFORM, &document, &signature)?;
    parse(verified)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    version: String,
    protocol: String,
    published: u64,
    state_version: u32,
    rollback_to_state: u32,
    notes: Option<String>,
    #[serde(default)]
    component: Vec<RawComponent>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComponent {
    name: String,
    path: String,
    size: u64,
    digest: String,
}
