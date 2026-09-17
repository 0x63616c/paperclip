//! Publisher signatures over release metadata (§12).
//!
//! # What is signed
//!
//! Exactly one thing, and it is stated here so that no later reader has to
//! infer it from code: the signed message is
//!
//! ```text
//! <domain> || <the published document bytes, verbatim>
//! ```
//!
//! where `<domain>` is one of the fixed, NUL-terminated byte strings in
//! [`Domain`], and the document bytes are the bytes of the published file
//! exactly as they were served — never a re-serialisation of a parsed value.
//!
//! That second half is the whole point. A scheme that signs "the document"
//! and then verifies by re-serialising a parsed structure is not a signature
//! scheme; it is a signature over whichever serialiser happened to be linked
//! that day. [`Verified`] therefore exists only as the result of
//! [`TrustedKeys::verify`], and every parser in this crate that matters takes
//! a `Verified` rather than a `&[u8]`: **verify the bytes, then parse the
//! bytes you verified.**
//!
//! # Where the keys live
//!
//! The secret half never leaves the publishing machine. [`SecretKey`] is
//! behind the `publishing` feature, which the device build turns off, so a
//! tablet binary does not contain a code path that can hold a signing key.
//! The tablet holds only [`PublicKey`]s, in a [`TrustedKeys`] set built from
//! files the operator installed — never from the artefact being verified. A
//! signature names a [`KeyId`]; it does not carry the key it was made with,
//! because a self-carried key authenticates nothing.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use ed25519_dalek::{Signature as RawSignature, VerifyingKey};

use crate::digest::Digest;

/// The domain separator a message is signed under.
///
/// Two documents that mean different things must never be interchangeable
/// even when their bytes happen to coincide, so every signature commits to
/// which kind of document it was made over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain(&'static [u8]);

impl Domain {
    /// A release descriptor: one app version and the archive that carries it.
    pub const RELEASE: Domain = Domain(b"paperclip.release.v1\0");

    /// A catalog index: which releases a catalog is offering, and its serial.
    pub const CATALOG: Domain = Domain(b"paperclip.catalog.v1\0");

    /// The separator bytes.
    pub fn as_bytes(self) -> &'static [u8] {
        self.0
    }

    /// The name used in error messages.
    pub fn label(self) -> &'static str {
        match self.0.split_last() {
            Some((_, name)) => std::str::from_utf8(name).unwrap_or("unknown"),
            None => "unknown",
        }
    }

    /// The exact bytes an Ed25519 signature is computed over.
    fn message(self, document: &[u8]) -> Vec<u8> {
        let mut message = Vec::with_capacity(self.0.len() + document.len());
        message.extend_from_slice(self.0);
        message.extend_from_slice(document);
        message
    }
}

/// A short, stable name for a public key: the first 8 bytes of its SHA-256.
///
/// Short enough to type into a `paperctl` flag, long enough that a second key
/// colliding with a trusted one is not a thing that happens by accident. It is
/// a *lookup* key, never a trust decision on its own — [`TrustedKeys`] still
/// verifies against the full public key it has on file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyId([u8; 8]);

impl KeyId {
    /// The id of a public key.
    fn of(key: &VerifyingKey) -> Self {
        let digest = Digest::of_bytes(key.as_bytes());
        let mut id = [0u8; 8];
        id.copy_from_slice(&digest.as_bytes()[..8]);
        Self(id)
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for KeyId {
    type Err = SignatureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = decode_hex(s, 8).ok_or(SignatureError::MalformedKeyId)?;
        let mut id = [0u8; 8];
        id.copy_from_slice(&bytes);
        Ok(Self(id))
    }
}

/// A publisher's public key, as the tablet holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    inner: VerifyingKey,
    id: KeyId,
}

impl PublicKey {
    /// The armour line a public key file contains.
    pub const ARMOUR: &'static str = "paperclip-public-key-v1";

    /// This key's id.
    pub fn id(&self) -> KeyId {
        self.id
    }

    /// The single line written to a public key file, newline included.
    pub fn to_armoured(&self) -> String {
        format!("{} {}\n", Self::ARMOUR, encode_hex(self.inner.as_bytes()))
    }
}

impl FromStr for PublicKey {
    type Err = SignatureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = armoured_body(s, Self::ARMOUR, 32)?;
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&bytes);
        // Rejects non-canonical and small-order encodings, so a key that
        // cannot make a meaningful signature never enters the trust set.
        let inner = VerifyingKey::from_bytes(&raw).map_err(|_| SignatureError::MalformedKey)?;
        let id = KeyId::of(&inner);
        Ok(Self { inner, id })
    }
}

