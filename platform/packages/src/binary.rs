//! Is the entrypoint a program this tablet could actually run?
//!
//! The package-time answer to the hostile case "invalid binary". A manifest
//! can name any file as its entrypoint, and every check before this one is
//! satisfied by a text file: the path is lexically safe, the file exists, it
//! is not a symlink, it is the right size. None of that distinguishes an
//! aarch64 executable from a shell script from a JPEG.
//!
//! Only the ELF header is read — 20 bytes — because that is where the answer
//! is. This is not a loader and does not pretend to be one: a file that passes
//! here is a file whose header says aarch64 ELF, which is what "could run on
//! that tablet" means at package time. Whether it then runs is launch-time's
//! question, and it has its own answer.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Bytes of ELF header this reads.
const HEADER_BYTES: usize = 20;

const MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const CLASS_64: u8 = 2;
const DATA_LITTLE_ENDIAN: u8 = 1;
const TYPE_EXECUTABLE: u16 = 2;
const TYPE_SHARED_OBJECT: u16 = 3;

/// `EM_AARCH64`. The tablet's architecture (WWW-1).
pub const MACHINE_AARCH64: u16 = 0xB7;

/// What kind of ELF object a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ObjectKind {
    /// A fixed-position executable.
    Executable,
    /// A position-independent executable, which is what a modern Rust release
    /// build produces and therefore the normal case here.
    SharedObject,
}

/// What an entrypoint's ELF header says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExecutableTarget {
    /// `e_machine`.
    pub machine: u16,
    /// `e_type`, narrowed to the two that can be executed.
    pub kind: ObjectKind,
}

impl ExecutableTarget {
    /// Whether this is something the tablet could run.
    pub const fn runs_on_device(self) -> bool {
        self.machine == MACHINE_AARCH64
    }
}

/// Why a file is not a usable entrypoint.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BinaryError {
    /// The file could not be read.
    #[error("cannot read {path}")]
    Read {
        /// The path attempted.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },

    /// The file is not an ELF object at all — a script, an archive, a
    /// directory listing somebody renamed.
    #[error("{path} is not an ELF executable")]
    NotElf {
        /// The path attempted.
        path: PathBuf,
    },

    /// 32-bit, or big-endian. Neither exists on this device.
    #[error("{path} is not a 64-bit little-endian ELF object")]
    WrongElfShape {
        /// The path attempted.
        path: PathBuf,
    },

    /// An ELF object that is not executable — an object file, a core dump.
    #[error("{path} is an ELF object but not an executable (e_type {kind})")]
    NotExecutable {
        /// The path attempted.
        path: PathBuf,
        /// The `e_type` that was found.
        kind: u16,
    },

    /// Built for the wrong architecture. Almost always a Mac binary that was
    /// packaged without cross-compiling.
    #[error("{path} is built for machine {found:#x}, not aarch64 ({expected:#x})")]
    WrongMachine {
        /// The path attempted.
        path: PathBuf,
        /// What the header said.
        found: u16,
        /// What the device needs.
        expected: u16,
    },
}

