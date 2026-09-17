# ADR-0003 — `paper.toml` shape, and capabilities as install policy

**Status:** accepted (Stage 1, WWW-2)

## Context

§8 and §12 require a package manifest carrying a stable app id, a display name,
a valid SemVer version, a relative entrypoint, declared assets and a supported
protocol version — with typed errors. §12 also states the rule this ADR mostly
exists for: **a manifest cannot grant itself capabilities.** Capability grants
are installation policy, not manifest content, and that must be encoded in the
types rather than asserted in a comment.

## Decision

### Shape

A single `[app]` table, TOML, with `deny_unknown_fields`. Flat, because nothing
in it needs nesting yet and a table added speculatively is a table someone will
populate incorrectly.

Validation splits in two, because they answer different questions at different
times:

- `Manifest::parse` — everything decidable from the text.
- `Manifest::ensure_runnable` — protocol compatibility with *this* build.
- `Manifest::validate_payload(root)` — entrypoint and assets present on disk.

`parse` deliberately accepts a protocol this platform cannot run. The App Store
has to display an app that will not install here, and refusing to parse it
would make that impossible.

Paths are a `RelativePath` newtype validated once, at parse time: no absolute
paths, no `..`, no `.` component, no `\`, no `~`, no NUL. Rejected, not
normalised — silently rewriting a path an author wrote means shipping something
they did not ask for.

### Capabilities

Four properties, together:

1. `Manifest` has no capability field.
2. `capabilities`, `permissions` and `grants` keys produce
   `ManifestError::SelfGrantedCapabilities`, a dedicated variant whose message
   says the decision is not the app's to make — rather than a generic
   "unknown field", which would read as a typo.
3. `GrantedCapabilities` implements neither `Deserialize` nor `Default` and has
   no public constructor. Nothing on disk can produce one.
4. `InstallPolicy::grant` is the only source of a `GrantedCapabilities`, and
   `InstalledApp::install` is the only way to pair one with a manifest. Both
   require a policy to be present.

`InstallPolicy::deny_all()` is the starting point.

## Consequences

- The rule cannot be violated by a future contributor without deleting code
  that visibly exists to stop them.
- A test asserts it end-to-end against the manifests the apps really ship:
  `no_shipped_app_can_grant_itself_a_capability` forges a `capabilities` key
  into each and checks the parse fails.
- The capability list (`storage`, `network`, `sharing`) is small and closed,
  and grows only when the host gains the enforcement point that goes with a new
  entry. A capability nothing checks is a comment pretending to be a type.
- `deny_unknown_fields` means there is no forward-compatibility tier. A future
  key needs a version bump or a migration. That is the intended trade: a key
  silently ignored is a key the author believes is working.

## What would make this wrong

Nothing WWW-1 can report. If §12's capability model itself changes, this ADR is
superseded.
