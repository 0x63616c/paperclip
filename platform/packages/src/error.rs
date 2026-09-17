//! Typed failures for manifests and package payloads (§12).
//!
//! Every variant names the field it is about and what was wrong with it, so a
//! `paperctl` user reading one message knows which line of `paper.toml` to
//! edit. Nothing here is stringly typed except the offending values themselves.

use std::io;
use std::path::PathBuf;

use paper_protocol::ProtocolVersion;

/// Why a `paper.toml` could not be turned into a [`Manifest`](crate::Manifest).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// The file is not valid TOML.
    #[error("paper.toml is not valid TOML")]
    Syntax(#[source] toml::de::Error),

    /// A required table or key is absent, or an unexpected one is present.
    #[error("paper.toml does not match the manifest schema")]
    Schema(#[source] toml::de::Error),

    /// The manifest tried to award itself capabilities.
    ///
    /// Kept as its own variant rather than folded into [`Self::Schema`]
    /// because the answer is not "you typed the key wrong", it is "that
    /// decision is not yours to make".
    #[error(
        "paper.toml declares `{key}`: an app cannot grant itself capabilities. \
         Capabilities are install policy, decided by the host at install time."
    )]
    SelfGrantedCapabilities {
        /// The reserved key that was present.
        key: &'static str,
    },

    /// `app.id` is not a usable app id.
    #[error("app.id `{value}` is not a valid app id")]
    AppId {
        /// The id as written.
        value: String,
        /// What specifically was wrong.
        #[source]
        source: IdError,
    },

    /// `app.name` is not a usable display name.
    #[error("app.name `{value}` is not a valid display name")]
    DisplayName {
        /// The name as written.
        value: String,
        /// What specifically was wrong.
        #[source]
        source: NameError,
    },

    /// `app.version` is not valid SemVer. Required by §4; not negotiable.
    #[error("app.version `{value}` is not valid SemVer")]
    Version {
        /// The version as written.
        value: String,
        /// The SemVer parser's complaint.
        #[source]
        source: semver::Error,
    },

    /// `app.entrypoint` is absolute, escapes the package, or is otherwise unsafe.
    #[error("app.entrypoint `{value}` is not a safe package-relative path")]
    Entrypoint {
        /// The path as written.
        value: String,
        /// What specifically was wrong.
        #[source]
        source: PathError,
    },

    /// One of `app.assets` is absolute, escapes the package, or is otherwise unsafe.
    #[error("app.assets[{index}] `{value}` is not a safe package-relative path")]
    Asset {
        /// Position in the `assets` array, so the offending line is findable.
        index: usize,
        /// The path as written.
        value: String,
        /// What specifically was wrong.
        #[source]
        source: PathError,
    },

    /// The same asset path is declared more than once.
    #[error("app.assets declares `{value}` twice")]
    DuplicateAsset {
        /// The repeated path.
        value: String,
    },

    /// The app was built against a protocol this platform cannot speak.
    #[error("app declares protocol {declared}, which this platform ({current}) cannot run")]
    UnsupportedProtocol {
        /// What the manifest asked for.
        declared: ProtocolVersion,
        /// What this build of the platform speaks.
        current: ProtocolVersion,
    },

    /// The manifest file itself could not be read.
    #[error("cannot read {path}")]
    Read {
        /// The path that was attempted.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },
}

/// Why a string is not a valid [`AppId`](crate::AppId).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdError {
    /// An app id must be at least `vendor.app`.
    #[error("needs at least two dot-separated segments, e.g. `dev.calum.chess`")]
    TooFewSegments,
    /// An empty segment, from a leading, trailing or doubled dot.
    #[error("has an empty segment")]
    EmptySegment,
    /// A segment started with something other than an ASCII lowercase letter.
    #[error("segment `{segment}` must start with a lowercase letter")]
    SegmentStart {
        /// The offending segment.
        segment: String,
    },
    /// A segment contained a character outside `[a-z0-9-]`.
    #[error("segment `{segment}` may only contain lowercase letters, digits and `-`")]
    SegmentCharacter {
        /// The offending segment.
        segment: String,
    },
    /// A segment ended with `-`.
    #[error("segment `{segment}` may not end with `-`")]
    SegmentEnd {
        /// The offending segment.
        segment: String,
    },
    /// The whole id exceeded [`AppId::MAX_LEN`](crate::AppId::MAX_LEN).
    #[error("is {len} characters, over the {max} character limit")]
    TooLong {
        /// Actual length.
        len: usize,
        /// Permitted length.
        max: usize,
    },
}

/// Why a string is not a valid [`DisplayName`](crate::DisplayName).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NameError {
    /// Empty, or nothing but whitespace.
    #[error("is empty")]
    Empty,
    /// Longer than [`DisplayName::MAX_LEN`](crate::DisplayName::MAX_LEN).
    #[error("is {len} characters, over the {max} character limit")]
    TooLong {
        /// Actual length, in characters.
        len: usize,
        /// Permitted length, in characters.
        max: usize,
    },
    /// Contains a control character, which no shelf label should.
    #[error("contains a control character")]
    ControlCharacter,
}

/// Why a string is not a safe package-relative path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PathError {
    /// The path was empty.
    #[error("is empty")]
    Empty,
    /// The path was rooted at `/`.
    #[error("is absolute")]
    Absolute,
    /// The path contained a Windows drive prefix or a backslash separator.
    #[error("uses a `\\` separator or a drive prefix")]
    NotPosix,
    /// The path contained `..`, which would escape the package directory.
    #[error("contains `..` and would escape the package")]
    ParentEscape,
    /// The path contained a `.` component or a doubled separator.
    #[error("contains an empty or `.` component")]
    EmptyComponent,
    /// The path began with `~`, which a shell would expand elsewhere.
    #[error("starts with `~`")]
    HomeExpansion,
    /// The path contained a NUL byte.
    #[error("contains a NUL byte")]
    Nul,
    /// The path exceeded [`RelativePath::MAX_LEN`](crate::RelativePath::MAX_LEN).
    #[error("is {len} characters, over the {max} character limit")]
    TooLong {
        /// Actual length.
        len: usize,
        /// Permitted length.
        max: usize,
    },
}

/// Why a package payload directory does not match its manifest.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PayloadError {
    /// The declared entrypoint is not in the payload.
    #[error("entrypoint `{path}` is missing from the package payload")]
    MissingEntrypoint {
        /// The manifest-relative path that was expected.
        path: String,
    },
    /// The declared entrypoint exists but is a directory or similar.
    #[error("entrypoint `{path}` is not a regular file")]
    EntrypointNotAFile {
        /// The manifest-relative path that was expected.
        path: String,
    },
    /// A declared asset is not in the payload.
    #[error("declared asset `{path}` is missing from the package payload")]
    MissingAsset {
        /// The manifest-relative path that was expected.
        path: String,
    },
    /// The payload could not be inspected at all.
    #[error("cannot inspect {path}")]
    Io {
        /// The path that was attempted.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },
}
