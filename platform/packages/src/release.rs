//! Release descriptors and the catalog index: the two signed documents (§12).
//!
//! # Two documents, two signatures
//!
//! A **release descriptor** names one version of one app and the archive that
//! carries it, including that archive's exact size and digest. A **catalog
//! index** says which releases a catalog is currently offering, and carries a
//! serial that only ever goes up.
//!
//! They are signed separately and on purpose. The index changes every time
//! anything is published; a release descriptor is written once and never
//! again. Signing releases through the index would mean every past release's
//! authenticity depended on the freshest index being honest, and it would make
//! "this version was published with these bytes" unanswerable after the fact.
//!
//! # Why the bytes, and not the values
//!
//! Every `verify_*` function here takes a [`Verified`], which can only come
//! from [`TrustedKeys::verify`](crate::signing::TrustedKeys::verify). Parsing
//! happens *after* that and *from those bytes*. There is no code path in this
//! module that parses a document and then goes looking for a signature, which
//! is the ordering mistake that quietly makes a signature scheme decorative.
//!
//! The publisher renders a document once, writes those bytes, and signs those
//! bytes. Verification never re-renders anything: TOML has no canonical form,
//! `toml` is not promised to be byte-stable across versions, and a scheme that
//! depends on two serialisers agreeing is a scheme that breaks on a patch
//! release of a dependency.

#[cfg(feature = "publishing")]
use std::fmt::Write as _;
use std::str::FromStr;

use paper_protocol::ProtocolVersion;
use semver::Version;
use serde::Deserialize;

use crate::digest::Digest;
use crate::id::{AppId, DisplayName};
use crate::path::RelativePath;
use crate::signing::{Domain, KeyId, Verified};

/// Largest release descriptor that will be read, in bytes.
pub const MAX_RELEASE_BYTES: u64 = 16 * 1024;

/// Largest catalog index that will be read, in bytes.
///
/// A personal catalog holds a handful of apps; a megabyte is four orders of
/// magnitude of headroom and still a bound.
pub const MAX_INDEX_BYTES: u64 = 1024 * 1024;

/// Longest release notes a descriptor may carry, in bytes.
pub const MAX_NOTES_BYTES: usize = 4096;

/// The file name a release descriptor is published under.
pub const RELEASE_FILE_NAME: &str = "release.toml";

/// The file name a catalog index is published under.
pub const INDEX_FILE_NAME: &str = "index.toml";

/// The suffix a detached signature is published under.
pub const SIGNATURE_SUFFIX: &str = ".sig";

/// One published version of one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    app: AppId,
    name: DisplayName,
    version: Version,
    protocol: ProtocolVersion,
    archive: RelativePath,
    size: u64,
    digest: Digest,
    published: u64,
    notes: String,
}

impl Release {
    /// Describes a release. Publisher side.
    ///
    /// `archive` is relative to the release descriptor's own directory in the
    /// catalog, so a catalog can be copied or re-hosted without rewriting
    /// anything it contains.
    #[cfg(feature = "publishing")]
    pub fn new(
        manifest: &crate::manifest::Manifest,
        archive: RelativePath,
        size: u64,
        digest: Digest,
        published: u64,
        notes: &str,
    ) -> Result<Self, ReleaseError> {
        if notes.len() > MAX_NOTES_BYTES {
            return Err(ReleaseError::NotesTooLong {
                len: notes.len(),
                max: MAX_NOTES_BYTES,
            });
        }
        Ok(Self {
            app: manifest.id().clone(),
            name: manifest.name().clone(),
            version: manifest.version().clone(),
            protocol: manifest.protocol(),
            archive,
            size,
            digest,
            published,
            notes: notes.to_owned(),
        })
    }

    /// The app this release belongs to.
    pub fn app(&self) -> &AppId {
        &self.app
    }

    /// The app's display name at this version.
    pub fn name(&self) -> &DisplayName {
        &self.name
    }

    /// The version.
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The platform protocol this build was made against.
    pub fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Where the archive sits, relative to this descriptor.
    pub fn archive(&self) -> &RelativePath {
        &self.archive
    }

