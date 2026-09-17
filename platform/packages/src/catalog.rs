//! Reading a catalog: fetch, verify, and refuse to go backwards (§12).
//!
//! A catalog is static files. An index, a release descriptor per published
//! version, an archive per release, and a detached signature beside each of
//! the two documents. No database, no device-control server, nothing that
//! executes. Serving one is `python3 -m http.server` over a directory, and
//! that is the entire operational story.
//!
//! # Transport is not trust
//!
//! [`Transport`] fetches bytes and has no opinion about them. Everything that
//! decides whether to believe those bytes happens here, against
//! [`TrustedKeys`], and it happens the same way whatever carried them. That is
//! why signatures are required regardless of transport (§12): over plain HTTP
//! on a home LAN the signature is the only thing standing between the device
//! and whoever else is on the network, and over HTTPS it is still the only
//! thing that says *the publisher* produced this rather than the server.
//!
//! When a transport does use HTTPS it must verify certificates. A transport
//! that skips verification is worse than plain HTTP, because it looks safe.
//!
//! # Signed is not current
//!
//! Every signature a publisher ever made stays valid forever, so "this index
//! verifies" cannot answer "this index is up to date". An attacker who can
//! serve bytes can serve last month's index, correctly signed, and hide the
//! release that fixed something.
//!
//! [`CatalogIndex::serial`] is the answer: it only goes up, the device
//! remembers the highest it has accepted, and an index that goes backwards is
//! [`CatalogError::Rollback`] rather than a quietly older view. The same
//! memory is what makes offline work — the last accepted index is cached, and
//! a catalog that cannot be reached falls back to it and says so, rather than
//! reporting that the device has no apps.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::path::RelativePath;
use crate::release::{
    self, CatalogIndex, INDEX_FILE_NAME, IndexEntry, MAX_INDEX_BYTES, MAX_RELEASE_BYTES,
    ReleaseError, SIGNATURE_SUFFIX, VerifiedRelease,
};
use crate::signing::{Domain, KeyId, Signature, SignatureError, TrustedKeys};
use crate::store::{self, Layout, StoreError};

/// Largest detached signature file that will be read.
const MAX_SIGNATURE_BYTES: u64 = 512;

/// Somewhere catalog bytes come from.
///
/// Deliberately tiny: a path in, bytes out. A transport that needed to know
/// what it was fetching would be a transport with a say in whether to trust
/// it.
pub trait Transport: fmt::Debug {
    /// Fetches a whole file, refusing to produce more than `limit` bytes.
    fn fetch(&self, path: &str, limit: u64) -> Result<Vec<u8>, TransportError>;

    /// Opens a file as a stream, for archives too large to hold in memory.
    fn open(&self, path: &str, limit: u64) -> Result<Box<dyn Read + '_>, TransportError>;
}

/// A catalog in a directory — a local build, or a share mounted on the device.
///
/// Symlinks are refused at every component, for the same reason they are
/// refused inside a package: a catalog directory is not necessarily written by
/// someone trusted, and a link is how a path that looks contained is not.
#[derive(Debug, Clone)]
pub struct FileTransport {
    root: PathBuf,
}

impl FileTransport {
    /// A transport over `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory it serves.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, TransportError> {
        let relative: RelativePath = path.parse().map_err(|_| TransportError::UnsafePath {
            path: path.to_owned(),
        })?;
        let mut walked = self.root.clone();
        for component in relative.components() {
            walked.push(component);
            match std::fs::symlink_metadata(&walked) {
                Ok(metadata) if metadata.is_symlink() => {
                    return Err(TransportError::UnsafePath {
                        path: path.to_owned(),
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(source) => {
                    return Err(TransportError::Unavailable {
                        path: path.to_owned(),
                        source,
                    });
                }
            }
        }
        Ok(relative.resolve_within(&self.root))
    }
}

impl Transport for FileTransport {
    fn fetch(&self, path: &str, limit: u64) -> Result<Vec<u8>, TransportError> {
        let mut reader = self.open(path, limit)?;
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|source| TransportError::Unavailable {
                path: path.to_owned(),
                source,
            })?;
        Ok(bytes)
    }

    fn open(&self, path: &str, limit: u64) -> Result<Box<dyn Read + '_>, TransportError> {
        let resolved = self.resolve(path)?;
        let file =
            std::fs::File::open(&resolved).map_err(|source| TransportError::Unavailable {
                path: path.to_owned(),
                source,
            })?;
        // `limit + 1` so that a file of exactly `limit` bytes reads whole and
        // one byte more is still visible as too long rather than truncated.
        Ok(Box::new(file.take(limit.saturating_add(1))))
    }
}

