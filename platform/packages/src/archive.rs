//! The `.paperpkg` archive: how a package travels, and how it is opened (§12).
//!
//! A `.paperpkg` is a gzip-compressed tar containing a `paper.toml` at its
//! root plus the payload that manifest declares. Tar because it is the one
//! container format already present everywhere this project touches; gzip
//! because the payload is mostly an already-compressed binary and the
//! difference between compressors is not worth a dependency argument.
//!
//! # Extraction is the hostile part
//!
//! Everything in [`extract`] exists because a tar archive is a list of
//! instructions, and the obvious implementation of "follow them" hands the
//! archive author a filesystem. The rules, all enforced per entry:
//!
//! - **Regular files and directories only.** Symlinks, hard links, devices,
//!   FIFOs and sockets are refused outright rather than filtered later. This
//!   is the direct descendant of the `validate_payload` symlink escape WWW-10
//!   found: a package that can write a link can point its entrypoint at
//!   `/bin/sh`, and the cheapest place to stop that is before the link exists.
//! - **Lexically safe paths.** Every name goes through [`RelativePath`], so
//!   `..`, absolute paths, `~`, `\` and NUL never reach the filesystem.
//! - **Reserved names.** Nothing may be called `.paperclip-*`; those belong to
//!   the installer, and a payload that could write one could forge the marker
//!   that says a release is complete.
//! - **Bounded everything.** Compressed bytes, uncompressed bytes, entry
//!   count, and the size of any single file. A gzip bomb is a few hundred
//!   bytes that becomes a full disk.
//! - **`create_new` only.** Every file is created exclusively, into a
//!   directory the caller promised is empty, so a duplicate entry is an error
//!   rather than an overwrite, and there is no pre-existing link to follow.
//! - **Permissions are assigned, not honoured.** The archive does not get to
//!   say what is executable; the manifest's entrypoint is, everything else is
//!   not.
//!
//! Nothing here executes anything. There is no install script hook and there
//! will not be one (§12).

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

use crate::digest::MeasuredReader;
use crate::error::PathError;
use crate::manifest::{MANIFEST_FILE_NAME, Manifest};
use crate::path::RelativePath;

/// The extension a package archive carries.
pub const ARCHIVE_EXTENSION: &str = "paperpkg";

/// The prefix the installer reserves for its own files inside a release
/// directory. A payload may not contain one.
pub const RESERVED_PREFIX: &str = ".paperclip-";

/// How large a package is allowed to be, before and after decompression.
///
/// The numbers are generous for a chess app and small enough that the worst a
/// hostile archive can do is waste one staging directory. They travel with the
/// installer rather than the archive, because a limit an attacker can set is
/// not a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ArchiveLimits {
    /// Largest compressed archive.
    pub compressed_bytes: u64,
    /// Largest total uncompressed payload.
    pub uncompressed_bytes: u64,
    /// Largest single file in the payload.
    pub file_bytes: u64,
    /// Most entries, files and directories together.
    pub entries: usize,
}

impl ArchiveLimits {
    /// The limits the installer uses unless something says otherwise.
    pub const DEFAULT: ArchiveLimits = ArchiveLimits {
        compressed_bytes: 64 * 1024 * 1024,
        uncompressed_bytes: 192 * 1024 * 1024,
        file_bytes: 128 * 1024 * 1024,
        entries: 4096,
    };
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What came out of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// The manifest found at the archive root.
    pub manifest: Manifest,
    /// Files written, excluding directories.
    pub files: usize,
    /// Payload bytes written to disk.
    ///
    /// The files, not the tar stream: tar pads every entry to a 512-byte block
    /// and the difference is large enough on a small package to make the two
    /// numbers look like a bug when they are reported side by side.
    pub bytes: u64,
}

/// What came out of an archive, before anything has been said about what the
/// files *mean*.
///
/// The half of extraction that is the same for an app package and a platform
/// release (§13): bounded, hostile-input-safe unpacking of a gzipped tar into
/// a directory, with no symlinks, no device nodes, no absolute paths and no
/// `..`. What is then required to be *in* that directory differs, and is the
/// caller's question.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExtractedTree {
    /// Files written, excluding directories.
    pub files: usize,
    /// Payload bytes written to disk.
    pub bytes: u64,
    /// Every regular file written, in the order the archive listed them.
    pub names: Vec<RelativePath>,
}

