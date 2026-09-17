//! Writing a catalog: the publishing half, which only ever runs on the Mac (§12).
//!
//! The whole module is behind the `publishing` feature, because this is the
//! code that holds a [`SecretKey`]. A device build turns the feature off and
//! does not link it.
//!
//! # Immutability
//!
//! A published version is final. [`Publisher::publish`] refuses to replace an
//! existing version with different bytes, and accepts a republish of identical
//! bytes as the no-op it is. That is the rule that makes a version number mean
//! something: `0.2.0` installed on the tablet in March is the same `0.2.0`
//! anyone fetches in June, and "it works on my device" is a statement about
//! specific bytes.
//!
//! Development builds are not an exception to this — they are the reason for
//! it. §12 gives them unique prerelease versions (`0.3.0-dev.4`) and exact
//! digests precisely so that iterating fast does not mean overwriting a
//! version someone already installed.
//!
//! # The index is derived, never edited
//!
//! Every publish rebuilds the index from the release descriptors actually
//! present, verifying each one on the way. An index cannot drift from the
//! catalog it describes because it is never the thing being edited, and a
//! descriptor that no longer verifies stops the publish rather than being
//! quietly dropped from the listing.

use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;

use crate::archive::{self, ArchiveError, ArchiveLimits};
use crate::digest::Digest;
use crate::error::ManifestError;
use crate::id::AppId;
use crate::manifest::Manifest;
use crate::release::{
    self, CatalogIndex, INDEX_FILE_NAME, IndexEntry, MAX_INDEX_BYTES, MAX_RELEASE_BYTES,
    RELEASE_FILE_NAME, Release, ReleaseError, SIGNATURE_SUFFIX, VersionConflict,
};
use crate::signing::{Domain, SecretKey, SignatureError, TrustedKeys};
use crate::store::{self, StoreError};

/// The directory a publish unpacks into while it checks a package.
const STAGING: &str = ".paperclip-publish";

/// What a publish produced.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Published {
    /// Which app.
    pub app: AppId,
    /// Which version.
    pub version: Version,
    /// The archive's digest.
    pub digest: Digest,
    /// The archive's size.
    pub size: u64,
    /// The catalog's serial after this publish.
    pub serial: u64,
    /// Whether these exact bytes were already published, so nothing changed.
    pub already_published: bool,
    /// Where the archive was written, relative to the catalog root.
    pub archive: String,
}

/// A report on everything a catalog contains.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Checked {
    /// The catalog's name.
    pub catalog: String,
    /// Its serial.
    pub serial: u64,
    /// Every release that verified, as `app version`.
    pub verified: Vec<String>,
}

/// A catalog directory, from the publishing side.
#[derive(Debug, Clone)]
pub struct Publisher {
    root: PathBuf,
    name: String,
}

impl Publisher {
    /// Opens or creates a catalog named `name` at `root`.
    pub fn open(root: impl Into<PathBuf>, name: &str) -> Result<Self, PublishError> {
        let root = root.into();
        store::create_dir_if_missing(&root)?;
        Ok(Self {
            root,
            name: name.to_owned(),
        })
    }