/// What a catalog is offering, and how fresh the answer is.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CatalogView {
    /// The index.
    pub index: CatalogIndex,
    /// The trusted key that signed it.
    pub signer: KeyId,
    /// Whether this came from the cache because the catalog was unreachable.
    ///
    /// Stale is shown, never hidden: an App Store that silently displays a
    /// month-old list is worse than one that says it could not reach the
    /// catalog (§12).
    pub stale: bool,
}

/// A catalog, read through a transport and checked against trusted keys.
#[derive(Debug)]
pub struct Catalog<T> {
    transport: T,
    keys: TrustedKeys,
    cache: Option<Layout>,
}

impl<T: Transport> Catalog<T> {
    /// A catalog with no memory: every read must reach the transport.
    pub fn new(transport: T, keys: TrustedKeys) -> Self {
        Self {
            transport,
            keys,
            cache: None,
        }
    }

    /// Gives the catalog somewhere to remember the last index it accepted.
    ///
    /// Without this there is no rollback protection and no offline view — both
    /// need a memory of what was already seen.
    pub fn with_cache(mut self, layout: Layout) -> Self {
        self.cache = Some(layout);
        self
    }

    /// The transport underneath.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Fetches, verifies and accepts the index.
    pub fn view(&self) -> Result<CatalogView, CatalogError> {
        let fetched = self
            .transport
            .fetch(INDEX_FILE_NAME, MAX_INDEX_BYTES)
            .and_then(|bytes| {
                let signature = self.transport.fetch(
                    &format!("{INDEX_FILE_NAME}{SIGNATURE_SUFFIX}"),
                    MAX_SIGNATURE_BYTES,
                )?;
                Ok((bytes, signature))
            });

        let (bytes, signature) = match fetched {
            Ok(pair) => pair,
            Err(unavailable) => return self.cached_view(unavailable),
        };

        let verified = self.verify_index(&bytes, &signature)?;
        self.accept(&verified, &bytes, &signature)?;
        Ok(CatalogView {
            signer: verified.signer(),
            index: verified.index().clone(),
            stale: false,
        })
    }

    /// Fetches and verifies one release descriptor.
    pub fn release(&self, entry: &IndexEntry) -> Result<VerifiedRelease, CatalogError> {
        let path = entry.descriptor().as_str();
        let bytes = self.transport.fetch(path, MAX_RELEASE_BYTES)?;
        let signature = self
            .transport
            .fetch(&format!("{path}{SIGNATURE_SUFFIX}"), MAX_SIGNATURE_BYTES)?;
        let signature: Signature = text(&signature)?.parse()?;
        let verified = self.keys.verify(Domain::RELEASE, &bytes, &signature)?;
        let release = release::parse_release(verified)?;

        // The index said which app and version this descriptor was for. If the
        // descriptor disagrees, one of them is lying and neither is usable.
        if release.release().app() != entry.app() || release.release().version() != entry.version()
        {
            return Err(CatalogError::IndexDisagreement {
                listed: format!("{} {}", entry.app(), entry.version()),
                signed: format!(
                    "{} {}",
                    release.release().app(),
                    release.release().version()
                ),
            });
        }
        Ok(release)
    }