impl ExtractedTree {
    /// Whether a file with this exact relative path was written.
    pub fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|written| written.as_str() == name)
    }
}

/// Unpacks a gzipped tar stream into `destination`, checking nothing about
/// what it contains.
///
/// `destination` must exist and be empty; the caller owns it, and on any error
/// it is the caller's job to remove it. Not doing that cleanup here is
/// deliberate: the installer already has to delete a staging directory on
/// failure, and a second, partial cleanup path is a second thing to get wrong.
///
/// This is the shared core under [`extract`]. It exists so the platform
/// updater does not need a second extractor: the protections here — the two
/// size bounds, the entry ceiling, the refusal of anything that is not a
/// regular file or a directory, the path sanitising — are the ones §12 asks
/// for, and a second copy of them is a second copy to get wrong. A caller that
/// wants an app package calls [`extract`]; a caller that wants a platform
/// release calls this and then verifies its own signed manifest.
///
/// # Errors
///
/// Any bound exceeded, any forbidden entry, any unwritable path, or a stream
/// that is not a readable gzipped tar.
pub fn extract_tree(
    source: impl Read,
    destination: &Path,
    limits: ArchiveLimits,
) -> Result<ExtractedTree, ArchiveError> {
    ensure_empty_directory(destination)?;

    // Two bounds, because they stop different attacks: the outer one caps what
    // is read off the wire, the inner one caps what decompression can turn it
    // into.
    let compressed = MeasuredReader::new(source, limits.compressed_bytes);
    let decoder = GzDecoder::new(compressed);
    let mut uncompressed = LimitedReader::new(decoder, limits.uncompressed_bytes);
    let mut archive = tar::Archive::new(&mut uncompressed);
    archive.set_preserve_permissions(false);
    archive.set_preserve_mtime(false);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(false);

    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut entries = 0usize;
    let mut names = Vec::new();

    let iter = archive.entries().map_err(ArchiveError::Malformed)?;
    for entry in iter {
        let mut entry = entry.map_err(ArchiveError::Malformed)?;
        entries += 1;
        if entries > limits.entries {
            return Err(ArchiveError::TooManyEntries {
                max: limits.entries,
            });
        }

        let kind = entry.header().entry_type();
        let raw = entry.path().map_err(ArchiveError::Malformed)?.into_owned();
        let name = entry_name(&raw, kind)?;

        match kind {
            tar::EntryType::Directory => {
                create_directory(&name.resolve_within(destination))?;
            }
            tar::EntryType::Regular => {
                let size = entry.header().size().map_err(ArchiveError::Malformed)?;
                if size > limits.file_bytes {
                    return Err(ArchiveError::FileTooLarge {
                        path: name.to_string(),
                        size,
                        max: limits.file_bytes,
                    });
                }
                write_file(&mut entry, destination, &name, size)?;
                files += 1;
                bytes += size;
                names.push(name);
            }
            other => {
                return Err(ArchiveError::ForbiddenEntry {
                    path: raw.display().to_string(),
                    kind: describe(other),
                });
            }
        }
    }

    Ok(ExtractedTree {
        files,
        bytes,
        names,
    })
}

/// Unpacks a `.paperpkg` stream into `destination`.
///
/// [`extract_tree`] does the unpacking; this adds what makes the result an
/// *app package*. The manifest is parsed from the extracted `paper.toml`
/// *after* extraction and the payload validated against it, so what a caller
/// ends up holding describes the bytes that are actually on disk.
///
/// # Errors
///
/// Everything [`extract_tree`] can fail with, plus a missing, unparseable or
/// unsatisfied `paper.toml`.
pub fn extract(
    source: impl Read,
    destination: &Path,
    limits: ArchiveLimits,
) -> Result<Extracted, ArchiveError> {
    let tree = extract_tree(source, destination, limits)?;

    if !tree.contains(MANIFEST_FILE_NAME) {
        return Err(ArchiveError::NoManifest);
    }

    let manifest =
        Manifest::read_package(destination).map_err(|source| ArchiveError::Manifest {
            source: Box::new(source),
        })?;
    manifest
        .validate_payload(destination)
        .map_err(|source| ArchiveError::Payload {
            source: Box::new(source),
        })?;
    set_executable(&manifest.entrypoint().resolve_within(destination))?;

    Ok(Extracted {
        manifest,
        files: tree.files,
        bytes: tree.bytes,
    })
}