/// A detached signature: which key made it, and the 64 bytes it produced.
///
/// Detached because the alternative — an envelope that wraps the document —
/// forces every reader to agree on how to unwrap it before it can check
/// anything, and disagreement there is indistinguishable from forgery. A
/// detached `.sig` file next to the document leaves the document byte-exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    key: KeyId,
    raw: [u8; 64],
}

impl Signature {
    /// The armour line a signature file contains.
    pub const ARMOUR: &'static str = "paperclip-signature-v1";

    /// Which key this claims to be from.
    pub fn key_id(&self) -> KeyId {
        self.key
    }

    /// The single line written to a `.sig` file, newline included.
    pub fn to_armoured(&self) -> String {
        format!("{} {} {}\n", Self::ARMOUR, self.key, encode_hex(&self.raw))
    }
}

impl FromStr for Signature {
    type Err = SignatureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let line = single_line(s)?;
        let mut fields = line.split(' ');
        if fields.next() != Some(Self::ARMOUR) {
            return Err(SignatureError::NotASignature);
        }
        let key: KeyId = fields
            .next()
            .ok_or(SignatureError::MalformedKeyId)?
            .parse()?;
        let body = fields.next().ok_or(SignatureError::Malformed)?;
        if fields.next().is_some() {
            return Err(SignatureError::Malformed);
        }
        let bytes = decode_hex(body, 64).ok_or(SignatureError::Malformed)?;
        let mut raw = [0u8; 64];
        raw.copy_from_slice(&bytes);
        Ok(Self { key, raw })
    }
}

/// The public keys this installation will accept releases from.
///
/// Built from files the operator put in place. Nothing in a catalog, a
/// release or a package can add to it — the "trust on first use" shortcut is
/// deliberately absent, because on a single personal device the cost of
/// copying one public key across once is a rounding error against the cost of
/// a catalog that can nominate its own publisher.
#[derive(Debug, Clone, Default)]
pub struct TrustedKeys {
    keys: BTreeMap<KeyId, PublicKey>,
}

impl TrustedKeys {
    /// A set that trusts nobody.
    pub fn none() -> Self {
        Self::default()
    }

    /// Adds a key the operator has decided to trust.
    pub fn trust(&mut self, key: PublicKey) -> &mut Self {
        self.keys.insert(key.id(), key);
        self
    }

    /// Whether anything is trusted at all.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Every trusted key id, in a stable order.
    pub fn key_ids(&self) -> impl Iterator<Item = KeyId> + '_ {
        self.keys.keys().copied()
    }

    /// Verifies `signature` over `document` under `domain`.
    ///
    /// On success the caller gets a [`Verified`] holding *the same bytes that
    /// were verified*. There is no way to obtain one otherwise, which is what
    /// stops a parse-then-verify ordering mistake from being expressible.
    pub fn verify<'a>(
        &self,
        domain: Domain,
        document: &'a [u8],
        signature: &Signature,
    ) -> Result<Verified<'a>, SignatureError> {
        let key = self
            .keys
            .get(&signature.key)
            .ok_or(SignatureError::UntrustedKey {
                key: signature.key,
                trusted: self.keys.len(),
            })?;
        let raw = RawSignature::from_bytes(&signature.raw);
        // `verify_strict`, not `verify`: the strict variant rejects
        // small-order and non-canonical `R` components, so a signature this
        // build accepts is one every other conforming implementation accepts
        // too. Malleability here would mean two distinct signature files that
        // both verify over the same release.
        key.inner
            .verify_strict(&domain.message(document), &raw)
            .map_err(|_| SignatureError::BadSignature {
                key: signature.key,
                domain: domain.label(),
            })?;
        Ok(Verified {
            bytes: document,
            signer: signature.key,
            domain,
        })
    }
}

/// Bytes that a trusted key has signed, and the key that signed them.
///
/// Holds a borrow of the original bytes rather than a copy, so "the bytes that
/// were verified" and "the bytes about to be parsed" are the same memory, not
/// two values that a later edit could let drift apart.
#[derive(Debug, Clone, Copy)]
pub struct Verified<'a> {
    bytes: &'a [u8],
    signer: KeyId,
    domain: Domain,
}

impl<'a> Verified<'a> {
    /// The verified bytes.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Which key signed them.
    pub fn signer(&self) -> KeyId {
        self.signer
    }

    /// Which kind of document they were signed as.
    pub fn domain(&self) -> Domain {
        self.domain
    }
}

/// The signing half. Publishing machines only.
#[cfg(feature = "publishing")]
#[derive(Clone)]
pub struct SecretKey {
    inner: ed25519_dalek::SigningKey,
}