    /// The archive's exact size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The archive's digest.
    ///
    /// Authenticated only because this whole descriptor is: a digest that
    /// arrived beside the bytes it describes proves nothing on its own.
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// When it was published, as seconds since the Unix epoch.
    pub fn published(&self) -> u64 {
        self.published
    }

    /// The release notes, for the App Store to show.
    pub fn notes(&self) -> &str {
        &self.notes
    }

    /// Renders the document a publisher writes and signs.
    ///
    /// Written by hand rather than through a serialiser: these are the bytes a
    /// signature will commit to for the life of the release, and they should
    /// be produced by code a reader can check by eye rather than by whichever
    /// TOML writer is linked this year.
    #[cfg(feature = "publishing")]
    pub fn to_document(&self) -> String {
        let mut out = String::new();
        out.push_str("# A Paperclip release descriptor. Signed exactly as written;\n");
        out.push_str("# editing any byte of this file invalidates its signature.\n");
        out.push_str("[release]\n");
        let _ = writeln!(out, "app = {}", quote(self.app.as_str()));
        let _ = writeln!(out, "name = {}", quote(self.name.as_str()));
        let _ = writeln!(out, "version = {}", quote(&self.version.to_string()));
        let _ = writeln!(out, "protocol = {}", quote(&self.protocol.to_string()));
        let _ = writeln!(out, "archive = {}", quote(self.archive.as_str()));
        let _ = writeln!(out, "size = {}", self.size);
        let _ = writeln!(out, "digest = {}", quote(&self.digest.to_string()));
        let _ = writeln!(out, "published = {}", self.published);
        let _ = writeln!(out, "notes = {}", quote(&self.notes));
        out
    }
}

/// Two sets of bytes claiming to be the same version.
///
/// Boxed wherever it appears in an error, because a version plus two digests
/// is large enough to make every `Result` in the install path pay for a case
/// that should never happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionConflict {
    /// Which app.
    pub app: AppId,
    /// Which version both sets of bytes claim.
    pub version: Version,
    /// The digest already recorded.
    pub recorded: Digest,
    /// The digest offered now.
    pub offered: Digest,
}

/// A release descriptor whose signature has already been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRelease {
    release: Release,
    signer: KeyId,
}

impl VerifiedRelease {
    /// The release.
    pub fn release(&self) -> &Release {
        &self.release
    }

    /// Which trusted key signed it.
    pub fn signer(&self) -> KeyId {
        self.signer
    }
}

/// Parses a release descriptor out of bytes a trusted key signed.
///
/// Takes a [`Verified`] and not a `&[u8]`, so "was this signed?" cannot be
/// skipped, forgotten, or answered later.
pub fn parse_release(verified: Verified<'_>) -> Result<VerifiedRelease, ReleaseError> {
    if verified.domain() != Domain::RELEASE {
        return Err(ReleaseError::WrongDomain {
            domain: verified.domain().label(),
        });
    }
    let text = document_text(verified.bytes(), MAX_RELEASE_BYTES)?;
    let raw: RawDocument = toml::from_str(&text).map_err(ReleaseError::Schema)?;
    let raw = raw.release;

    if raw.notes.len() > MAX_NOTES_BYTES {
        return Err(ReleaseError::NotesTooLong {
            len: raw.notes.len(),
            max: MAX_NOTES_BYTES,
        });
    }

    Ok(VerifiedRelease {
        release: Release {
            app: field("app", &raw.app)?,
            name: field("name", &raw.name)?,
            version: Version::parse(&raw.version).map_err(|_| ReleaseError::Field {
                field: "version",
                value: raw.version.clone(),
            })?,
            protocol: field("protocol", &raw.protocol)?,
            archive: field("archive", &raw.archive)?,
            size: raw.size,
            digest: field("digest", &raw.digest)?,
            published: raw.published,
            notes: raw.notes,
        },
        signer: verified.signer(),
    })
}