    /// The directory being published into.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Adds a package to the catalog and rewrites the index.
    pub fn publish(
        &self,
        package: &Path,
        secret: &SecretKey,
        notes: &str,
        published_at: u64,
    ) -> Result<Published, PublishError> {
        let bytes = fs::read(package).map_err(|source| PublishError::Io {
            path: package.to_path_buf(),
            source,
        })?;
        let digest = Digest::of_bytes(&bytes);
        let size = bytes.len() as u64;
        let manifest = self.inspect(&bytes)?;

        let version = manifest.version().clone();
        let app = manifest.id().clone();
        let directory = format!("apps/{app}/{version}");
        let archive_name = format!("{}-{version}.{}", app.leaf(), archive::ARCHIVE_EXTENSION);
        let target = self.root.join(&directory);

        let release = Release::new(
            &manifest,
            archive_name
                .parse()
                .map_err(|_| PublishError::ArchiveName {
                    name: archive_name.clone(),
                })?,
            size,
            digest,
            published_at,
            notes,
        )?;
        let document = release.to_document();

        let existing = target.join(RELEASE_FILE_NAME);
        let mut already_published = false;
        if existing.exists() {
            let previous = self.read_release(&directory, &secret.public_key_set())?;
            if previous.release().digest() != digest {
                return Err(PublishError::Immutable(Box::new(VersionConflict {
                    app,
                    version,
                    recorded: previous.release().digest(),
                    offered: digest,
                })));
            }
            already_published = true;
        }

        if !already_published {
            store::create_dir_if_missing(&target)?;
            store::atomic_write(&target.join(&archive_name), &bytes)?;
            store::atomic_write(&existing, document.as_bytes())?;
            store::atomic_write(
                &target.join(format!("{RELEASE_FILE_NAME}{SIGNATURE_SUFFIX}")),
                secret
                    .sign(Domain::RELEASE, document.as_bytes())
                    .to_armoured()
                    .as_bytes(),
            )?;
        }

        let serial = self.rewrite_index(secret, published_at)?;
        Ok(Published {
            app,
            version,
            digest,
            size,
            serial,
            already_published,
            archive: format!("{directory}/{archive_name}"),
        })
    }

    /// Verifies everything in the catalog against `keys`.
    ///
    /// The same checks a device makes, run where a mistake is still cheap: the
    /// index verifies, every descriptor it lists verifies, and every archive
    /// hashes to what its descriptor says.
    pub fn check(&self, keys: &TrustedKeys) -> Result<Checked, PublishError> {
        let (bytes, signature) = self.read_index_bytes()?;
        let signature = signature.parse::<crate::signing::Signature>()?;
        let verified = keys.verify(Domain::CATALOG, &bytes, &signature)?;
        let index = release::parse_index(verified)?;

        let mut report = Checked {
            catalog: index.index().catalog().to_owned(),
            serial: index.index().serial(),
            verified: Vec::new(),
        };

        for entry in index.index().entries() {
            let directory = parent_of(entry.descriptor().as_str());
            let release = self.read_release(&directory, keys)?;
            let release = release.release();
            if release.app() != entry.app() || release.version() != entry.version() {
                return Err(PublishError::IndexDisagreement {
                    listed: format!("{} {}", entry.app(), entry.version()),
                    signed: format!("{} {}", release.app(), release.version()),
                });
            }
            let archive = self.root.join(&directory).join(release.archive().as_str());
            let bytes = fs::read(&archive).map_err(|source| PublishError::Io {
                path: archive.clone(),
                source,
            })?;
            if bytes.len() as u64 != release.size() || Digest::of_bytes(&bytes) != release.digest()
            {
                return Err(PublishError::ArchiveMismatch {
                    path: archive,
                    expected: release.digest(),
                    actual: Digest::of_bytes(&bytes),
                });
            }
            report
                .verified
                .push(format!("{} {}", release.app(), release.version()));
        }
        Ok(report)
    }

    /// Unpacks a package into a scratch directory just to read its manifest.
    ///
    /// Through the same [`archive::extract`] a device uses, so a package that
    /// would be refused on the tablet is refused here, where the person who
    /// built it is still looking at the terminal.
    fn inspect(&self, bytes: &[u8]) -> Result<Manifest, PublishError> {
        let staging = self.root.join(STAGING);
        store::remove_tree(&staging)?;
        store::create_dir_if_missing(&staging)?;
        let outcome = archive::extract(bytes, &staging, ArchiveLimits::DEFAULT);
        store::remove_tree(&staging)?;
        Ok(outcome?.manifest)
    }