#[cfg(feature = "publishing")]
impl SecretKey {
    /// The armour line a secret key file contains.
    pub const ARMOUR: &'static str = "paperclip-secret-key-v1";

    /// A new key from the operating system's CSPRNG.
    pub fn generate() -> Result<Self, SignatureError> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|_| SignatureError::NoEntropy)?;
        Ok(Self {
            inner: ed25519_dalek::SigningKey::from_bytes(&seed),
        })
    }

    /// The matching public key.
    pub fn public_key(&self) -> PublicKey {
        let inner = self.inner.verifying_key();
        let id = KeyId::of(&inner);
        PublicKey { inner, id }
    }

    /// Signs `document` under `domain`.
    pub fn sign(&self, domain: Domain, document: &[u8]) -> Signature {
        use ed25519_dalek::Signer as _;

        Signature {
            key: self.public_key().id(),
            raw: self.inner.sign(&domain.message(document)).to_bytes(),
        }
    }

    /// The single line written to a secret key file, newline included.
    ///
    /// The caller is responsible for the mode it lands on disk with; see
    /// `paperctl key generate`.
    pub fn to_armoured(&self) -> String {
        format!("{} {}\n", Self::ARMOUR, encode_hex(self.inner.as_bytes()))
    }
}

#[cfg(feature = "publishing")]
impl FromStr for SecretKey {
    type Err = SignatureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = armoured_body(s, Self::ARMOUR, 32)?;
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Ok(Self {
            inner: ed25519_dalek::SigningKey::from_bytes(&seed),
        })
    }
}

#[cfg(feature = "publishing")]
impl fmt::Debug for SecretKey {
    /// Prints the *public* id and nothing else: a secret key that can reach a
    /// log through a derived `Debug` is a secret key that will.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey({})", self.public_key().id())
    }
}

/// Why a key, signature or verification failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SignatureError {
    /// The file did not start with the expected armour line.
    #[error("expected a `{expected}` line")]
    WrongArmour {
        /// What the file should have said.
        expected: &'static str,
    },

    /// A signature file that is not a signature file.
    #[error("expected a `paperclip-signature-v1` line")]
    NotASignature,

    /// The armour line was there but the rest of it was not usable.
    #[error("the line is not in `<armour> <hex>` form")]
    Malformed,

    /// A key id that is not sixteen hex characters.
    #[error("a key id is 16 hex characters")]
    MalformedKeyId,

    /// Thirty-two bytes that are not a valid Ed25519 public key.
    #[error("those 32 bytes are not a valid Ed25519 public key")]
    MalformedKey,

    /// More than one line, or none.
    #[error("expected exactly one line")]
    NotOneLine,

    /// The signature is from a key this installation has not been told to
    /// trust. Not the same failure as a bad signature, and worth its own
    /// message: the fix is to install a public key, not to re-publish.
    #[error(
        "signed by key {key}, which is not trusted here ({trusted} key(s) trusted). \
         Install the publisher's public key before installing from this catalog."
    )]
    UntrustedKey {
        /// The key the signature named.
        key: KeyId,
        /// How many keys are trusted.
        trusted: usize,
    },

    /// A trusted key, and the signature still does not check out. The document
    /// was altered after it was signed, or it was signed as something else.
    #[error("the {domain} signature from key {key} does not verify; the document was altered")]
    BadSignature {
        /// The key that should have signed it.
        key: KeyId,
        /// The domain it was verified under.
        domain: &'static str,
    },

    /// The system CSPRNG refused. Nothing sensible to do but stop.
    #[error("the operating system would not provide entropy for a new key")]
    NoEntropy,
}

/// The body of a one-line armoured file, decoded and length-checked.
fn armoured_body(
    text: &str,
    armour: &'static str,
    bytes: usize,
) -> Result<Vec<u8>, SignatureError> {
    let line = single_line(text)?;
    let body = line
        .strip_prefix(armour)
        .and_then(|rest| rest.strip_prefix(' '))
        .ok_or(SignatureError::WrongArmour { expected: armour })?;
    decode_hex(body, bytes).ok_or(SignatureError::Malformed)
}

/// The single meaningful line of `text`, with blank lines and `#` comments
/// discarded so a key file can be annotated.
fn single_line(text: &str) -> Result<&str, SignatureError> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let line = lines.next().ok_or(SignatureError::NotOneLine)?;
    if lines.next().is_some() {
        return Err(SignatureError::NotOneLine);
    }
    Ok(line)
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn decode_hex(text: &str, expected: usize) -> Option<Vec<u8>> {
    if text.len() != expected * 2 {
        return None;
    }
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(expected);
    for pair in raw.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(all(test, feature = "publishing"))]