    /// Opens the archive a release names, as a stream.
    ///
    /// The bound is the size the signed descriptor declares, so a server that
    /// tries to send more is stopped at the door rather than after the disk is
    /// full. The digest is checked by the installer, which is the code that
    /// writes the bytes down.
    pub fn open_archive(
        &self,
        entry: &IndexEntry,
        release: &VerifiedRelease,
    ) -> Result<Box<dyn Read + '_>, CatalogError> {
        let path = sibling(entry.descriptor(), release.release().archive());
        Ok(self.transport.open(&path, release.release().size())?)
    }

    /// Verifies index bytes, without accepting them.
    fn verify_index(
        &self,
        bytes: &[u8],
        signature: &[u8],
    ) -> Result<release::VerifiedIndex, CatalogError> {
        let signature: Signature = text(signature)?.parse()?;
        let verified = self.keys.verify(Domain::CATALOG, bytes, &signature)?;
        Ok(release::parse_index(verified)?)
    }

    /// Refuses an index older than the newest one already seen, then caches it.
    fn accept(
        &self,
        verified: &release::VerifiedIndex,
        bytes: &[u8],
        signature: &[u8],
    ) -> Result<(), CatalogError> {
        let Some(layout) = &self.cache else {
            return Ok(());
        };
        let index = verified.index();

        if let Some(previous) = self.read_cache(layout, index.catalog())? {
            let cached_bytes = previous.document;
            let cached = self.verify_index(&cached_bytes, &previous.signature)?;
            let cached = cached.index();
            if index.serial() < cached.serial() {
                return Err(CatalogError::Rollback {
                    catalog: index.catalog().to_owned(),
                    offered: index.serial(),
                    accepted: cached.serial(),
                });
            }
            // A serial that stands still while the content moves is either a
            // publisher that forgot to bump it or someone splicing entries in.
            // Both are the same refusal: nothing about this index can be
            // ordered against what is already trusted.
            if index.serial() == cached.serial() && bytes != cached_bytes.as_slice() {
                return Err(CatalogError::StalledSerial {
                    catalog: index.catalog().to_owned(),
                    serial: index.serial(),
                });
            }
        }

        let directory = self.cache_dir(layout);
        store::create_dir_if_missing(&directory)?;
        store::atomic_write(&directory.join(cache_name(index.catalog())), bytes)?;
        store::atomic_write(
            &directory.join(format!("{}{SIGNATURE_SUFFIX}", cache_name(index.catalog()))),
            signature,
        )?;
        Ok(())
    }

    /// The last accepted index, when the catalog itself cannot be reached.
    fn cached_view(&self, unavailable: TransportError) -> Result<CatalogView, CatalogError> {
        let Some(layout) = &self.cache else {
            return Err(CatalogError::Transport(unavailable));
        };
        let directory = self.cache_dir(layout);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => return Err(CatalogError::Transport(unavailable)),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let (Ok(bytes), Ok(signature)) = (
                std::fs::read(&path),
                std::fs::read(path.with_extension("toml.sig")),
            ) else {
                continue;
            };
            let verified = self.verify_index(&bytes, &signature)?;
            return Ok(CatalogView {
                signer: verified.signer(),
                index: verified.index().clone(),
                stale: true,
            });
        }
        Err(CatalogError::Transport(unavailable))
    }

    fn read_cache(
        &self,
        layout: &Layout,
        catalog: &str,
    ) -> Result<Option<CachedIndex>, CatalogError> {
        let directory = self.cache_dir(layout);
        let document = directory.join(cache_name(catalog));
        let signature = directory.join(format!("{}{SIGNATURE_SUFFIX}", cache_name(catalog)));
        match (std::fs::read(&document), std::fs::read(&signature)) {
            (Ok(document), Ok(signature)) => Ok(Some(CachedIndex {
                document,
                signature,
            })),
            _ => Ok(None),
        }
    }

    fn cache_dir(&self, layout: &Layout) -> PathBuf {
        layout.state_dir().join("catalog")
    }
}