/// Builds a `.paperpkg` from a package directory.
///
/// Deterministic: entries are sorted, ownership and timestamps are zeroed, and
/// permissions are assigned from the manifest rather than copied from the
/// build machine. Building the same tree twice produces the same bytes, which
/// is what makes "this version was already published, with these bytes" a
/// question with an answer.
///
/// Only files the manifest declares — the manifest itself, the entrypoint and
/// the assets — go in. A build directory full of object files does not become
/// part of a release by being adjacent to one.
#[cfg(feature = "publishing")]
pub fn build(source: &Path, sink: impl io::Write) -> Result<BuiltArchive, ArchiveError> {
    use std::collections::BTreeSet;

    let manifest = Manifest::read_package(source).map_err(|source| ArchiveError::Manifest {
        source: Box::new(source),
    })?;
    manifest
        .validate_payload(source)
        .map_err(|source| ArchiveError::Payload {
            source: Box::new(source),
        })?;

    // Sorted, so the same tree produces the same archive: `BTreeSet` for the
    // declared paths, with the manifest named first because it is the one
    // entry that is not declared by anything.
    let mut declared: BTreeSet<&RelativePath> = BTreeSet::new();
    declared.insert(manifest.entrypoint());
    declared.extend(manifest.assets());

    let mut files = 0usize;
    let mut bytes = 0u64;
    let encoder = flate2::write::GzEncoder::new(sink, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    bytes += append(&mut builder, source, MANIFEST_FILE_NAME, false)?;
    files += 1;
    for path in declared {
        reject_reserved(path)?;
        let executable = path == manifest.entrypoint();
        bytes += append(&mut builder, source, path.as_str(), executable)?;
        files += 1;
    }

    let encoder = builder.into_inner().map_err(archive_io)?;
    encoder.finish().map_err(archive_io)?;

    Ok(BuiltArchive {
        manifest,
        files,
        bytes,
    })
}

/// What went into an archive that was just built.
#[cfg(feature = "publishing")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltArchive {
    /// The manifest the package declares.
    pub manifest: Manifest,
    /// Files written into the archive.
    pub files: usize,
    /// Uncompressed payload bytes.
    pub bytes: u64,
}

/// Checks and converts one tar entry name.
fn entry_name(raw: &Path, kind: tar::EntryType) -> Result<RelativePath, ArchiveError> {
    let text = raw.to_str().ok_or_else(|| ArchiveError::UnsafePath {
        path: raw.display().to_string(),
        source: PathError::NotPosix,
    })?;
    // Tar writes directories with a trailing slash. Strip it before the
    // lexical check rather than teaching `RelativePath` about a shape only tar
    // produces.
    let text = if kind == tar::EntryType::Directory {
        text.strip_suffix('/').unwrap_or(text)
    } else {
        text
    };
    let path: RelativePath = text.parse().map_err(|source| ArchiveError::UnsafePath {
        path: raw.display().to_string(),
        source,
    })?;
    reject_reserved(&path)?;
    Ok(path)
}

/// Refuses any component that trespasses on the installer's own namespace.
fn reject_reserved(path: &RelativePath) -> Result<(), ArchiveError> {
    for component in path.components() {
        if component.starts_with(RESERVED_PREFIX) {
            return Err(ArchiveError::ReservedName {
                path: path.to_string(),
                component: component.to_owned(),
            });
        }
    }
    Ok(())
}