/// What a catalog is offering, and how current it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogIndex {
    catalog: String,
    serial: u64,
    generated: u64,
    entries: Vec<IndexEntry>,
}

impl CatalogIndex {
    /// Longest catalog name, in bytes.
    pub const MAX_NAME_LEN: usize = 64;

    /// Most entries an index may list.
    pub const MAX_ENTRIES: usize = 1024;

    /// Builds an index. Publisher side.
    #[cfg(feature = "publishing")]
    pub fn new(
        catalog: &str,
        serial: u64,
        generated: u64,
        entries: Vec<IndexEntry>,
    ) -> Result<Self, ReleaseError> {
        let index = Self {
            catalog: catalog.to_owned(),
            serial,
            generated,
            entries,
        };
        index.check()?;
        Ok(index)
    }

    /// Which catalog this is. Serials are only comparable within one name.
    pub fn catalog(&self) -> &str {
        &self.catalog
    }

    /// The monotonic serial.
    ///
    /// The only defence against being handed a truthfully signed but stale
    /// index — every signature a publisher ever made stays valid forever, so
    /// "is this signed?" cannot answer "is this current?".
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// When it was generated, as seconds since the Unix epoch.
    pub fn generated(&self) -> u64 {
        self.generated
    }

    /// Everything on offer.
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// Every entry for one app, newest version first.
    pub fn versions_of<'a>(&'a self, app: &AppId) -> Vec<&'a IndexEntry> {
        let mut matching: Vec<&IndexEntry> =
            self.entries.iter().filter(|e| &e.app == app).collect();
        matching.sort_by(|a, b| b.version.cmp(&a.version));
        matching
    }

    /// The newest *stable* version of an app the catalog offers.
    ///
    /// Prereleases are excluded, and that is the whole reason this is not just
    /// "the highest version". §12 gives development builds unique prerelease
    /// versions, and SemVer orders `0.3.0-dev.1` above `0.2.0` — so an
    /// "install the newest" path that did not filter would quietly move a
    /// device onto a development build the moment one was published.
    pub fn newest_stable(&self, app: &AppId) -> Option<&IndexEntry> {
        self.versions_of(app)
            .into_iter()
            .find(|entry| entry.version.pre.is_empty())
    }

    /// The newest version of an app, prereleases included.
    ///
    /// For showing a developer what exists. Installing one is an explicit act:
    /// name the version.
    pub fn newest_any(&self, app: &AppId) -> Option<&IndexEntry> {
        self.versions_of(app).into_iter().next()
    }

    /// The entry for one exact version, if the catalog offers it.
    pub fn exact(&self, app: &AppId, version: &Version) -> Option<&IndexEntry> {
        self.entries
            .iter()
            .find(|entry| &entry.app == app && &entry.version == version)
    }

    /// Renders the document a publisher writes and signs.
    #[cfg(feature = "publishing")]
    pub fn to_document(&self) -> String {
        let mut out = String::new();
        out.push_str("# A Paperclip catalog index. Signed exactly as written;\n");
        out.push_str("# editing any byte of this file invalidates its signature.\n");
        out.push_str("[catalog]\n");
        let _ = writeln!(out, "name = {}", quote(&self.catalog));
        let _ = writeln!(out, "serial = {}", self.serial);
        let _ = writeln!(out, "generated = {}", self.generated);
        for entry in &self.entries {
            out.push_str("\n[[release]]\n");
            let _ = writeln!(out, "app = {}", quote(entry.app.as_str()));
            let _ = writeln!(out, "name = {}", quote(entry.name.as_str()));
            let _ = writeln!(out, "version = {}", quote(&entry.version.to_string()));
            let _ = writeln!(out, "descriptor = {}", quote(entry.descriptor.as_str()));
        }
        out
    }

    fn check(&self) -> Result<(), ReleaseError> {
        if self.catalog.is_empty() || self.catalog.len() > Self::MAX_NAME_LEN {
            return Err(ReleaseError::CatalogName {
                len: self.catalog.len(),
                max: Self::MAX_NAME_LEN,
            });
        }
        if self.entries.len() > Self::MAX_ENTRIES {
            return Err(ReleaseError::TooManyEntries {
                len: self.entries.len(),
                max: Self::MAX_ENTRIES,
            });
        }
        Ok(())
    }
}

