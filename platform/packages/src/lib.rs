//! The Paperclip package format (§8) and the manifest it is described by (§12).
//!
//! A package is a directory containing a `paper.toml` manifest plus the payload
//! it declares. This crate can read that manifest, say precisely why a bad one
//! is bad, and check a payload directory against it.
//!
//! The one rule worth stating up front: **a manifest cannot grant itself
//! capabilities.** [`Manifest`] has no capability field and no way to gain one;
//! the only value that says what an app may do is [`GrantedCapabilities`],
//! which is not deserialisable and is produced solely by [`InstallPolicy`] at
//! install time. Declaring `capabilities` in a `paper.toml` is a parse error,
//! not a request.

mod capability;
mod error;
mod id;
mod manifest;
mod path;

pub use capability::{Capability, GrantedCapabilities, InstallPolicy, InstalledApp};
pub use error::{IdError, ManifestError, NameError, PathError, PayloadError};
pub use id::{AppId, DisplayName};
pub use manifest::{MANIFEST_FILE_NAME, MAX_ASSETS, MAX_MANIFEST_BYTES, Manifest};
pub use path::RelativePath;