fn create_directory(path: &Path) -> Result<(), ArchiveError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        // Tar archives commonly name a directory both explicitly and
        // implicitly through the files inside it. A directory that is already
        // a directory is not an escape; we created it ourselves moments ago.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(ArchiveError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_file(
    entry: &mut dyn Read,
    destination: &Path,
    name: &RelativePath,
    size: u64,
) -> Result<(), ArchiveError> {
    let path = name.resolve_within(destination);
    if let Some(parent) = path.parent() {
        create_directories(destination, parent)?;
    }
    // `create_new` is the load-bearing flag: it fails if anything is already
    // at `path`, including a dangling symlink, so a duplicate archive entry
    // cannot overwrite an earlier one and nothing can be followed anywhere.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|source| ArchiveError::Io {
            path: path.clone(),
            source,
        })?;
    let copied = io::copy(&mut entry.take(size), &mut file).map_err(|source| ArchiveError::Io {
        path: path.clone(),
        source,
    })?;
    if copied != size {
        return Err(ArchiveError::Truncated {
            path: name.to_string(),
            declared: size,
            actual: copied,
        });
    }
    Ok(())
}

/// Creates every missing directory between `root` and `path`.
///
/// `create_dir_all` would do this, but it accepts an existing symlink-to-a-
/// directory as "already there". Walking down from a root we created ourselves
/// and calling `create_dir` at each step means every directory on the way is
/// one this function made.
fn create_directories(root: &Path, path: &Path) -> Result<(), ArchiveError> {
    let relative = path.strip_prefix(root).unwrap_or(Path::new(""));
    let mut walked = root.to_path_buf();
    for component in relative.components() {
        walked.push(component);
        create_directory(&walked)?;
    }
    Ok(())
}

fn ensure_empty_directory(path: &Path) -> Result<(), ArchiveError> {
    let mut entries = fs::read_dir(path).map_err(|source| ArchiveError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if entries.next().is_some() {
        return Err(ArchiveError::DestinationNotEmpty {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Marks the entrypoint executable and nothing else.
#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), ArchiveError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|source| {
        ArchiveError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), ArchiveError> {
    Ok(())
}

/// Appends one declared file to an archive under a deterministic header.
#[cfg(feature = "publishing")]
fn append(
    builder: &mut tar::Builder<impl io::Write>,
    source: &Path,
    name: &str,
    executable: bool,
) -> Result<u64, ArchiveError> {
    let on_disk = source.join(name);
    let metadata = fs::symlink_metadata(&on_disk).map_err(|source| ArchiveError::Io {
        path: on_disk.clone(),
        source,
    })?;
    if metadata.is_dir() {
        return Err(ArchiveError::DeclaredDirectory {
            path: name.to_owned(),
        });
    }
    if !metadata.is_file() {
        return Err(ArchiveError::ForbiddenEntry {
            path: name.to_owned(),
            kind: "something that is not a regular file",
        });
    }

    // Built by hand rather than from the file's metadata: uid, gid, mtime and
    // mode are decisions, not facts about the build machine.
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(metadata.len());
    header.set_mode(if executable { 0o755 } else { 0o644 });
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();

    let mut file = fs::File::open(&on_disk).map_err(|source| ArchiveError::Io {
        path: on_disk.clone(),
        source,
    })?;
    builder
        .append_data(&mut header, name, &mut file)
        .map_err(|source| ArchiveError::Io {
            path: on_disk,
            source,
        })?;
    Ok(metadata.len())
}

/// The failure for a write to the archive stream itself, which has no path.
#[cfg(feature = "publishing")]
fn archive_io(source: io::Error) -> ArchiveError {
    ArchiveError::Io {
        path: PathBuf::from("<archive stream>"),
        source,
    }
}

fn describe(kind: tar::EntryType) -> &'static str {
    match kind {
        tar::EntryType::Symlink => "a symbolic link",
        tar::EntryType::Link => "a hard link",
        tar::EntryType::Char => "a character device",
        tar::EntryType::Block => "a block device",
        tar::EntryType::Fifo => "a FIFO",
        _ => "an entry type packages may not contain",
    }
}

/// A reader that stops once its source has produced more than a limit.
///
/// Distinct from [`MeasuredReader`] because this one sits on the *output* of
/// decompression, where there is nothing to hash and the number that matters
/// is how much a small input expanded into.
#[derive(Debug)]
struct LimitedReader<R> {
    inner: R,
    read: u64,
    limit: u64,
}

impl<R: Read> LimitedReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            read: 0,
            limit,
        }
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.read > self.limit {
            return Err(expanded_too_far(self.limit));
        }
        let room = self.limit.saturating_add(1) - self.read;
        let take = buf.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..take])?;
        self.read += n as u64;
        if self.read > self.limit {
            return Err(expanded_too_far(self.limit));
        }
        Ok(n)
    }
}