/// One line of a catalog index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    app: AppId,
    name: DisplayName,
    version: Version,
    descriptor: RelativePath,
}

impl IndexEntry {
    /// Describes one offered release. Publisher side.
    #[cfg(feature = "publishing")]
    pub fn new(app: AppId, name: DisplayName, version: Version, descriptor: RelativePath) -> Self {
        Self {
            app,
            name,
            version,
            descriptor,
        }
    }

    /// Which app.
    pub fn app(&self) -> &AppId {
        &self.app
    }

    /// Its display name, so the App Store can list an app without fetching
    /// every descriptor first.
    pub fn name(&self) -> &DisplayName {
        &self.name
    }

    /// Which version.
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// Where the descriptor sits, relative to the index.
    pub fn descriptor(&self) -> &RelativePath {
        &self.descriptor
    }
}

/// A catalog index whose signature has already been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIndex {
    index: CatalogIndex,
    signer: KeyId,
}

impl VerifiedIndex {
    /// The index.
    pub fn index(&self) -> &CatalogIndex {
        &self.index
    }

    /// Which trusted key signed it.
    pub fn signer(&self) -> KeyId {
        self.signer
    }
}

/// Parses a catalog index out of bytes a trusted key signed.
pub fn parse_index(verified: Verified<'_>) -> Result<VerifiedIndex, ReleaseError> {
    if verified.domain() != Domain::CATALOG {
        return Err(ReleaseError::WrongDomain {
            domain: verified.domain().label(),
        });
    }
    let text = document_text(verified.bytes(), MAX_INDEX_BYTES)?;
    let raw: RawIndexDocument = toml::from_str(&text).map_err(ReleaseError::Schema)?;

    let mut entries = Vec::with_capacity(raw.release.len());
    for entry in raw.release {
        entries.push(IndexEntry {
            app: field("app", &entry.app)?,
            name: field("name", &entry.name)?,
            version: Version::parse(&entry.version).map_err(|_| ReleaseError::Field {
                field: "version",
                value: entry.version.clone(),
            })?,
            descriptor: field("descriptor", &entry.descriptor)?,
        });
    }

    let index = CatalogIndex {
        catalog: raw.catalog.name,
        serial: raw.catalog.serial,
        generated: raw.catalog.generated,
        entries,
    };
    index.check()?;

    Ok(VerifiedIndex {
        index,
        signer: verified.signer(),
    })
}

/// Bounds and decodes a signed document before any parser sees it.
fn document_text(bytes: &[u8], max: u64) -> Result<String, ReleaseError> {
    if bytes.len() as u64 > max {
        return Err(ReleaseError::TooLarge {
            len: bytes.len() as u64,
            max,
        });
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| ReleaseError::NotUtf8)
}

/// Parses one string field into its checked type.
fn field<T: FromStr>(name: &'static str, value: &str) -> Result<T, ReleaseError> {
    value.parse().map_err(|_| ReleaseError::Field {
        field: name,
        value: value.to_owned(),
    })
}

