# ADR-0013 — The `.paperpkg` archive, and exactly which bytes are signed

**Status:** accepted (Stage 7, WWW-7)

## Context

§12 requires signed, immutable, versioned releases published from the Mac to a
private home-network catalog. Ed25519 and established hashing libraries. It
also requires — in so many words — that the scheme **define precisely which
bytes are signed**, with detached signatures or a well-specified envelope, and
that it not depend on unspecified JSON reserialisation.

That last requirement is the one this ADR exists for. A signature scheme where
the verifier re-serialises a parsed structure and compares is not a signature
scheme; it is a signature over whichever serialiser happened to be linked. TOML
has no canonical form, `toml` makes no byte-stability promise across versions,
and a patch release of a dependency can silently turn every existing signature
into a failure — or, worse, make two different documents verify against one
signature.

## Decision

### The archive

`.paperpkg` is **gzip over tar**. Tar because it is already present everywhere
this project touches; gzip because the payload is a mostly-incompressible
binary and picking a better compressor is not worth a dependency argument.

Building is deterministic: entries sorted, uid/gid/mtime zeroed, mode assigned
from the manifest rather than copied from the build machine. The same tree
produces the same bytes, which is what makes "this version was already
published, with these bytes" a question with an answer.

Only files the manifest declares go in — the manifest, the entrypoint, the
assets. A build directory full of object files does not join a release by being
adjacent to one.

### Extraction

`paper_packages::archive::extract` enforces, per entry:

- **Regular files and directories only.** Symlinks, hard links, devices, FIFOs
  and sockets are refused outright. This is the descendant of the symlink
  escape WWW-10 found in `validate_payload`: a package that can write a link
  can point its entrypoint at `/bin/sh`, and the cheapest place to stop that is
  before the link exists.
- **Lexically safe paths**, through the existing `RelativePath`.
- **No reserved names.** Nothing may be called `.paperclip-*`; those belong to
  the installer, and a payload able to write one could forge the marker that
  says a release is complete.
- **Bounded everything** — compressed bytes, total uncompressed bytes, entry
  count, per-file size. The limits travel with the installer, never with the
  archive: a limit an attacker can set is not a limit.
- **`create_new` only**, into a directory the caller has proved is empty. A
  duplicate entry is an error rather than an overwrite, and there is no
  pre-existing name to follow.
- **Permissions assigned, not honoured.** The entrypoint is executable;
  nothing else is.

Nothing is executed. There is no install-script hook and there will not be one.

### The signed bytes

Two documents are signed, separately:

| Document | Domain separator | Signed over |
|---|---|---|
| `release.toml` | `paperclip.release.v1\0` | the file's bytes, verbatim |
| `index.toml` | `paperclip.catalog.v1\0` | the file's bytes, verbatim |

The signed message is `<domain> ‖ <document bytes as served>`. Signatures are
**detached**, in a `<name>.sig` beside the document, so the document stays
byte-exact and no reader has to agree on how to unwrap an envelope before it
can check anything — disagreement there is indistinguishable from forgery.

The domain separator means a release signature can never be replayed as a
catalog signature.

In code the ordering is enforced by types rather than by discipline:
`TrustedKeys::verify` is the only source of a `Verified`, `Verified` borrows
the bytes it verified, and every parser takes a `Verified` rather than a
`&[u8]`. **Verify the bytes, then parse the bytes you verified.** There is no
code path that parses first.

A signature names a `KeyId` — eight bytes of SHA-256 over the public key — and
never carries the key itself. A self-carried key authenticates nothing. The
trust set is built only from public key files the operator installed; there is
no trust-on-first-use.

### Two signatures, not one

Releases are signed individually, not vouched for by the index. The index
changes on every publish; a release descriptor is written once. Signing
releases through the index would make every past release's authenticity depend
on the freshest index being honest, and would make "this version was published
with these bytes" unanswerable after the fact.

### Digests are integrity, never authentication

A digest says the bytes are the bytes. It says nothing about who produced them.
`Digest` is spelled `sha256:<64 hex>` so that a bare hex string never parses,
and nothing in the crate accepts one as evidence about a publisher.

### Where the secret key can exist

`SecretKey` lives behind the `publishing` cargo feature, on by default because
the Mac is where this crate is built. The device build turns it off
(`default-features = false`) and does not link a code path that can hold a
signing key. `paperctl key generate` writes the secret with mode `0o600` set at
creation — not a later `chmod`, which leaves a window in which the key is
world-readable.

### Serials

`CatalogIndex` carries a monotonic `serial`. Every signature a publisher ever
made stays valid forever, so "this verifies" cannot answer "this is current":
someone who can serve bytes can serve last month's correctly signed index and
hide the release that fixed something. The device remembers the highest serial
it has accepted; a lower one is refused, and the same serial with different
content is refused too.

## Consequences

- Publishing is `paperctl package` then `paperctl publish`. Serving a catalog
  is a static file server over a directory. There is no database and no
  device-control server.
- `Transport` fetches bytes and has no opinion about them, so signatures are
  required regardless of transport. Over plain HTTP on a home LAN the
  signature is the only thing between the tablet and whoever else is on the
  network; over HTTPS it is still the only thing that says *the publisher*
  produced this rather than the server.
- A transport using HTTPS must verify certificates. One that skips
  verification is worse than plain HTTP, because it looks safe.
- Only `FileTransport` ships in this pass. An HTTP transport lands with the App
  Store, which is the first thing that needs one; the trait is the seam and
  nothing above it changes.
- `Capability::Packages` is added, with `PackageManager::on_behalf_of` as its
  enforcement point. The App Store manages packages because policy granted it
  that, not because it is the App Store.
- Republishing identical bytes is a no-op rather than an error. Republishing a
  version with *different* bytes is refused.

## What would make this wrong

- If a release ever needs to carry something that is not a file — a signature
  over a range, a delta against a previous version — the "whole document,
  verbatim" rule needs revisiting rather than extending.
- If the catalog gains more than one publisher, `KeyId` collisions stop being a
  thing that does not happen by accident and the eight-byte id needs widening.
- If Ed25519 is ever the wrong primitive here, the domain separators are
  versioned (`.v1`) precisely so a second scheme can coexist rather than
  replace.