mod tests {
    use super::{Domain, PublicKey, SecretKey, Signature, SignatureError, TrustedKeys};

    fn keypair() -> (SecretKey, TrustedKeys) {
        let secret = SecretKey::generate().unwrap();
        let mut trusted = TrustedKeys::none();
        trusted.trust(secret.public_key());
        (secret, trusted)
    }

    #[test]
    fn verifies_a_signature_over_the_exact_bytes() {
        let (secret, trusted) = keypair();
        let document = b"[release]\nversion = \"1.0.0\"\n";
        let signature = secret.sign(Domain::RELEASE, document);

        let verified = trusted
            .verify(Domain::RELEASE, document, &signature)
            .unwrap();
        assert_eq!(verified.bytes(), document);
        assert_eq!(verified.signer(), secret.public_key().id());
    }

    #[test]
    fn a_single_byte_of_whitespace_breaks_the_signature() {
        let (secret, trusted) = keypair();
        let document = b"[release]\nversion = \"1.0.0\"\n";
        let signature = secret.sign(Domain::RELEASE, document);

        // Semantically identical TOML, different bytes. This is exactly the
        // case a "re-serialise and compare" scheme would wave through.
        let reserialised = b"[release]\nversion  = \"1.0.0\"\n";
        assert!(matches!(
            trusted.verify(Domain::RELEASE, reserialised, &signature),
            Err(SignatureError::BadSignature { .. })
        ));
    }

    #[test]
    fn a_release_signature_is_not_a_catalog_signature() {
        let (secret, trusted) = keypair();
        let document = b"serial = 3\n";
        let signature = secret.sign(Domain::RELEASE, document);
        assert!(matches!(
            trusted.verify(Domain::CATALOG, document, &signature),
            Err(SignatureError::BadSignature { .. })
        ));
    }

    #[test]
    fn an_untrusted_key_is_refused_even_with_a_valid_signature() {
        let stranger = SecretKey::generate().unwrap();
        let (_, trusted) = keypair();
        let document = b"anything";
        let signature = stranger.sign(Domain::RELEASE, document);
        assert!(matches!(
            trusted.verify(Domain::RELEASE, document, &signature),
            Err(SignatureError::UntrustedKey { .. })
        ));
    }

    #[test]
    fn a_signature_cannot_nominate_its_own_key() {
        // The armoured form carries a key *id*, never a key. There is no
        // parse that turns a signature file into something trustable.
        let stranger = SecretKey::generate().unwrap();
        let signature = stranger.sign(Domain::RELEASE, b"x");
        let text = signature.to_armoured();
        assert!(!text.contains(&super::encode_hex(stranger.public_key().inner.as_bytes())));
        assert_eq!(text.parse::<Signature>().unwrap(), signature);
    }

    #[test]
    fn keys_and_signatures_round_trip_through_their_files() {
        let secret = SecretKey::generate().unwrap();
        let public = secret.public_key();
        let reparsed = secret.to_armoured().parse::<SecretKey>();
        assert!(reparsed.is_ok());
        assert_eq!(reparsed.unwrap().public_key(), public);
        assert_eq!(public.to_armoured().parse::<PublicKey>().unwrap(), public);

        let signature = secret.sign(Domain::CATALOG, b"x");
        assert_eq!(
            signature.to_armoured().parse::<Signature>().unwrap(),
            signature
        );
    }

    #[test]
    fn key_files_tolerate_comments_and_reject_second_lines() {
        let public = SecretKey::generate().unwrap().public_key();
        let annotated = format!("# calum's publishing key\n\n{}", public.to_armoured());
        assert_eq!(annotated.parse::<PublicKey>().unwrap(), public);

        let doubled = format!("{}{}", public.to_armoured(), public.to_armoured());
        assert_eq!(
            doubled.parse::<PublicKey>(),
            Err(SignatureError::NotOneLine)
        );
    }

    #[test]
    fn a_secret_key_never_prints_itself() {
        let secret = SecretKey::generate().unwrap();
        let printed = format!("{secret:?}");
        assert!(printed.contains(&secret.public_key().id().to_string()));
        assert!(!printed.contains(&super::encode_hex(secret.inner.as_bytes())));
    }

    #[test]
    fn a_public_key_file_is_not_a_secret_key_file() {
        let secret = SecretKey::generate().unwrap();
        assert!(matches!(
            secret.public_key().to_armoured().parse::<SecretKey>(),
            Err(SignatureError::WrongArmour {
                expected: SecretKey::ARMOUR
            })
        ));
    }
}