/// The error a [`LimitedReader`] stops with.
fn expanded_too_far(limit: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the package expands past its {limit} byte limit"),
    )
}

/// Why an archive could not be built or opened.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArchiveError {
    /// The destination directory was not empty, so extraction could have
    /// overwritten something.
    #[error("{path} is not an empty directory")]
    DestinationNotEmpty {
        /// Where extraction was asked to write.
        path: PathBuf,
    },

    /// The archive is not a readable gzip tar, or decompression exceeded its
    /// limit.
    #[error("the archive is not readable, or expands past its size limit")]
    Malformed(#[source] io::Error),

    /// An entry that is not a regular file or a directory.
    #[error(
        "the package contains {kind} at `{path}`; packages may only contain files and directories"
    )]
    ForbiddenEntry {
        /// The entry's name as the archive wrote it.
        path: String,
        /// What it was.
        kind: &'static str,
    },

    /// An entry name that could escape the package.
    #[error("the package contains an unsafe path `{path}`")]
    UnsafePath {
        /// The name as the archive wrote it.
        path: String,
        /// Why it was refused.
        #[source]
        source: PathError,
    },

    /// An entry using a name the installer reserves.
    #[error(
        "the package contains `{path}`, whose component `{component}` uses the \
         reserved `{RESERVED_PREFIX}` prefix"
    )]
    ReservedName {
        /// The offending path.
        path: String,
        /// The offending component.
        component: String,
    },

    /// More entries than the limits allow.
    #[error("the package contains more than {max} entries")]
    TooManyEntries {
        /// The limit.
        max: usize,
    },

    /// A single file larger than the limits allow.
    #[error("`{path}` is {size} bytes, over the {max} byte per-file limit")]
    FileTooLarge {
        /// Which file.
        path: String,
        /// Its declared size.
        size: u64,
        /// The limit.
        max: u64,
    },

    /// An entry's content ran out before its header said it would — the mark
    /// of a truncated download rather than a malicious archive.
    #[error("`{path}` is truncated: {declared} bytes declared, {actual} present")]
    Truncated {
        /// Which file.
        path: String,
        /// What the header said.
        declared: u64,
        /// What was there.
        actual: u64,
    },

    /// No `paper.toml` at the archive root.
    #[error("the package has no {MANIFEST_FILE_NAME} at its root")]
    NoManifest,

    /// The manifest inside the archive is not valid.
    #[error("the package's manifest is not valid")]
    Manifest {
        /// Why.
        #[source]
        source: Box<crate::error::ManifestError>,
    },

    /// The payload does not match the manifest inside the archive.
    #[error("the package's payload does not match its manifest")]
    Payload {
        /// Why.
        #[source]
        source: Box<crate::error::PayloadError>,
    },

    /// A manifest declared a directory where a file was required.
    #[error("`{path}` is a directory; a manifest declares files, not directories")]
    DeclaredDirectory {
        /// Which declared path.
        path: String,
    },

    /// Something on the local filesystem failed.
    #[error("cannot access {path}")]
    Io {
        /// Which path.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },
}

#[cfg(all(test, feature = "publishing"))]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{ARCHIVE_EXTENSION, ArchiveError, ArchiveLimits, build, extract};

    const MANIFEST: &str = r#"
