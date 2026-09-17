//! Content digests, and the one way they are written down (§12).
//!
//! A digest here is always SHA-256 and always spelled `sha256:<64 hex>`. The
//! algorithm is in the text so a future second algorithm cannot be confused
//! with this one, and so a bare hex string — which could be anything — never
//! parses.
//!
//! A digest is an **integrity** check, never an authentication one. Nothing in
//! this crate accepts a digest as evidence about who published something; that
//! is what [`signing`](crate::signing) is for. A digest fetched next to the
//! bytes it describes tells you only that whoever served the bytes also served
//! a matching digest.

use std::fmt;
use std::io::{self, Read};
use std::str::FromStr;

use sha2::{Digest as _, Sha256};

/// The algorithm prefix every digest carries.
const PREFIX: &str = "sha256:";

/// Bytes in a SHA-256 digest.
const LEN: usize = 32;

/// A SHA-256 content digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest([u8; LEN]);

impl Digest {
    /// The digest of a byte slice.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(hasher.finalize().into())
    }

    /// The raw digest bytes.
    pub fn as_bytes(&self) -> &[u8; LEN] {
        &self.0
    }

    /// The digest as lowercase hex, without the `sha256:` prefix.
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(LEN * 2);
        for byte in self.0 {
            out.push(nibble(byte >> 4));
            out.push(nibble(byte & 0x0f));
        }
        out
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", self.to_hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

impl FromStr for Digest {
    type Err = DigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some(hex) = s.strip_prefix(PREFIX) else {
            let algorithm = s.split_once(':').map(|(a, _)| a.to_owned());
            return Err(match algorithm {
                Some(algorithm) => DigestError::UnknownAlgorithm { algorithm },
                None => DigestError::MissingAlgorithm,
            });
        };
        if hex.len() != LEN * 2 {
            return Err(DigestError::Length { len: hex.len() });
        }
        let mut bytes = [0u8; LEN];
        let raw = hex.as_bytes();
        for (index, byte) in bytes.iter_mut().enumerate() {
            let high = unhex(raw[index * 2])?;
            let low = unhex(raw[index * 2 + 1])?;
            *byte = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

/// A reader that hashes and counts everything read through it, and refuses to
/// read past a limit.
///
/// Used wherever bytes arrive from somewhere untrusted: the digest and the
/// length come out of the *same pass* that wrote the bytes to disk, so there
/// is no window in which a file on disk differs from the bytes that were
/// checked (§12).
#[derive(Debug)]
pub struct MeasuredReader<R> {
    inner: R,
    hasher: Sha256,
    read: u64,
    limit: u64,
}

impl<R: Read> MeasuredReader<R> {
    /// Wraps `inner`, refusing to produce more than `limit` bytes.
    pub fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            read: 0,
            limit,
        }
    }

    /// Bytes read so far.
    pub fn bytes_read(&self) -> u64 {
        self.read
    }

    /// The digest of everything read so far.
    pub fn digest(self) -> Digest {
        Digest(self.hasher.finalize().into())
    }
}

impl<R: Read> Read for MeasuredReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.read > self.limit {
            return Err(over_limit(self.limit));
        }
        // One byte past the limit is enough to know the source is too big, and
        // it is never handed on or hashed: the caller cannot end up with a
        // file holding more than `limit` bytes of a source that had more.
        let room = self.limit.saturating_add(1) - self.read;
        let take = buf.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..take])?;
        self.read += n as u64;
        if self.read > self.limit {
            return Err(over_limit(self.limit));
        }
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

/// Why a digest string could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DigestError {
    /// No `<algorithm>:` prefix at all.
    #[error("a digest must be written `sha256:<64 hex characters>`")]
    MissingAlgorithm,

    /// A prefix, but not one this build understands.
    #[error("digest algorithm `{algorithm}` is not supported; only `sha256` is")]
    UnknownAlgorithm {
        /// The algorithm as written.
        algorithm: String,
    },

    /// The right prefix, the wrong amount of hex.
    #[error("a sha256 digest is 64 hex characters, not {len}")]
    Length {
        /// How many characters followed the prefix.
        len: usize,
    },

    /// A character that is not hex.
    #[error("`{character}` is not a hex character")]
    Character {
        /// The offending character.
        character: char,
    },
}

/// The error a [`MeasuredReader`] stops with once its source is too long.
fn over_limit(limit: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("input exceeds the {limit} byte limit"),
    )
}

fn nibble(value: u8) -> char {
    char::from(match value {
        0..=9 => b'0' + value,
        _ => b'a' + value - 10,
    })
}

fn unhex(byte: u8) -> Result<u8, DigestError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(DigestError::Character {
            character: char::from(byte),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::{Digest, DigestError, MeasuredReader};

    /// The published SHA-256 of the empty string; if this fails, the hashing
    /// library is not doing what its name says.
    const EMPTY: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn hashes_known_vectors() {
        assert_eq!(Digest::of_bytes(b"").to_string(), EMPTY);
        assert_eq!(
            Digest::of_bytes(b"abc").to_string(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn round_trips_through_text() {
        let digest = Digest::of_bytes(b"paperclip");
        assert_eq!(digest.to_string().parse::<Digest>().unwrap(), digest);
    }

    #[test]
    fn rejects_bare_hex_and_other_algorithms() {
        let hex = Digest::of_bytes(b"abc").to_hex();
        assert_eq!(hex.parse::<Digest>(), Err(DigestError::MissingAlgorithm));
        assert_eq!(
            format!("md5:{hex}").parse::<Digest>(),
            Err(DigestError::UnknownAlgorithm {
                algorithm: "md5".to_owned()
            })
        );
    }

    #[test]
    fn rejects_malformed_hex() {
        assert_eq!(
            "sha256:abcd".parse::<Digest>(),
            Err(DigestError::Length { len: 4 })
        );
        let mut bad = Digest::of_bytes(b"abc").to_hex();
        bad.replace_range(0..1, "z");
        assert_eq!(
            format!("sha256:{bad}").parse::<Digest>(),
            Err(DigestError::Character { character: 'z' })
        );
    }

    #[test]
    fn measures_and_hashes_in_one_pass() {
        let mut reader = MeasuredReader::new(&b"paperclip"[..], 1024);
        let mut sink = Vec::new();
        reader.read_to_end(&mut sink).unwrap();
        assert_eq!(sink, b"paperclip");
        assert_eq!(reader.bytes_read(), 9);
        assert_eq!(reader.digest(), Digest::of_bytes(b"paperclip"));
    }

    #[test]
    fn refuses_to_read_past_its_limit() {
        let mut reader = MeasuredReader::new(&b"paperclip"[..], 4);
        let mut sink = Vec::new();
        let error = reader.read_to_end(&mut sink).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        // The limit is a ceiling on what is *kept*, not a truncation: the
        // caller gets an error rather than four silently correct bytes, and
        // never sees the byte that broke the limit.
        assert!(sink.len() <= 4);
    }
}