    /// Rebuilds and signs the index from the descriptors on disk.
    fn rewrite_index(&self, secret: &SecretKey, generated: u64) -> Result<u64, PublishError> {
        let keys = secret.public_key_set();
        let mut entries = Vec::new();
        for directory in self.release_directories()? {
            let release = self.read_release(&directory, &keys)?;
            let release = release.release();
            entries.push(IndexEntry::new(
                release.app().clone(),
                release.name().clone(),
                release.version().clone(),
                format!("{directory}/{RELEASE_FILE_NAME}")
                    .parse()
                    .map_err(|_| PublishError::ArchiveName {
                        name: directory.clone(),
                    })?,
            ));
        }
        entries.sort_by(|a, b| {
            a.app()
                .cmp(b.app())
                .then_with(|| a.version().cmp(b.version()))
        });

        let serial = match self.read_index_bytes() {
            Ok((bytes, signature)) => {
                let signature = signature.parse::<crate::signing::Signature>()?;
                let verified = keys.verify(Domain::CATALOG, &bytes, &signature)?;
                release::parse_index(verified)?.index().serial() + 1
            }
            // No index yet, or one this key did not sign. Either way this is
            // the first serial *this* publisher can vouch for.
            Err(_) => 1,
        };

        let index = CatalogIndex::new(&self.name, serial, generated, entries)?;
        let document = index.to_document();
        store::atomic_write(&self.root.join(INDEX_FILE_NAME), document.as_bytes())?;
        store::atomic_write(
            &self
                .root
                .join(format!("{INDEX_FILE_NAME}{SIGNATURE_SUFFIX}")),
            secret
                .sign(Domain::CATALOG, document.as_bytes())
                .to_armoured()
                .as_bytes(),
        )?;
        Ok(serial)
    }

    /// Every `apps/<id>/<version>` directory holding a descriptor, sorted.
    fn release_directories(&self) -> Result<Vec<String>, PublishError> {
        let apps = self.root.join("apps");
        let mut found = Vec::new();
        let Ok(entries) = fs::read_dir(&apps) else {
            return Ok(found);
        };
        for app in entries.flatten() {
            let Ok(versions) = fs::read_dir(app.path()) else {
                continue;
            };
            for version in versions.flatten() {
                if !version.path().join(RELEASE_FILE_NAME).exists() {
                    continue;
                }
                found.push(format!(
                    "apps/{}/{}",
                    app.file_name().to_string_lossy(),
                    version.file_name().to_string_lossy()
                ));
            }
        }
        found.sort();
        Ok(found)
    }

    fn read_release(
        &self,
        directory: &str,
        keys: &TrustedKeys,
    ) -> Result<release::VerifiedRelease, PublishError> {
        let path = self.root.join(directory).join(RELEASE_FILE_NAME);
        let bytes = read_bounded(&path, MAX_RELEASE_BYTES)?;
        let signature = fs::read_to_string(
            self.root
                .join(directory)
                .join(format!("{RELEASE_FILE_NAME}{SIGNATURE_SUFFIX}")),
        )
        .map_err(|source| PublishError::Io {
            path: path.clone(),
            source,
        })?;
        let signature = signature.parse::<crate::signing::Signature>()?;
        let verified = keys.verify(Domain::RELEASE, &bytes, &signature)?;
        Ok(release::parse_release(verified)?)
    }

    fn read_index_bytes(&self) -> Result<(Vec<u8>, String), PublishError> {
        let path = self.root.join(INDEX_FILE_NAME);
        let bytes = read_bounded(&path, MAX_INDEX_BYTES)?;
        let signature = fs::read_to_string(
            self.root
                .join(format!("{INDEX_FILE_NAME}{SIGNATURE_SUFFIX}")),
        )
        .map_err(|source| PublishError::Io {
            path: path.clone(),
            source,
        })?;
        Ok((bytes, signature))
    }
}

