//! `paper.toml`: what an app says about itself (§12).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use paper_protocol::ProtocolVersion;
use semver::Version;
use serde::Deserialize;
use serde::de::Error as _;
use serde::de::IgnoredAny;

use crate::error::{ManifestError, PayloadError};
use crate::id::{AppId, DisplayName};
use crate::path::RelativePath;

/// The manifest file name, at the root of every package.
pub const MANIFEST_FILE_NAME: &str = "paper.toml";

/// A parsed, validated `paper.toml`.
///
/// Every field is already a checked type, so nothing downstream re-validates:
/// if you are holding a `Manifest`, the id is well formed, the version is
/// SemVer, and the paths stay inside the package.
///
/// There is no capability field here and there never will be — see
/// [`capability`](crate::Capability) for why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    id: AppId,
    name: DisplayName,
    version: Version,
    protocol: ProtocolVersion,
    entrypoint: RelativePath,
    assets: Vec<RelativePath>,
}

impl Manifest {
    /// Reads and validates the `paper.toml` at the root of a package directory.
    pub fn read_package(root: &Path) -> Result<Self, ManifestError> {
        let path = root.join(MANIFEST_FILE_NAME);
        let text = fs::read_to_string(&path).map_err(|source| ManifestError::Read {
            path: path.clone(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Validates manifest text.
    ///
    /// This checks everything that can be decided from the text alone. It does
    /// deliberately **not** check protocol compatibility with the running
    /// platform: the App Store has to be able to read and display a manifest
    /// for an app this device cannot run. Use [`Self::ensure_runnable`] at the
    /// point where running it is actually the question.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let table: toml::Table = text.parse().map_err(ManifestError::Syntax)?;
        let raw: RawManifest = table.try_into().map_err(ManifestError::Schema)?;

        if raw.capabilities.is_some() {
            return Err(ManifestError::SelfGrantedCapabilities {
                key: "capabilities",
            });
        }
        if raw.permissions.is_some() {
            return Err(ManifestError::SelfGrantedCapabilities { key: "permissions" });
        }
        let app = raw.app;
        for (present, key) in [
            (app.capabilities.is_some(), "app.capabilities"),
            (app.permissions.is_some(), "app.permissions"),
            (app.grants.is_some(), "app.grants"),
        ] {
            if present {
                return Err(ManifestError::SelfGrantedCapabilities { key });
            }
        }

        let id = app
            .id
            .parse::<AppId>()
            .map_err(|source| ManifestError::AppId {
                value: app.id.clone(),
                source,
            })?;
        let name =
            app.name
                .parse::<DisplayName>()
                .map_err(|source| ManifestError::DisplayName {
                    value: app.name.clone(),
                    source,
                })?;
        let version = Version::parse(&app.version).map_err(|source| ManifestError::Version {
            value: app.version.clone(),
            source,
        })?;
        let protocol = app
            .protocol
            .parse::<ProtocolVersion>()
            .map_err(|_| ManifestError::Schema(unreadable_protocol(&app.protocol)))?;
        let entrypoint =
            app.entrypoint
                .parse::<RelativePath>()
                .map_err(|source| ManifestError::Entrypoint {
                    value: app.entrypoint.clone(),
                    source,
                })?;

        let mut seen = BTreeSet::new();
        let mut assets = Vec::with_capacity(app.assets.len());
        for (index, declared) in app.assets.iter().enumerate() {
            let asset =
                declared
                    .parse::<RelativePath>()
                    .map_err(|source| ManifestError::Asset {
                        index,
                        value: declared.clone(),
                        source,
                    })?;
            if !seen.insert(asset.clone()) {
                return Err(ManifestError::DuplicateAsset {
                    value: declared.clone(),
                });
            }
            assets.push(asset);
        }

        Ok(Self {
            id,
            name,
            version,
            protocol,
            entrypoint,
            assets,
        })
    }

    /// Checks that this platform build can actually run the app.
    pub fn ensure_runnable(&self) -> Result<(), ManifestError> {
        if paper_protocol::CURRENT.can_run(self.protocol) {
            Ok(())
        } else {
            Err(ManifestError::UnsupportedProtocol {
                declared: self.protocol,
                current: paper_protocol::CURRENT,
            })
        }
    }

    /// Checks a payload directory against what the manifest declared.
    ///
    /// Separate from [`Self::parse`] because the two answer different
    /// questions at different times: a catalog entry is parsed without its
    /// payload in hand, and a staged package is checked before it is moved
    /// into place.
    pub fn validate_payload(&self, root: &Path) -> Result<(), PayloadError> {
        let entrypoint = self.entrypoint.resolve_within(root);
        match fs::metadata(&entrypoint) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => {
                return Err(PayloadError::EntrypointNotAFile {
                    path: self.entrypoint.as_str().to_owned(),
                });
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(PayloadError::MissingEntrypoint {
                    path: self.entrypoint.as_str().to_owned(),
                });
            }
            Err(source) => {
                return Err(PayloadError::Io {
                    path: entrypoint,
                    source,
                });
            }
        }

        for asset in &self.assets {
            let resolved = asset.resolve_within(root);
            match fs::metadata(&resolved) {
                Ok(_) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    return Err(PayloadError::MissingAsset {
                        path: asset.as_str().to_owned(),
                    });
                }
                Err(source) => {
                    return Err(PayloadError::Io {
                        path: resolved,
                        source,
                    });
                }
            }
        }

        Ok(())
    }

    /// The stable app id.
    pub fn id(&self) -> &AppId {
        &self.id
    }

    /// The label to draw on the shelf.
    pub fn name(&self) -> &DisplayName {
        &self.name
    }

    /// The app's own SemVer version.
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The platform protocol the app was built against.
    pub fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// The executable to launch, relative to the package root.
    pub fn entrypoint(&self) -> &RelativePath {
        &self.entrypoint
    }

    /// Files the package promises to ship.
    pub fn assets(&self) -> &[RelativePath] {
        &self.assets
    }
}

/// Builds a schema error for a protocol string TOML itself was happy with.
fn unreadable_protocol(value: &str) -> toml::de::Error {
    toml::de::Error::custom(format!(
        "app.protocol `{value}` is not a `major.minor` protocol version"
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    app: RawApp,
    #[serde(default)]
    capabilities: Option<IgnoredAny>,
    #[serde(default)]
    permissions: Option<IgnoredAny>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawApp {
    id: String,
    name: String,
    version: String,
    protocol: String,
    entrypoint: String,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    capabilities: Option<IgnoredAny>,
    #[serde(default)]
    permissions: Option<IgnoredAny>,
    #[serde(default)]
    grants: Option<IgnoredAny>,
}