/// The last index a device accepted, as it was served.
#[derive(Debug, Clone)]
struct CachedIndex {
    document: Vec<u8>,
    signature: Vec<u8>,
}

/// The cache file name for a catalog.
///
/// A catalog names itself, and a name is not a path: anything that is not a
/// plain identifier character becomes `_`, so a catalog called `../../etc`
/// writes a file called `______etc.toml` inside the cache directory.
fn cache_name(catalog: &str) -> String {
    let mut name: String = catalog
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    name.push_str(".toml");
    name
}

/// A path beside `descriptor`, named by `file`.
fn sibling(descriptor: &RelativePath, file: &RelativePath) -> String {
    let mut parts: Vec<&str> = descriptor.components().collect();
    parts.pop();
    parts.push(file.as_str());
    parts.join("/")
}

fn text(bytes: &[u8]) -> Result<String, CatalogError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| CatalogError::MalformedSignatureFile)
}

/// Why bytes could not be fetched.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// The catalog could not be reached, or the file is not there.
    #[error("cannot reach `{path}` in the catalog")]
    Unavailable {
        /// What was asked for.
        path: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A path that could leave the catalog.
    #[error("`{path}` is not a safe path inside a catalog")]
    UnsafePath {
        /// What was asked for.
        path: String,
    },
}