[app]
id = "dev.calum.chess"
name = "Chess"
version = "0.1.0"
protocol = "1.0"
entrypoint = "bin/chess"
assets = ["assets/board.dat"]
"#;

    /// A package directory that is exactly what its manifest declares.
    fn package(root: &Path) {
        fs::write(root.join("paper.toml"), MANIFEST).unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        fs::write(root.join("bin/chess"), b"#!not actually run\n").unwrap();
        fs::create_dir(root.join("assets")).unwrap();
        fs::write(root.join("assets/board.dat"), b"board").unwrap();
    }

    fn built() -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        package(dir.path());
        let mut bytes = Vec::new();
        build(dir.path(), &mut bytes).unwrap();
        bytes
    }

    /// A tar containing whatever entries the caller describes, gzipped.
    fn tar_with(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, kind, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_mode(0o644);
            header.set_size(body.len() as u64);
            if *kind == tar::EntryType::Symlink {
                header.set_size(0);
                header
                    .set_link_name(std::str::from_utf8(body).unwrap())
                    .unwrap();
            }
            header.set_cksum();
            builder.append_data(&mut header, name, &body[..]).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn round_trips_a_package() {
        let archive = built();
        let out = tempfile::tempdir().unwrap();
        let extracted = extract(&archive[..], out.path(), ArchiveLimits::DEFAULT).unwrap();

        assert_eq!(extracted.manifest.id().as_str(), "dev.calum.chess");
        assert_eq!(extracted.files, 3);
        assert_eq!(
            fs::read(out.path().join("assets/board.dat")).unwrap(),
            b"board"
        );
    }

    #[test]
    fn builds_the_same_bytes_twice() {
        assert_eq!(built(), built());
    }

    #[test]
    fn the_entrypoint_is_executable_and_assets_are_not() {
        use std::os::unix::fs::PermissionsExt as _;

        let out = tempfile::tempdir().unwrap();
        extract(&built()[..], out.path(), ArchiveLimits::DEFAULT).unwrap();

        let mode = |p: &str| {
            fs::metadata(out.path().join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("bin/chess") & 0o111, 0o111);
        assert_eq!(mode("assets/board.dat") & 0o111, 0);
    }

    #[test]
    fn an_archive_only_carries_what_the_manifest_declares() {
        let dir = tempfile::tempdir().unwrap();
        package(dir.path());
        fs::write(dir.path().join("secrets.env"), b"TOKEN=hunter2").unwrap();
        let mut bytes = Vec::new();
        build(dir.path(), &mut bytes).unwrap();

        let out = tempfile::tempdir().unwrap();
        extract(&bytes[..], out.path(), ArchiveLimits::DEFAULT).unwrap();
        assert!(!out.path().join("secrets.env").exists());
    }

    #[test]
    fn refuses_a_symlink_entry() {
        // The WWW-10 escape, in archive form: a link named as the entrypoint
        // pointing at a shell. It never reaches the filesystem.
        let archive = tar_with(&[
            ("paper.toml", tar::EntryType::Regular, MANIFEST.as_bytes()),
            ("bin/chess", tar::EntryType::Symlink, b"/bin/sh"),
        ]);
        let out = tempfile::tempdir().unwrap();
        let error = extract(&archive[..], out.path(), ArchiveLimits::DEFAULT).unwrap_err();
        assert!(
            matches!(error, ArchiveError::ForbiddenEntry { kind, .. } if kind == "a symbolic link"),
            "{error:?}"
        );
        assert!(!out.path().join("bin/chess").exists());
    }

    #[test]
    fn refuses_a_hard_link_entry() {
        let archive = tar_with(&[
            ("paper.toml", tar::EntryType::Regular, MANIFEST.as_bytes()),
            ("bin/chess", tar::EntryType::Link, b"/bin/sh"),
        ]);
        let out = tempfile::tempdir().unwrap();
        assert!(matches!(
            extract(&archive[..], out.path(), ArchiveLimits::DEFAULT),
            Err(ArchiveError::ForbiddenEntry { .. })
        ));
    }

    /// A one-entry tar whose name is written straight into the header.
    ///
    /// The `tar` crate's builder refuses to *write* `..` and absolute names,
    /// which is helpful right up until you need to prove the *reader* refuses
    /// them too. A real attacker is not using this library to build the
    /// archive, so neither does this helper.
    fn tar_with_raw_name(name: &str, body: &[u8]) -> Vec<u8> {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(body.len() as u64);
        header.set_mtime(0);
        let raw = header.as_ustar_mut().expect("a ustar header").name.as_mut();
        raw[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();

        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        builder.append(&header, body).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn refuses_a_path_that_escapes_the_package() {
        let archive = tar_with_raw_name("../../etc/cron.d/evil", b"* * * * *");
        let out = tempfile::tempdir().unwrap();
        let error = extract(&archive[..], out.path(), ArchiveLimits::DEFAULT).unwrap_err();
        assert!(
            matches!(&error, ArchiveError::UnsafePath { source, .. }
                if matches!(source, crate::error::PathError::ParentEscape)),
            "{error:?}"
        );
    }

    #[test]
    fn refuses_an_absolute_path() {
        let archive = tar_with_raw_name("/etc/passwd", b"root:x:0:0");
        let out = tempfile::tempdir().unwrap();
        let error = extract(&archive[..], out.path(), ArchiveLimits::DEFAULT).unwrap_err();
        assert!(
            matches!(&error, ArchiveError::UnsafePath { source, .. }
                if matches!(source, crate::error::PathError::Absolute)),
            "{error:?}"
        );
        assert_eq!(walk_bytes(out.path()), 0, "extraction wrote something");
    }

    #[test]
    fn refuses_the_installers_reserved_names() {
        let archive = tar_with(&[
            ("paper.toml", tar::EntryType::Regular, MANIFEST.as_bytes()),
            (
                ".paperclip-release",
                tar::EntryType::Regular,
                b"digest = \"sha256:00\"",
            ),
        ]);
        let out = tempfile::tempdir().unwrap();
        assert!(matches!(
            extract(&archive[..], out.path(), ArchiveLimits::DEFAULT),
            Err(ArchiveError::ReservedName { .. })
        ));
    }

    #[test]
    fn refuses_a_duplicate_entry() {
        let archive = tar_with(&[
            ("paper.toml", tar::EntryType::Regular, MANIFEST.as_bytes()),
            ("bin/chess", tar::EntryType::Regular, b"first"),
            ("bin/chess", tar::EntryType::Regular, b"second"),
        ]);
        let out = tempfile::tempdir().unwrap();
        let error = extract(&archive[..], out.path(), ArchiveLimits::DEFAULT).unwrap_err();
        assert!(
            matches!(&error, ArchiveError::Io { source, .. }
                if source.kind() == std::io::ErrorKind::AlreadyExists),
            "{error:?}"
        );
    }

    #[test]
    fn refuses_an_archive_with_no_manifest() {
        let archive = tar_with(&[("bin/chess", tar::EntryType::Regular, b"x")]);
        let out = tempfile::tempdir().unwrap();
        assert!(matches!(
            extract(&archive[..], out.path(), ArchiveLimits::DEFAULT),
            Err(ArchiveError::NoManifest)
        ));
    }

    #[test]
    fn refuses_a_payload_the_manifest_does_not_match() {
        let archive = tar_with(&[("paper.toml", tar::EntryType::Regular, MANIFEST.as_bytes())]);
        let out = tempfile::tempdir().unwrap();
        assert!(matches!(
            extract(&archive[..], out.path(), ArchiveLimits::DEFAULT),
            Err(ArchiveError::Payload { .. })
        ));
    }

    #[test]
    fn refuses_a_file_larger_than_the_per_file_limit() {
        let out = tempfile::tempdir().unwrap();
        let limits = ArchiveLimits {
            file_bytes: 4,
            ..ArchiveLimits::DEFAULT
        };
        assert!(matches!(
            extract(&built()[..], out.path(), limits),
            Err(ArchiveError::FileTooLarge { .. })
        ));
    }

    #[test]
    fn refuses_a_decompression_bomb() {
        // A *valid* tar whose one entry is 16 MiB of zeros, gzipped down to a
        // few kilobytes. The per-file limit is left wide open so that what
        // stops it is the bound on total expansion, which is the bound a
        // many-small-files bomb would have to get past too.
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::best(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(16 * 1024 * 1024);
        header.set_cksum();
        builder
            .append_data(&mut header, "bin/chess", &vec![0u8; 16 * 1024 * 1024][..])
            .unwrap();
        let bomb = builder.into_inner().unwrap().finish().unwrap();
        assert!(
            (bomb.len() as u64) < 128 * 1024,
            "a bomb that is not small compressed is not a bomb: {} bytes",
            bomb.len()
        );

        let out = tempfile::tempdir().unwrap();
        let limits = ArchiveLimits {
            uncompressed_bytes: 64 * 1024,
            ..ArchiveLimits::DEFAULT
        };
        let error = extract(&bomb[..], out.path(), limits).unwrap_err();
        assert!(
            format!("{error}").contains("cannot access")
                || matches!(error, ArchiveError::Malformed(_)),
            "{error:?}"
        );
        let written: u64 = walk_bytes(out.path());
        assert!(
            written <= 64 * 1024,
            "extraction wrote {written} bytes past a 65536 byte limit"
        );
    }

    /// Total bytes on disk under `root`, used to check a limit actually held.
    fn walk_bytes(root: &Path) -> u64 {
        let mut total = 0;
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            total += if metadata.is_dir() {
                walk_bytes(&entry.path())
            } else {
                metadata.len()
            };
        }
        total
    }

    #[test]
    fn refuses_a_compressed_stream_over_the_wire_limit() {
        let archive = built();
        let out = tempfile::tempdir().unwrap();
        let limits = ArchiveLimits {
            compressed_bytes: 8,
            ..ArchiveLimits::DEFAULT
        };
        assert!(matches!(
            extract(&archive[..], out.path(), limits),
            Err(ArchiveError::Malformed(_))
        ));
    }

    #[test]
    fn refuses_too_many_entries() {
        let entries: Vec<(String, tar::EntryType, Vec<u8>)> = (0..10)
            .map(|i| (format!("f{i}"), tar::EntryType::Regular, b"x".to_vec()))
            .collect();
        let borrowed: Vec<(&str, tar::EntryType, &[u8])> = entries
            .iter()
            .map(|(n, k, b)| (n.as_str(), *k, b.as_slice()))
            .collect();
        let archive = tar_with(&borrowed);
        let out = tempfile::tempdir().unwrap();
        let limits = ArchiveLimits {
            entries: 4,
            ..ArchiveLimits::DEFAULT
        };
        assert!(matches!(
            extract(&archive[..], out.path(), limits),
            Err(ArchiveError::TooManyEntries { max: 4 })
        ));
    }

    #[test]
    fn refuses_a_truncated_archive() {
        let mut archive = built();
        archive.truncate(archive.len() / 2);
        let out = tempfile::tempdir().unwrap();
        assert!(extract(&archive[..], out.path(), ArchiveLimits::DEFAULT).is_err());
    }

    #[test]
    fn refuses_a_destination_that_is_not_empty() {
        let out = tempfile::tempdir().unwrap();
        fs::write(out.path().join("already-here"), b"x").unwrap();
        assert!(matches!(
            extract(&built()[..], out.path(), ArchiveLimits::DEFAULT),
            Err(ArchiveError::DestinationNotEmpty { .. })
        ));
    }

    #[test]
    fn building_refuses_a_symlinked_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("paper.toml"), MANIFEST).unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", dir.path().join("bin/chess")).unwrap();
        fs::create_dir(dir.path().join("assets")).unwrap();
        fs::write(dir.path().join("assets/board.dat"), b"board").unwrap();

        let mut bytes = Vec::new();
        assert!(matches!(
            build(dir.path(), &mut bytes),
            Err(ArchiveError::Payload { .. })
        ));
    }

    #[test]
    fn the_extension_is_what_the_tooling_writes() {
        assert_eq!(ARCHIVE_EXTENSION, "paperpkg");
    }
}
