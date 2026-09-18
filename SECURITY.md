# Security policy

Paperclip is a personal application environment for one reMarkable Paper Pro
(see the project description). It is not a multi-tenant service and there is
no SLA, but two properties matter enough to state plainly:

- **Package signing.** A catalog index and every release descriptor in it are
  Ed25519-signed, and the installer verifies the signed bytes before parsing
  anything (`platform/packages/src/signing.rs`, `install.rs`, §12). The
  device build never links a code path that can *produce* a signature — the
  `publishing` feature that gates `SecretKey` is off by default and asserted
  absent from the device binary in CI
  (`tools/assert-no-signing-path.sh`) — so a compromised tablet cannot forge a
  release, only a compromised publishing machine or a leaked private key can.
- **Archive extraction.** `.paperpkg` files and platform releases are
  untrusted input until their signature verifies, and the extractor
  (`platform/packages/src/archive.rs`) is written to that assumption: no
  symlinks or device nodes, no path escaping the destination, bounded
  compressed size, uncompressed size, entry count and per-file size. Two
  `cargo-fuzz` targets (`platform/packages/fuzz`) exercise it and the
  `paper.toml` parser directly against arbitrary bytes.

## Reporting a vulnerability

This repository does not currently accept public contributions (see
`CONTRIBUTING.md`), so please report a suspected vulnerability privately
rather than opening a public issue: use GitHub's private vulnerability
reporting on this repository (**Security → Report a vulnerability**), or
contact the maintainer through the profile on
<https://github.com/0x63616c>.

Please include:

- What you found and why it is a vulnerability, not a proposed feature.
- A minimal reproduction — a crafted `paper.toml`, `.paperpkg`, or catalog
  index that demonstrates the issue is ideal, given the surfaces above.
- Whether it requires the attacker to already control a trusted signing key,
  a catalog server, or the LAN — that changes severity a great deal for a
  personal, single-device, private-catalog project (project description).

## Out of scope

- Anything that requires Developer Mode already being enabled on the tablet,
  or physical access to a device that is already unlocked. The standing
  project rule is that Developer Mode is never enabled and firmware is never
  downgraded; a report that assumes otherwise is describing a different
  threat model than this project's.
- Denial of service against a single personal device with no external
  audience.