/// Reads an entrypoint's ELF header.
pub fn inspect_entrypoint(path: &Path) -> Result<ExecutableTarget, BinaryError> {
    let mut header = [0u8; HEADER_BYTES];
    let mut file = File::open(path).map_err(|source| BinaryError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    // A file shorter than the header cannot be an ELF object, which is the
    // same answer as a file whose header is wrong.
    if file.read_exact(&mut header).is_err() {
        return Err(BinaryError::NotElf {
            path: path.to_path_buf(),
        });
    }

    if header[0..4] != MAGIC {
        return Err(BinaryError::NotElf {
            path: path.to_path_buf(),
        });
    }
    if header[4] != CLASS_64 || header[5] != DATA_LITTLE_ENDIAN {
        return Err(BinaryError::WrongElfShape {
            path: path.to_path_buf(),
        });
    }

    let kind = u16::from_le_bytes([header[16], header[17]]);
    let kind = match kind {
        TYPE_EXECUTABLE => ObjectKind::Executable,
        TYPE_SHARED_OBJECT => ObjectKind::SharedObject,
        other => {
            return Err(BinaryError::NotExecutable {
                path: path.to_path_buf(),
                kind: other,
            });
        }
    };
    let machine = u16::from_le_bytes([header[18], header[19]]);
    Ok(ExecutableTarget { machine, kind })
}

/// Reads an entrypoint's header and insists it targets the tablet.
pub fn require_device_entrypoint(path: &Path) -> Result<ExecutableTarget, BinaryError> {
    let target = inspect_entrypoint(path)?;
    if !target.runs_on_device() {
        return Err(BinaryError::WrongMachine {
            path: path.to_path_buf(),
            found: target.machine,
            expected: MACHINE_AARCH64,
        });
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::{
        BinaryError, MACHINE_AARCH64, ObjectKind, inspect_entrypoint, require_device_entrypoint,
    };
    use std::path::Path;

    /// Builds the first 20 bytes of an ELF header.
    fn header(class: u8, data: u8, kind: u16, machine: u16) -> Vec<u8> {
        let mut bytes = vec![0x7f, b'E', b'L', b'F', class, data];
        bytes.resize(16, 0);
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&machine.to_le_bytes());
        bytes
    }

    fn written(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin");
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn accepts_an_aarch64_position_independent_executable() {
        let (_dir, path) = written(&header(2, 1, 3, MACHINE_AARCH64));
        let target = require_device_entrypoint(&path).unwrap();
        assert_eq!(target.machine, MACHINE_AARCH64);
        assert_eq!(target.kind, ObjectKind::SharedObject);
        assert!(target.runs_on_device());
    }

    /// The hostile case by name: a package whose entrypoint is not a program.
    /// Every earlier check passes for a shell script — the path is safe, the
    /// file exists, it is not a link — so this is the one that catches it.
    #[test]
    fn a_shell_script_is_not_a_binary() {
        let (_dir, path) = written(b"#!/bin/sh\nrm -rf /\n");
        assert!(matches!(
            inspect_entrypoint(&path),
            Err(BinaryError::NotElf { .. })
        ));
    }

    #[test]
    fn a_file_too_short_to_have_a_header_is_not_a_binary() {
        let (_dir, path) = written(&[0x7f, b'E']);
        assert!(matches!(
            inspect_entrypoint(&path),
            Err(BinaryError::NotElf { .. })
        ));
        let (_dir, path) = written(b"");
        assert!(matches!(
            inspect_entrypoint(&path),
            Err(BinaryError::NotElf { .. })
        ));
    }

    /// The realistic mistake: packaging the binary `cargo build` just produced
    /// on the Mac instead of the cross-compiled one.
    #[test]
    fn a_host_binary_is_refused_with_the_architecture_named() {
        const EM_X86_64: u16 = 0x3E;
        let (_dir, path) = written(&header(2, 1, 2, EM_X86_64));
        let error = require_device_entrypoint(&path).unwrap_err();
        assert!(
            matches!(error, BinaryError::WrongMachine { found, .. } if found == EM_X86_64),
            "{error:?}"
        );
        // It is still a readable ELF object; only the target is wrong.
        assert!(inspect_entrypoint(&path).is_ok());
    }

    #[test]
    fn a_32_bit_or_big_endian_object_is_refused() {
        let (_dir, path) = written(&header(1, 1, 2, MACHINE_AARCH64));
        assert!(matches!(
            inspect_entrypoint(&path),
            Err(BinaryError::WrongElfShape { .. })
        ));
        let (_dir, path) = written(&header(2, 2, 2, MACHINE_AARCH64));
        assert!(matches!(
            inspect_entrypoint(&path),
            Err(BinaryError::WrongElfShape { .. })
        ));
    }

    #[test]
    fn a_relocatable_object_file_is_not_an_entrypoint() {
        const ET_REL: u16 = 1;
        let (_dir, path) = written(&header(2, 1, ET_REL, MACHINE_AARCH64));
        assert!(matches!(
            inspect_entrypoint(&path),
            Err(BinaryError::NotExecutable { kind: ET_REL, .. })
        ));
    }

    #[test]
    fn a_missing_file_reports_the_path() {
        assert!(matches!(
            inspect_entrypoint(Path::new("/nonexistent/paperclip/bin")),
            Err(BinaryError::Read { .. })
        ));
    }
}
