//! The Paperclip package format (§8), and everything that happens to a package
//! between a directory on the Mac and a selected release on the tablet (§11, §12).
//!
//! # The rule worth stating up front
//!
//! **A manifest cannot grant itself capabilities.** [`Manifest`] has no
//! capability field and no way to gain one; the only value that says what an
//! app may do is [`GrantedCapabilities`], which is not deserialisable and is
//! produced solely by [`InstallPolicy`] at install time. Declaring
//! `capabilities` in a `paper.toml` is a parse error, not a request.
//!
//! # Where things are
//!
//! §7 asks for private modules with a crate's surface in its `lib.rs`
//! re-exports, and the manifest half of this crate is exactly that: [`Manifest`],
//! [`AppId`], [`RelativePath`] and the typed errors are re-exported here and
//! their modules are private.
//!
//! The rest is not one thing, and flattening it would lose the part that
//! matters. Each of these is a public module because its **module
//! documentation is part of its contract** — `signing` states precisely which
//! bytes a signature covers, `store` states what makes a commit durable, and
//! neither survives being reduced to a list of re-exported names:
//!
//! | Module | What it owns |
//! |---|---|
//! | [`archive`] | `.paperpkg`: deterministic building, and hostile extraction |
//! | [`signing`] | Ed25519, and exactly which bytes are signed |
//! | [`release`] | The two signed documents: release descriptors and the catalog index |
//! | [`catalog`] | Fetching and verifying a catalog, and refusing to go backwards |
//! | [`store`] | The §11 layout, and the durability primitives |
//! | [`install`] | The install transaction: stage, verify, commit, activate, recover |
//! | [`launch`] | Whether a release has ever actually started |
//! | [`publish`] | Writing a catalog. Publishing machines only |
//!
//! There is one path to each type. A type reachable through its module is not
//! also re-exported here, so `paper_packages::install::InstallError` is its
//! name and there is no second one to keep in step.

pub mod archive;
mod binary;
mod capability;
pub mod catalog;
mod check;
mod digest;
mod error;
mod id;
pub mod install;
pub mod launch;
mod manifest;
mod path;
#[cfg(feature = "publishing")]
pub mod publish;
pub mod release;
pub mod signing;
pub mod store;

pub use binary::{
    BinaryError, ExecutableTarget, MACHINE_AARCH64, ObjectKind, inspect_entrypoint,
    require_device_entrypoint,
};
pub use capability::{Capability, GrantedCapabilities, InstallPolicy, InstalledApp};
pub use check::{
    CheckError, MAX_ASSET_BYTES, MAX_ENTRYPOINT_BYTES, MAX_PACKAGE_BYTES, PackageCheck,
};
pub use digest::{Digest, DigestError, MeasuredReader};
pub use error::{IdError, ManifestError, NameError, PathError, PayloadError};
pub use id::{AppId, DisplayName};
pub use manifest::{MANIFEST_FILE_NAME, MAX_ASSETS, MAX_MANIFEST_BYTES, Manifest};
pub use paper_protocol::RelativePath;