/// Why a catalog could not be read or believed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// The bytes never arrived, and there was nothing cached.
    #[error("the catalog is not available and nothing is cached")]
    Transport(#[from] TransportError),

    /// A signature file that is not one.
    #[error("the detached signature file is not readable")]
    MalformedSignatureFile,

    /// A signature that does not verify against a trusted key.
    #[error("the catalog's signature is not acceptable")]
    Signature(#[from] SignatureError),

    /// A signed document that is not usable.
    #[error("the catalog's contents are not readable")]
    Document(#[from] ReleaseError),

    /// The index and a release descriptor describe different things.
    #[error("the index lists `{listed}` but the signed descriptor is for `{signed}`")]
    IndexDisagreement {
        /// What the index said.
        listed: String,
        /// What the descriptor said.
        signed: String,
    },

    /// An index older than one already accepted.
    #[error(
        "catalog `{catalog}` offered serial {offered}, older than the {accepted} \
         already accepted. Refusing to move backwards."
    )]
    Rollback {
        /// Which catalog.
        catalog: String,
        /// What it offered.
        offered: u64,
        /// What is already trusted.
        accepted: u64,
    },

    /// Different content under a serial already seen.
    #[error("catalog `{catalog}` changed its contents without advancing serial {serial}")]
    StalledSerial {
        /// Which catalog.
        catalog: String,
        /// The serial that did not move.
        serial: u64,
    },

    /// The cache could not be read or written.
    #[error("the catalog cache could not be updated")]
    Store(#[from] StoreError),
}

#[cfg(all(test, feature = "publishing"))]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{Catalog, CatalogError, FileTransport, Transport, cache_name};
    use crate::digest::Digest;
    use crate::manifest::Manifest;
    use crate::release::{CatalogIndex, IndexEntry, Release, SIGNATURE_SUFFIX};
    use crate::signing::{Domain, SecretKey, TrustedKeys};
    use crate::store::Layout;

    fn manifest(version: &str) -> Manifest {
        Manifest::parse(&format!(
            r#"
            [app]
            id = "dev.calum.chess"
            name = "Chess"
            version = "{version}"
            protocol = "1.0"
            entrypoint = "bin/chess"
            "#
        ))
        .unwrap()
    }

    /// Writes a catalog holding one release of Chess at `version`.
    fn publish(root: &Path, secret: &SecretKey, serial: u64, version: &str) {
        let descriptor_dir = format!("apps/dev.calum.chess/{version}");
        fs::create_dir_all(root.join(&descriptor_dir)).unwrap();
        fs::write(
            root.join(&descriptor_dir).join("chess.paperpkg"),
            b"archive",
        )
        .unwrap();

        let release = Release::new(
            &manifest(version),
            "chess.paperpkg".parse().unwrap(),
            7,
            Digest::of_bytes(b"archive"),
            0,
            "",
        )
        .unwrap();
        let document = release.to_document();
        let path = root.join(&descriptor_dir).join("release.toml");
        fs::write(&path, &document).unwrap();
        fs::write(
            path.with_extension("toml.sig"),
            secret
                .sign(Domain::RELEASE, document.as_bytes())
                .to_armoured(),
        )
        .unwrap();

        let index = CatalogIndex::new(
            "calum-home",
            serial,
            0,
            vec![IndexEntry::new(
                "dev.calum.chess".parse().unwrap(),
                "Chess".parse().unwrap(),
                version.parse().unwrap(),
                format!("{descriptor_dir}/release.toml").parse().unwrap(),
            )],
        )
        .unwrap();
        let document = index.to_document();
        fs::write(root.join("index.toml"), &document).unwrap();
        fs::write(
            root.join(format!("index.toml{SIGNATURE_SUFFIX}")),
            secret
                .sign(Domain::CATALOG, document.as_bytes())
                .to_armoured(),
        )
        .unwrap();
    }

    fn trusting(secret: &SecretKey) -> TrustedKeys {
        let mut keys = TrustedKeys::none();
        keys.trust(secret.public_key());
        keys
    }

    #[test]
    fn reads_a_published_catalog() {
        let secret = SecretKey::generate().unwrap();
        let served = tempfile::tempdir().unwrap();
        publish(served.path(), &secret, 1, "0.1.0");

        let catalog = Catalog::new(FileTransport::new(served.path()), trusting(&secret));
        let view = catalog.view().unwrap();
        assert!(!view.stale);
        assert_eq!(view.index.serial(), 1);

        let entry = &view.index.entries()[0];
        let release = catalog.release(entry).unwrap();
        assert_eq!(release.release().version().to_string(), "0.1.0");

        let mut archive = catalog.open_archive(entry, &release).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut archive, &mut bytes).unwrap();
        assert_eq!(bytes, b"archive");
    }

    #[test]
    fn an_untrusted_publisher_gets_nowhere() {
        let publisher = SecretKey::generate().unwrap();
        let stranger = SecretKey::generate().unwrap();
        let served = tempfile::tempdir().unwrap();
        publish(served.path(), &publisher, 1, "0.1.0");

        let catalog = Catalog::new(FileTransport::new(served.path()), trusting(&stranger));
        assert!(matches!(catalog.view(), Err(CatalogError::Signature(_))));
    }

    #[test]
    fn a_tampered_index_is_refused() {
        let secret = SecretKey::generate().unwrap();
        let served = tempfile::tempdir().unwrap();
        publish(served.path(), &secret, 1, "0.1.0");

        let index = served.path().join("index.toml");
        let text = fs::read_to_string(&index).unwrap();
        fs::write(&index, text.replace("0.1.0", "9.9.9")).unwrap();

        let catalog = Catalog::new(FileTransport::new(served.path()), trusting(&secret));
        assert!(matches!(catalog.view(), Err(CatalogError::Signature(_))));
    }

    #[test]
    fn a_descriptor_the_index_misdescribes_is_refused() {
        let secret = SecretKey::generate().unwrap();
        let served = tempfile::tempdir().unwrap();
        publish(served.path(), &secret, 1, "0.1.0");

        // Both documents stay correctly signed; only the pairing is wrong.
        let index = CatalogIndex::new(
            "calum-home",
            2,
            0,
            vec![IndexEntry::new(
                "dev.calum.chess".parse().unwrap(),
                "Chess".parse().unwrap(),
                "9.9.9".parse().unwrap(),
                "apps/dev.calum.chess/0.1.0/release.toml".parse().unwrap(),
            )],
        )
        .unwrap();
        let document = index.to_document();
        fs::write(served.path().join("index.toml"), &document).unwrap();
        fs::write(
            served.path().join(format!("index.toml{SIGNATURE_SUFFIX}")),
            secret
                .sign(Domain::CATALOG, document.as_bytes())
                .to_armoured(),
        )
        .unwrap();

        let catalog = Catalog::new(FileTransport::new(served.path()), trusting(&secret));
        let view = catalog.view().unwrap();
        assert!(matches!(
            catalog.release(&view.index.entries()[0]),
            Err(CatalogError::IndexDisagreement { .. })
        ));
    }

    #[test]
    fn an_older_serial_cannot_replace_a_newer_one() {
        let secret = SecretKey::generate().unwrap();
        let served = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let layout = Layout::new(state.path());

        publish(served.path(), &secret, 5, "0.2.0");
        let catalog = Catalog::new(FileTransport::new(served.path()), trusting(&secret))
            .with_cache(layout.clone());
        assert_eq!(catalog.view().unwrap().index.serial(), 5);

        // A correctly signed older index — the replay an attacker on the LAN
        // gets for free by keeping a copy of yesterday's catalog.
        publish(served.path(), &secret, 4, "0.1.0");
        assert!(matches!(
            catalog.view(),
            Err(CatalogError::Rollback {
                offered: 4,
                accepted: 5,
                ..
            })
        ));
    }

    #[test]
    fn the_same_serial_with_different_contents_is_refused() {
        let secret = SecretKey::generate().unwrap();
        let served = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();

        publish(served.path(), &secret, 3, "0.1.0");
        let catalog = Catalog::new(FileTransport::new(served.path()), trusting(&secret))
            .with_cache(Layout::new(state.path()));
        catalog.view().unwrap();

        publish(served.path(), &secret, 3, "0.2.0");
        assert!(matches!(
            catalog.view(),
            Err(CatalogError::StalledSerial { serial: 3, .. })
        ));
    }

    #[test]
    fn an_unreachable_catalog_falls_back_to_the_cache_and_says_so() {
        let secret = SecretKey::generate().unwrap();
        let served = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        publish(served.path(), &secret, 2, "0.1.0");

        let catalog = Catalog::new(FileTransport::new(served.path()), trusting(&secret))
            .with_cache(Layout::new(state.path()));
        assert!(!catalog.view().unwrap().stale);

        fs::remove_file(served.path().join("index.toml")).unwrap();
        let view = catalog.view().unwrap();
        assert!(view.stale);
        assert_eq!(view.index.serial(), 2);
        assert_eq!(view.index.entries().len(), 1);
    }

    #[test]
    fn an_unreachable_catalog_with_no_cache_is_an_error() {
        let secret = SecretKey::generate().unwrap();
        let served = tempfile::tempdir().unwrap();
        let catalog = Catalog::new(FileTransport::new(served.path()), trusting(&secret));
        assert!(matches!(catalog.view(), Err(CatalogError::Transport(_))));
    }

    #[test]
    fn a_transport_refuses_to_leave_its_root() {
        let served = tempfile::tempdir().unwrap();
        let transport = FileTransport::new(served.path());
        assert!(transport.fetch("../../etc/passwd", 1024).is_err());
        assert!(transport.fetch("/etc/passwd", 1024).is_err());

        std::os::unix::fs::symlink("/etc", served.path().join("escape")).unwrap();
        assert!(transport.fetch("escape/passwd", 1024).is_err());
    }

    #[test]
    fn a_catalog_name_cannot_choose_where_its_cache_is_written() {
        assert_eq!(cache_name("../../etc/shadow"), "______etc_shadow.toml");
        assert_eq!(cache_name("calum-home"), "calum-home.toml");
    }
}
