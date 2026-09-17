# The app contract

What an app must provide to be installable, and what the platform promises in
return. Stage 1 defines the manifest half; the runtime half (lifecycle, input
and rendering messages) arrives in Stage 6.

## `paper.toml`

Every package has one, at its root.

```toml
[app]
id = "dev.calum.chess"       # stable, reverse-DNS, lowercase
name = "Chess"                # what the shelf shows
version = "0.1.0"             # SemVer, required by §4
protocol = "1.0"              # the platform contract this was built against
entrypoint = "bin/chess"      # relative to the package root
assets = []                   # files the package promises to ship
```

### Field rules

| Field | Rule |
|---|---|
| `id` | Two or more dot-separated segments. Each starts with a lowercase letter and contains only `a–z`, `0–9` and `-`, and does not end with `-`. At most 128 bytes. Safe unescaped as a directory name, a unit name and a URL path segment. |
| `name` | Trimmed, non-empty, at most 48 characters, no control characters. |
| `version` | Valid SemVer, including pre-release and build metadata. |
| `protocol` | `major.minor`. Not SemVer: a protocol either changed shape or it did not. |
| `entrypoint` | Package-relative. No leading `/`, no `..`, no `.` component, no `\`, no `~`, no NUL, at most 512 bytes. |
| `assets` | Same path rules as `entrypoint`. No duplicates. Optional; defaults to empty. |

Anything else in the file is an error. There is no "ignored for forward
compatibility" tier, because a key silently ignored is a key the author
believes is working.

### Validation happens in two steps

`Manifest::parse` decides everything readable from the text. It deliberately
does **not** check protocol compatibility, because the App Store has to be able
to display an app this device cannot run.

- `Manifest::ensure_runnable()` — asks whether this platform build can run it.
- `Manifest::validate_payload(root)` — asks whether the entrypoint and every
  declared asset is actually present.

Both are called by `paperctl manifest validate --payload`.

## Capabilities are not part of the manifest

An app cannot grant itself anything. This is enforced by types, not by review:

- `Manifest` has no capability field.
- A `capabilities`, `permissions` or `grants` key in a `paper.toml` is a
  dedicated parse error — `ManifestError::SelfGrantedCapabilities` — whose
  message says the decision is not the app's to make.
- `GrantedCapabilities` implements neither `Deserialize` nor `Default` and has
  no public constructor, so no file on disk can produce one.
- The only source of a `GrantedCapabilities` is `InstallPolicy::grant`, and the
  only way to pair one with a manifest is `InstalledApp::install`, which
  requires a policy.

`InstallPolicy::deny_all()` is the starting point. Capabilities are added to it
by the host, per app id.

Current capabilities: `storage`, `network`, `sharing`. The list grows when the
host gains the enforcement point that goes with a new entry, not before — a
capability nothing checks is a comment pretending to be a type.

## Protocol compatibility

A host may be newer than the app it runs, never older:

```
host.can_run(app)  ⟺  host.major == app.major && host.minor >= app.minor
```

An app needing `1.3` will not start on a `1.1` host, because the messages it
expects do not exist there.