/// A TOML basic string, escaped.
#[cfg(feature = "publishing")]
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Why a signed document could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReleaseError {
    /// The document was signed as a different kind of document.
    #[error("this was signed as a {domain} document, not the one being read")]
    WrongDomain {
        /// What it was signed as.
        domain: &'static str,
    },

    /// Larger than the bound for its kind.
    #[error("the document is {len} bytes, over the {max} byte limit")]
    TooLarge {
        /// Its size.
        len: u64,
        /// The limit.
        max: u64,
    },

    /// Signed bytes that are not UTF-8.
    #[error("the document is not UTF-8")]
    NotUtf8,

    /// Valid UTF-8, wrong shape.
    #[error("the document does not match the schema")]
    Schema(#[source] toml::de::Error),

    /// A field that is present but not usable.
    #[error("`{field}` is not valid: `{value}`")]
    Field {
        /// Which field.
        field: &'static str,
        /// What it said.
        value: String,
    },

    /// Release notes over the bound.
    #[error("the release notes are {len} bytes, over the {max} byte limit")]
    NotesTooLong {
        /// Their size.
        len: usize,
        /// The limit.
        max: usize,
    },

    /// A catalog name that is empty or too long.
    #[error("a catalog name must be 1 to {max} bytes, not {len}")]
    CatalogName {
        /// Its length.
        len: usize,
        /// The limit.
        max: usize,
    },

    /// More entries than an index may list.
    #[error("the index lists {len} releases, over the {max} limit")]
    TooManyEntries {
        /// How many.
        len: usize,
        /// The limit.
        max: usize,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDocument {
    release: RawRelease,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRelease {
    app: String,
    name: String,
    version: String,
    protocol: String,
    archive: String,
    size: u64,
    digest: String,
    published: u64,
    #[serde(default)]
    notes: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIndexDocument {
    catalog: RawCatalog,
    #[serde(default)]
    release: Vec<RawIndexEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalog {
    name: String,
    serial: u64,
    generated: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIndexEntry {
    app: String,
    name: String,
    version: String,
    descriptor: String,
}

#[cfg(all(test, feature = "publishing"))]
mod tests {
    use super::{CatalogIndex, IndexEntry, Release, ReleaseError, parse_index, parse_release};
    use crate::digest::Digest;
    use crate::manifest::Manifest;
    use crate::signing::{Domain, SecretKey, TrustedKeys};

    fn manifest() -> Manifest {
        Manifest::parse(
            r#"
            [app]
            id = "dev.calum.chess"
            name = "Chess"
            version = "0.2.0"
            protocol = "1.0"
            entrypoint = "bin/chess"
            "#,
        )
        .unwrap()
    }

    fn release() -> Release {
        Release::new(
            &manifest(),
            "chess-0.2.0.paperpkg".parse().unwrap(),
            4096,
            Digest::of_bytes(b"archive"),
            1_758_000_000,
            "Castling works now.",
        )
        .unwrap()
    }

    fn keys() -> (SecretKey, TrustedKeys) {
        let secret = SecretKey::generate().unwrap();
        let mut trusted = TrustedKeys::none();
        trusted.trust(secret.public_key());
        (secret, trusted)
    }

    #[test]
    fn a_release_survives_the_round_trip_it_is_signed_over() {
        let (secret, trusted) = keys();
        let release = release();
        let document = release.to_document();
        let signature = secret.sign(Domain::RELEASE, document.as_bytes());

        let verified = trusted
            .verify(Domain::RELEASE, document.as_bytes(), &signature)
            .unwrap();
        let parsed = parse_release(verified).unwrap();

        assert_eq!(parsed.release(), &release);
        assert_eq!(parsed.signer(), secret.public_key().id());
    }

    #[test]
    fn a_release_cannot_be_read_from_a_catalog_signature() {
        let (secret, trusted) = keys();
        let document = release().to_document();
        let signature = secret.sign(Domain::CATALOG, document.as_bytes());
        let verified = trusted
            .verify(Domain::CATALOG, document.as_bytes(), &signature)
            .unwrap();
        assert!(matches!(
            parse_release(verified),
            Err(ReleaseError::WrongDomain { .. })
        ));
    }

    #[test]
    fn notes_with_quotes_and_newlines_survive_rendering() {
        let (secret, trusted) = keys();
        let hostile = "He said \"check\"\nthen \\ escaped\ttab\r\nand \u{1}control";
        let release = Release::new(
            &manifest(),
            "a.paperpkg".parse().unwrap(),
            1,
            Digest::of_bytes(b""),
            0,
            hostile,
        )
        .unwrap();
        let document = release.to_document();
        let signature = secret.sign(Domain::RELEASE, document.as_bytes());
        let verified = trusted
            .verify(Domain::RELEASE, document.as_bytes(), &signature)
            .unwrap();
        assert_eq!(parse_release(verified).unwrap().release().notes(), hostile);
    }

    #[test]
    fn a_descriptor_cannot_smuggle_an_extra_key() {
        let (secret, trusted) = keys();
        let document = format!("{}trusted = true\n", release().to_document());
        let signature = secret.sign(Domain::RELEASE, document.as_bytes());
        let verified = trusted
            .verify(Domain::RELEASE, document.as_bytes(), &signature)
            .unwrap();
        assert!(matches!(
            parse_release(verified),
            Err(ReleaseError::Schema(_))
        ));
    }

    #[test]
    fn a_descriptor_with_an_escaping_archive_path_is_refused() {
        let (secret, trusted) = keys();
        let document = release()
            .to_document()
            .replace("chess-0.2.0.paperpkg", "../../../etc/passwd");
        let signature = secret.sign(Domain::RELEASE, document.as_bytes());
        let verified = trusted
            .verify(Domain::RELEASE, document.as_bytes(), &signature)
            .unwrap();
        assert!(matches!(
            parse_release(verified),
            Err(ReleaseError::Field {
                field: "archive",
                ..
            })
        ));
    }

    #[test]
    fn an_index_round_trips_and_orders_versions() {
        let (secret, trusted) = keys();
        let entry = |version: &str| {
            IndexEntry::new(
                "dev.calum.chess".parse().unwrap(),
                "Chess".parse().unwrap(),
                version.parse().unwrap(),
                format!("apps/dev.calum.chess/{version}/release.toml")
                    .parse()
                    .unwrap(),
            )
        };
        let index = CatalogIndex::new(
            "calum-home",
            7,
            1_758_000_000,
            vec![entry("0.1.0"), entry("0.3.0-dev.1"), entry("0.2.0")],
        )
        .unwrap();

        let document = index.to_document();
        let signature = secret.sign(Domain::CATALOG, document.as_bytes());
        let verified = trusted
            .verify(Domain::CATALOG, document.as_bytes(), &signature)
            .unwrap();
        let parsed = parse_index(verified).unwrap();

        assert_eq!(parsed.index(), &index);
        assert_eq!(parsed.index().serial(), 7);
        let app = "dev.calum.chess".parse().unwrap();
        // SemVer orders 0.3.0-dev.1 *above* 0.2.0, which is exactly why
        // `newest_stable` exists and `newest_any` is a separate question.
        assert_eq!(
            parsed
                .index()
                .newest_any(&app)
                .unwrap()
                .version()
                .to_string(),
            "0.3.0-dev.1"
        );
        assert_eq!(
            parsed
                .index()
                .newest_stable(&app)
                .unwrap()
                .version()
                .to_string(),
            "0.2.0"
        );
        assert!(
            parsed
                .index()
                .exact(&app, &"0.1.0".parse().unwrap())
                .is_some()
        );
        assert!(
            parsed
                .index()
                .exact(&app, &"9.9.9".parse().unwrap())
                .is_none()
        );
        assert_eq!(parsed.index().versions_of(&app).len(), 3);
    }

    #[test]
    fn an_empty_index_is_a_valid_index() {
        let (secret, trusted) = keys();
        let index = CatalogIndex::new("calum-home", 1, 0, Vec::new()).unwrap();
        let document = index.to_document();
        let signature = secret.sign(Domain::CATALOG, document.as_bytes());
        let verified = trusted
            .verify(Domain::CATALOG, document.as_bytes(), &signature)
            .unwrap();
        assert!(parse_index(verified).unwrap().index().entries().is_empty());
    }

    #[test]
    fn notes_are_bounded() {
        let long = "n".repeat(super::MAX_NOTES_BYTES + 1);
        assert!(matches!(
            Release::new(
                &manifest(),
                "a.paperpkg".parse().unwrap(),
                1,
                Digest::of_bytes(b""),
                0,
                &long,
            ),
            Err(ReleaseError::NotesTooLong { .. })
        ));
    }
}