impl SecretKey {
    /// A trust set holding only this key's public half.
    ///
    /// What the publisher checks its own catalog against. A publisher that
    /// trusted whatever key the catalog happened to be signed with could
    /// happily rebuild an index over someone else's releases.
    pub fn public_key_set(&self) -> TrustedKeys {
        let mut keys = TrustedKeys::none();
        keys.trust(self.public_key());
        keys
    }
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, PublishError> {
    let metadata = fs::metadata(path).map_err(|source| PublishError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > max {
        return Err(PublishError::TooLarge {
            path: path.to_path_buf(),
            len: metadata.len(),
            max,
        });
    }
    fs::read(path).map_err(|source| PublishError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn parent_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) => parent.to_owned(),
        None => String::new(),
    }
}

/// Why a publish or a catalog check failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PublishError {
    /// This version is published already, with different bytes.
    #[error(
        "`{}` {} is already published with a different archive ({}, offered {}). \
         A published version is immutable: bump the version, or use a prerelease \
         for a development build.",
        .0.app, .0.version, .0.recorded, .0.offered
    )]
    Immutable(Box<VersionConflict>),

    /// An archive on disk does not match its descriptor.
    #[error("{path} does not match its descriptor ({expected}, got {actual})")]
    ArchiveMismatch {
        /// Which archive.
        path: PathBuf,
        /// What the descriptor says.
        expected: Digest,
        /// What the bytes hash to.
        actual: Digest,
    },

    /// The index and a descriptor describe different things.
    #[error("the index lists `{listed}` but the signed descriptor is for `{signed}`")]
    IndexDisagreement {
        /// What the index said.
        listed: String,
        /// What the descriptor said.
        signed: String,
    },

    /// A generated name that is not a usable relative path.
    #[error("`{name}` is not a usable name inside a catalog")]
    ArchiveName {
        /// The name.
        name: String,
    },

    /// A catalog file larger than its bound.
    #[error("{path} is {len} bytes, over the {max} byte limit")]
    TooLarge {
        /// Which file.
        path: PathBuf,
        /// Its size.
        len: u64,
        /// The limit.
        max: u64,
    },

    /// The package could not be opened.
    #[error("the package could not be read")]
    Archive(#[from] ArchiveError),

    /// The package's manifest is not usable.
    #[error("the package's manifest is not usable")]
    Manifest(#[from] ManifestError),

    /// A signed document could not be built or read.
    #[error("a catalog document could not be read")]
    Document(#[from] ReleaseError),

    /// A signature could not be made or checked.
    #[error("a catalog signature is not acceptable")]
    Signature(#[from] SignatureError),

    /// A durable write failed.
    #[error("the catalog could not be written")]
    Store(#[from] StoreError),

    /// A filesystem operation failed.
    #[error("cannot access {path}")]
    Io {
        /// Which path.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{PublishError, Publisher};
    use crate::archive;
    use crate::signing::SecretKey;

    const MANIFEST: &str = r#"
[app]
id = "dev.calum.chess"
name = "Chess"
version = "VERSION"
protocol = "1.0"
entrypoint = "bin/chess"
"#;

    /// A `.paperpkg` for Chess at `version`, with `body` as its entrypoint.
    fn package(version: &str, body: &[u8]) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("paper.toml"),
            MANIFEST.replace("VERSION", version),
        )
        .unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        fs::write(dir.path().join("bin/chess"), body).unwrap();
        let mut bytes = Vec::new();
        archive::build(dir.path(), &mut bytes).unwrap();
        bytes
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn publishing_writes_a_catalog_a_device_would_accept() {
        let secret = SecretKey::generate().unwrap();
        let catalog = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();

        let path = write(work.path(), "chess.paperpkg", &package("0.1.0", b"one"));
        let published = publisher
            .publish(&path, &secret, "First cut.", 100)
            .unwrap();
        assert_eq!(published.version.to_string(), "0.1.0");
        assert_eq!(published.serial, 1);
        assert!(!published.already_published);

        let report = publisher.check(&secret.public_key_set()).unwrap();
        assert_eq!(report.catalog, "calum-home");
        assert_eq!(report.verified, vec!["dev.calum.chess 0.1.0"]);
        assert!(!catalog.path().join(super::STAGING).exists());
    }

    #[test]
    fn the_serial_advances_with_every_publish() {
        let secret = SecretKey::generate().unwrap();
        let catalog = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();

        for (index, version) in ["0.1.0", "0.2.0", "0.3.0"].iter().enumerate() {
            let path = write(
                work.path(),
                &format!("chess-{version}.paperpkg"),
                &package(version, version.as_bytes()),
            );
            let published = publisher.publish(&path, &secret, "", 100).unwrap();
            assert_eq!(published.serial, index as u64 + 1);
        }
        assert_eq!(
            publisher
                .check(&secret.public_key_set())
                .unwrap()
                .verified
                .len(),
            3
        );
    }

    #[test]
    fn a_version_cannot_be_republished_with_different_bytes() {
        let secret = SecretKey::generate().unwrap();
        let catalog = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();

        let first = write(work.path(), "a.paperpkg", &package("0.1.0", b"one"));
        publisher.publish(&first, &secret, "", 100).unwrap();

        let second = write(work.path(), "b.paperpkg", &package("0.1.0", b"two"));
        assert!(matches!(
            publisher.publish(&second, &secret, "", 200),
            Err(PublishError::Immutable(_))
        ));
    }

    #[test]
    fn republishing_identical_bytes_is_a_no_op() {
        let secret = SecretKey::generate().unwrap();
        let catalog = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();

        let bytes = package("0.1.0", b"one");
        let path = write(work.path(), "a.paperpkg", &bytes);
        publisher.publish(&path, &secret, "", 100).unwrap();
        let again = publisher.publish(&path, &secret, "", 200).unwrap();
        assert!(again.already_published);
        // The serial still moves: the index was rewritten, and a device that
        // has seen serial 1 must not be handed another serial 1.
        assert_eq!(again.serial, 2);
    }

    #[test]
    fn a_development_prerelease_is_published_alongside_the_stable_one() {
        let secret = SecretKey::generate().unwrap();
        let catalog = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();

        for version in ["0.1.0", "0.2.0-dev.1", "0.2.0-dev.2"] {
            let path = write(
                work.path(),
                &format!("{version}.paperpkg"),
                &package(version, version.as_bytes()),
            );
            publisher.publish(&path, &secret, "", 100).unwrap();
        }
        let report = publisher.check(&secret.public_key_set()).unwrap();
        assert_eq!(report.verified.len(), 3);
    }

    #[test]
    fn a_check_catches_an_archive_swapped_under_its_descriptor() {
        let secret = SecretKey::generate().unwrap();
        let catalog = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();

        let path = write(work.path(), "a.paperpkg", &package("0.1.0", b"one"));
        let published = publisher.publish(&path, &secret, "", 100).unwrap();

        fs::write(catalog.path().join(&published.archive), b"not the package").unwrap();
        assert!(matches!(
            publisher.check(&secret.public_key_set()),
            Err(PublishError::ArchiveMismatch { .. })
        ));
    }

    #[test]
    fn a_check_catches_an_edited_descriptor() {
        let secret = SecretKey::generate().unwrap();
        let catalog = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();

        let path = write(work.path(), "a.paperpkg", &package("0.1.0", b"one"));
        publisher.publish(&path, &secret, "", 100).unwrap();

        let descriptor = catalog
            .path()
            .join("apps/dev.calum.chess/0.1.0/release.toml");
        let text = fs::read_to_string(&descriptor).unwrap();
        fs::write(&descriptor, text.replace("size = ", "size  = ")).unwrap();

        assert!(matches!(
            publisher.check(&secret.public_key_set()),
            Err(PublishError::Signature(_))
        ));
    }

    #[test]
    fn another_publishers_catalog_is_not_checkable_with_the_wrong_key() {
        let secret = SecretKey::generate().unwrap();
        let stranger = SecretKey::generate().unwrap();
        let catalog = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();

        let path = write(work.path(), "a.paperpkg", &package("0.1.0", b"one"));
        publisher.publish(&path, &secret, "", 100).unwrap();

        assert!(matches!(
            publisher.check(&stranger.public_key_set()),
            Err(PublishError::Signature(_))
        ));
    }

    #[test]
    fn a_package_with_a_hostile_payload_never_reaches_the_catalog() {
        let secret = SecretKey::generate().unwrap();
        let catalog = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let publisher = Publisher::open(catalog.path(), "calum-home").unwrap();

        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let manifest = MANIFEST.replace("VERSION", "0.1.0");
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(manifest.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "paper.toml", manifest.as_bytes())
            .unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        link.set_link_name("/bin/sh").unwrap();
        link.set_cksum();
        builder
            .append_data(&mut link, "bin/chess", &[][..])
            .unwrap();
        let hostile = builder.into_inner().unwrap().finish().unwrap();

        let path = write(work.path(), "hostile.paperpkg", &hostile);
        assert!(matches!(
            publisher.publish(&path, &secret, "", 100),
            Err(PublishError::Archive(_))
        ));
        assert!(!catalog.path().join("apps").exists());
        assert!(!catalog.path().join(super::STAGING).exists());
    }
}
