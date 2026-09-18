//! Errors [`crate::counter`] and [`crate::autostart`] return.
//!
//! Two sources: a raw [`std::io::Error`] for reads, and
//! [`paper_packages::store::StoreError`] for the durable writes, which
//! already knows how to describe itself — see
//! `platform/packages/src/store.rs`'s `atomic_write`.

use std::path::PathBuf;

/// Something durable-state handling could not do.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// The counter file exists but is not a small non-negative integer.
    ///
    /// Deliberately an error rather than a lenient "treat as zero": a
    /// corrupt counter reading as zero is the one reading that silently
    /// turns off the safety this file exists to provide.
    #[error("{path}: not a boot counter (`{text}`)")]
    CorruptCounter {
        /// Where it was.
        path: PathBuf,
        /// What was actually there.
        text: String,
    },
    /// A plain read failed for a reason other than "not found".
    #[error("{path}: {source}")]
    Io {
        /// Where.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A durable write failed.
    #[error("{path}: {source}")]
    Store {
        /// Where.
        path: PathBuf,
        /// Why.
        #[source]
        source: paper_packages::store::StoreError,
    },
    /// Installing or removing the persistent units did not complete.
    ///
    /// A string rather than a structured variant because the things that go
    /// wrong here are not one shape: a `mount` that refused, a `systemctl`
    /// that failed, a write to a root filesystem that is full. What the
    /// caller does with any of them is identical — report it and leave the
    /// device alone — so the detail is for a person to read, not for code to
    /// match on.
    #[error("{0}")]
    Install(String),
}
