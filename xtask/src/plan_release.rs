//! `cargo xtask plan-release` — "what needs publishing?" and nothing else
//! (WWW-61, ADR-0026).
//!
//! Reconciles what is *declared* in the tree — every `apps/<app>/paper.toml`
//! and the platform's own `release.toml` — against what is already
//! *published*, read from the GitHub release list. Not a `git diff`: that
//! trigger is wrong after a force-push, a multi-commit push, a re-run of a
//! red build, or a revert, and declared-vs-published is none of those things
//! — it is idempotent, and publishing nothing is the normal case, not a
//! failure.
//!
//! # The fetch is a seam
//!
//! [`ReleaseSource`] is the only thing here that touches the network — one
//! method, `Vec<PublishedRelease>` out, no opinion about what they mean.
//! [`build_plan`] and everything upstream of it are plain functions over that
//! `Vec`, so every scenario in the acceptance criteria is a fixture, not a
//! network call. [`GithubReleaseSource`] shells out to `curl` rather than
//! adding an HTTP client dependency: `curl` is already how this repository
//! reaches the network from a shell script (`tools/vm-harness`,
//! `tools/cross`), it is on every GitHub Actions runner and every Mac this
//! runs from, and a dependency-free process call is one less thing in the
//! supply chain for a planning step that changes nothing.
//!
//! # What "already published with the same bytes" means before a build ran
//!
//! `plan-release` runs before anything is built (the ticket is explicit:
//! "do not build or sign anything"), so it cannot compare package bytes the
//! way [`paper_packages::publish::Publisher::publish`] does — those do not
//! exist yet. What it *can* read from the tree is the declared manifest
//! itself: `apps/<app>/paper.toml` verbatim, or `release.toml` verbatim for
//! the platform. [`content_digest`] hashes those bytes the way
//! [`paper_packages::Digest`] spells one (`sha256:<hex>`, ADR-0013), and a
//! publish step is expected to record that same digest in the GitHub
//! release's body as a `paperclip-digest: sha256:<hex>` line. A declared
//! version whose tag exists with a matching line is up to date; a declared
//! version whose tag exists with a *different* line — or none at all — is
//! the mistake ADR-0013's "different bytes is refused" already guards
//! against one layer down, surfaced here before a build is wasted on it.
//! Until a later ticket teaches the publish step to write that line, every
//! already-published tag reads as unverifiable and this planner reports it
//! as a conflict rather than silently trusting it — the safe direction for
//! half a contract.
//!
//! # The tag convention
//!
//! Proposed here, not published anywhere yet: apps publish under
//! `<app-id>-v<version>` (e.g. `dev.calum.chess-v0.2.0`), the platform under
//! `platform-v<version>`. A later ticket that writes the actual publish step
//! is what validates it.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Where the catalog apps live, relative to the repository root.
const APPS_DIR: &str = "apps";
/// The app manifest file name inside each `apps/<app>` directory. Not every
/// entry under `apps/` has one — `chess-rules` and `sudoku-rules` are library
/// crates with no `paper.toml` — and those are skipped rather than treated as
/// an error.
const APP_MANIFEST_FILE_NAME: &str = "paper.toml";
/// The platform release manifest, at the repository root (WWW-61,
/// ADR-0026).
const RELEASE_MANIFEST_FILE_NAME: &str = "release.toml";
/// The line a publish step is expected to record in a GitHub release's body,
/// naming the digest of what it declared at publish time.
const DIGEST_LINE_PREFIX: &str = "paperclip-digest: ";

/// Why the plan could not be built.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PlanError {
    /// `apps/` could not be listed.
    #[error("cannot list {path}")]
    ReadDir {
        /// Which directory.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A manifest could not be read.
    #[error("cannot read {path}")]
    Read {
        /// Which file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A manifest was not valid UTF-8.
    #[error("{path} is not valid UTF-8")]
    Utf8 {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::str::Utf8Error,
    },
    /// A manifest did not parse as the expected shape.
    #[error("{path} is not a valid manifest")]
    Syntax {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: toml::de::Error,
    },
    /// A manifest's version was not SemVer.
    #[error("{path}'s version is not SemVer")]
    Version {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: semver::Error,
    },
    /// `curl` could not be run.
    #[error("cannot run curl against {url}")]
    Spawn {
        /// What was fetched.
        url: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// `curl` ran and reported failure.
    #[error("curl against {url} exited with {status}")]
    Fetch {
        /// What was fetched.
        url: String,
        /// The exit status `curl` reported.
        status: std::process::ExitStatus,
    },
    /// The release list did not parse as JSON in the expected shape.
    #[error("the release list from {url} is not the JSON GitHub's API documents")]
    Json {
        /// What was fetched.
        url: String,
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

/// One app declared in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredApp {
    pub id: String,
    pub version: Version,
    pub content_digest: String,
}

/// The platform declared in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredPlatform {
    pub version: Version,
    pub content_digest: String,
}

#[derive(Debug, Deserialize)]
struct RawAppManifest {
    app: RawApp,
}

#[derive(Debug, Deserialize)]
struct RawApp {
    id: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct RawReleaseManifest {
    release: RawReleaseTable,
}

#[derive(Debug, Deserialize)]
struct RawReleaseTable {
    version: String,
}

/// `sha256:<hex>` over `bytes`, spelled the way [`paper_packages::Digest`]
/// spells one (ADR-0013). `xtask` does not depend on that crate for one
/// function's worth of formatting.
fn content_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

/// Reads every `apps/<app>/paper.toml` under `repo_root`, sorted by app id
/// for deterministic output.
pub(crate) fn read_declared_apps(repo_root: &Path) -> Result<Vec<DeclaredApp>, PlanError> {
    let apps_dir = repo_root.join(APPS_DIR);
    let entries = fs::read_dir(&apps_dir).map_err(|source| PlanError::ReadDir {
        path: apps_dir.clone(),
        source,
    })?;

    let mut apps = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| PlanError::ReadDir {
            path: apps_dir.clone(),
            source,
        })?;
        let manifest_path = entry.path().join(APP_MANIFEST_FILE_NAME);
        if !manifest_path.is_file() {
            continue;
        }
        let bytes = fs::read(&manifest_path).map_err(|source| PlanError::Read {
            path: manifest_path.clone(),
            source,
        })?;
        let text = std::str::from_utf8(&bytes).map_err(|source| PlanError::Utf8 {
            path: manifest_path.clone(),
            source,
        })?;
        let raw: RawAppManifest = toml::from_str(text).map_err(|source| PlanError::Syntax {
            path: manifest_path.clone(),
            source,
        })?;
        let version = raw
            .app
            .version
            .parse::<Version>()
            .map_err(|source| PlanError::Version {
                path: manifest_path.clone(),
                source,
            })?;
        apps.push(DeclaredApp {
            id: raw.app.id,
            version,
            content_digest: content_digest(&bytes),
        });
    }
    apps.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(apps)
}

/// Reads `release.toml` at `repo_root`.
pub(crate) fn read_declared_platform(repo_root: &Path) -> Result<DeclaredPlatform, PlanError> {
    let path = repo_root.join(RELEASE_MANIFEST_FILE_NAME);
    let bytes = fs::read(&path).map_err(|source| PlanError::Read {
        path: path.clone(),
        source,
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|source| PlanError::Utf8 {
        path: path.clone(),
        source,
    })?;
    let raw: RawReleaseManifest = toml::from_str(text).map_err(|source| PlanError::Syntax {
        path: path.clone(),
        source,
    })?;
    let version = raw
        .release
        .version
        .parse::<Version>()
        .map_err(|source| PlanError::Version {
            path: path.clone(),
            source,
        })?;
    Ok(DeclaredPlatform {
        version,
        content_digest: content_digest(&bytes),
    })
}

/// One release GitHub already lists. Only the fields the planner reads —
/// GitHub's API carries many more.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PublishedRelease {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub body: Option<String>,
}

/// Somewhere the published release list comes from. Deliberately tiny — a
/// repo in, a `Vec` out — so every test can be a fixture and nothing above
/// this trait needs to know how the bytes arrived.
pub(crate) trait ReleaseSource: fmt::Debug {
    /// Fetches every release GitHub lists for `repo` (`owner/name`).
    fn published_releases(&self, repo: &str) -> Result<Vec<PublishedRelease>, PlanError>;
}

/// The real [`ReleaseSource`]: `curl` against GitHub's public, unauthenticated
/// releases endpoint.
#[derive(Debug)]
pub(crate) struct GithubReleaseSource;

impl ReleaseSource for GithubReleaseSource {
    fn published_releases(&self, repo: &str) -> Result<Vec<PublishedRelease>, PlanError> {
        let url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
        let output = Command::new("curl")
            .args(["-sS", "-f", "-H", "Accept: application/vnd.github+json"])
            .args(["-H", "User-Agent: paperclip-xtask"])
            .arg(&url)
            .output()
            .map_err(|source| PlanError::Spawn {
                url: url.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(PlanError::Fetch {
                url,
                status: output.status,
            });
        }
        serde_json::from_slice(&output.stdout).map_err(|source| PlanError::Json { url, source })
    }
}

/// What a declared version needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    /// No published release names this tag: it needs building and
    /// publishing.
    Publish,
    /// Already published, and its recorded digest matches what is declared
    /// now.
    UpToDate,
    /// Already published under this tag, but the recorded digest does not
    /// match — or was never recorded. A mistake someone needs to see.
    Conflict,
}

/// One line of the plan: what a declared name and version resolves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PlanEntry {
    pub name: String,
    pub version: String,
    pub tag: String,
    pub action: Action,
    pub expected_digest: String,
    pub published_digest: Option<String>,
}

/// The whole answer to "what needs publishing?".
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReleasePlan {
    pub platform: PlanEntry,
    pub apps: Vec<PlanEntry>,
}

impl ReleasePlan {
    /// Every entry, platform first.
    fn entries(&self) -> impl Iterator<Item = &PlanEntry> {
        std::iter::once(&self.platform).chain(self.apps.iter())
    }

    /// Whether anything needs building and publishing.
    pub(crate) fn needs_publishing(&self) -> bool {
        self.entries().any(|entry| entry.action == Action::Publish)
    }

    /// Whether a declared version is already published under different
    /// content — the case that must be surfaced loudly rather than skipped.
    pub(crate) fn has_conflicts(&self) -> bool {
        self.entries().any(|entry| entry.action == Action::Conflict)
    }
}

/// The digest a published release under `tag` recorded, if any — the first
/// `paperclip-digest: sha256:<hex>` line in its body, ignoring drafts.
fn published_digest(tag: &str, published: &[PublishedRelease]) -> Option<Option<String>> {
    let release = published.iter().find(|r| r.tag_name == tag && !r.draft)?;
    Some(
        release
            .body
            .as_deref()
            .and_then(|body| {
                body.lines()
                    .find_map(|line| line.strip_prefix(DIGEST_LINE_PREFIX))
            })
            .map(str::trim)
            .map(str::to_owned),
    )
}

/// Classifies one declared `(name, version)` against `published`.
fn classify(
    name: &str,
    version: &Version,
    tag: String,
    expected_digest: String,
    published: &[PublishedRelease],
) -> PlanEntry {
    let (action, recorded) = match published_digest(&tag, published) {
        None => (Action::Publish, None),
        Some(Some(digest)) if digest == expected_digest => (Action::UpToDate, Some(digest)),
        Some(recorded) => (Action::Conflict, recorded),
    };
    PlanEntry {
        name: name.to_owned(),
        version: version.to_string(),
        tag,
        action,
        expected_digest,
        published_digest: recorded,
    }
}

/// Builds the plan from what is declared against what is published. Pure —
/// every acceptance-criteria scenario is a call to this with a fixture
/// `published` list, no network involved.
pub(crate) fn build_plan(
    apps: &[DeclaredApp],
    platform: &DeclaredPlatform,
    published: &[PublishedRelease],
) -> ReleasePlan {
    let platform_entry = classify(
        "platform",
        &platform.version,
        format!("platform-v{}", platform.version),
        platform.content_digest.clone(),
        published,
    );
    let app_entries = apps
        .iter()
        .map(|app| {
            classify(
                &app.id,
                &app.version,
                format!("{}-v{}", app.id, app.version),
                app.content_digest.clone(),
                published,
            )
        })
        .collect();
    ReleasePlan {
        platform: platform_entry,
        apps: app_entries,
    }
}

/// Reads the tree under `repo_root` and builds the plan against `source`.
pub(crate) fn plan(
    repo_root: &Path,
    repo: &str,
    source: &dyn ReleaseSource,
) -> Result<ReleasePlan, PlanError> {
    let apps = read_declared_apps(repo_root)?;
    let platform = read_declared_platform(repo_root)?;
    let published = source.published_releases(repo)?;
    Ok(build_plan(&apps, &platform, &published))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, version: &str, digest: &str) -> DeclaredApp {
        DeclaredApp {
            id: id.to_owned(),
            version: version.parse().unwrap(),
            content_digest: digest.to_owned(),
        }
    }

    fn platform(version: &str, digest: &str) -> DeclaredPlatform {
        DeclaredPlatform {
            version: version.parse().unwrap(),
            content_digest: digest.to_owned(),
        }
    }

    fn published(tag: &str, digest: &str) -> PublishedRelease {
        PublishedRelease {
            tag_name: tag.to_owned(),
            draft: false,
            body: Some(format!("paperclip-digest: {digest}\n\nRelease notes.")),
        }
    }

    #[test]
    fn nothing_to_do_when_every_declared_version_is_already_published_identically() {
        let apps = [app("dev.calum.chess", "0.2.0", "sha256:aaaa")];
        let platform_manifest = platform("0.1.0", "sha256:bbbb");
        let published = [
            published("dev.calum.chess-v0.2.0", "sha256:aaaa"),
            published("platform-v0.1.0", "sha256:bbbb"),
        ];

        let plan = build_plan(&apps, &platform_manifest, &published);

        assert!(!plan.needs_publishing());
        assert!(!plan.has_conflicts());
        assert_eq!(plan.platform.action, Action::UpToDate);
        assert_eq!(plan.apps[0].action, Action::UpToDate);
    }

    #[test]
    fn one_app_bumped_needs_publishing_and_nothing_else_does() {
        let apps = [
            app("dev.calum.chess", "0.3.0", "sha256:new"),
            app("dev.calum.sudoku", "0.1.0", "sha256:same"),
        ];
        let platform_manifest = platform("0.1.0", "sha256:platform-same");
        let published = [
            published("dev.calum.chess-v0.2.0", "sha256:old"),
            published("dev.calum.sudoku-v0.1.0", "sha256:same"),
            published("platform-v0.1.0", "sha256:platform-same"),
        ];

        let plan = build_plan(&apps, &platform_manifest, &published);

        assert!(plan.needs_publishing());
        assert!(!plan.has_conflicts());
        assert_eq!(plan.apps[0].action, Action::Publish);
        assert_eq!(plan.apps[1].action, Action::UpToDate);
        assert_eq!(plan.platform.action, Action::UpToDate);
    }

    #[test]
    fn several_apps_bumped_are_each_reported() {
        let apps = [
            app("dev.calum.chess", "0.3.0", "sha256:chess-new"),
            app("dev.calum.sudoku", "0.2.0", "sha256:sudoku-new"),
        ];
        let platform_manifest = platform("0.1.0", "sha256:platform-same");
        let published = [published("platform-v0.1.0", "sha256:platform-same")];

        let plan = build_plan(&apps, &platform_manifest, &published);

        assert!(plan.needs_publishing());
        assert_eq!(plan.apps[0].action, Action::Publish);
        assert_eq!(plan.apps[1].action, Action::Publish);
    }

    #[test]
    fn platform_bumped_needs_building_independently_of_apps() {
        let apps = [app("dev.calum.chess", "0.2.0", "sha256:same")];
        let platform_manifest = platform("0.2.0", "sha256:platform-new");
        let published = [
            published("dev.calum.chess-v0.2.0", "sha256:same"),
            published("platform-v0.1.0", "sha256:platform-old"),
        ];

        let plan = build_plan(&apps, &platform_manifest, &published);

        assert!(plan.needs_publishing());
        assert_eq!(plan.platform.action, Action::Publish);
        assert_eq!(plan.apps[0].action, Action::UpToDate);
    }

    #[test]
    fn a_declared_version_already_published_with_identical_content_is_up_to_date() {
        let apps = [app("dev.calum.chess", "0.2.0", "sha256:same")];
        let platform_manifest = platform("0.1.0", "sha256:platform-same");
        let published = [
            published("dev.calum.chess-v0.2.0", "sha256:same"),
            published("platform-v0.1.0", "sha256:platform-same"),
        ];

        let plan = build_plan(&apps, &platform_manifest, &published);

        assert!(!plan.has_conflicts());
        assert_eq!(plan.apps[0].action, Action::UpToDate);
        assert_eq!(
            plan.apps[0].published_digest.as_deref(),
            Some("sha256:same")
        );
    }

    #[test]
    fn a_declared_version_already_published_with_different_content_is_a_conflict_not_a_skip() {
        let apps = [app("dev.calum.chess", "0.2.0", "sha256:declared-now")];
        let platform_manifest = platform("0.1.0", "sha256:platform-same");
        let published = [
            published("dev.calum.chess-v0.2.0", "sha256:published-earlier"),
            published("platform-v0.1.0", "sha256:platform-same"),
        ];

        let plan = build_plan(&apps, &platform_manifest, &published);

        assert!(plan.has_conflicts());
        assert!(!plan.needs_publishing());
        assert_eq!(plan.apps[0].action, Action::Conflict);
        assert_eq!(
            plan.apps[0].published_digest.as_deref(),
            Some("sha256:published-earlier")
        );
    }

    #[test]
    fn a_published_tag_with_no_recorded_digest_is_a_conflict_not_a_silent_trust() {
        let apps = [app("dev.calum.chess", "0.2.0", "sha256:declared-now")];
        let platform_manifest = platform("0.1.0", "sha256:platform-same");
        let published = [
            PublishedRelease {
                tag_name: "dev.calum.chess-v0.2.0".to_owned(),
                draft: false,
                body: None,
            },
            published("platform-v0.1.0", "sha256:platform-same"),
        ];

        let plan = build_plan(&apps, &platform_manifest, &published);

        assert!(plan.has_conflicts());
        assert_eq!(plan.apps[0].action, Action::Conflict);
        assert_eq!(plan.apps[0].published_digest, None);
    }

    #[test]
    fn a_draft_release_does_not_count_as_published() {
        let apps = [app("dev.calum.chess", "0.2.0", "sha256:same")];
        let platform_manifest = platform("0.1.0", "sha256:platform-same");
        let published = [
            PublishedRelease {
                tag_name: "dev.calum.chess-v0.2.0".to_owned(),
                draft: true,
                body: Some("paperclip-digest: sha256:same".to_owned()),
            },
            published("platform-v0.1.0", "sha256:platform-same"),
        ];

        let plan = build_plan(&apps, &platform_manifest, &published);

        assert_eq!(plan.apps[0].action, Action::Publish);
    }

    #[test]
    fn content_digest_is_stable_sha256_lowercase_hex() {
        assert_eq!(
            content_digest(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    fn write_app(dir: &Path, name: &str, id: &str, version: &str) {
        let app_dir = dir.join("apps").join(name);
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(
            app_dir.join(APP_MANIFEST_FILE_NAME),
            format!("[app]\nid = \"{id}\"\nname = \"{name}\"\nversion = \"{version}\"\nprotocol = \"1.0\"\nentrypoint = \"bin/{name}\"\nassets = []\n"),
        )
        .unwrap();
    }

    #[test]
    fn reads_declared_apps_from_a_tree_and_skips_directories_with_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write_app(dir.path(), "chess", "dev.calum.chess", "0.2.0");
        write_app(dir.path(), "home", "dev.calum.home", "0.2.0");
        fs::create_dir_all(dir.path().join("apps/chess-rules")).unwrap();

        let apps = read_declared_apps(dir.path()).unwrap();

        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].id, "dev.calum.chess");
        assert_eq!(apps[1].id, "dev.calum.home");
    }

    #[test]
    fn reads_the_declared_platform_version_and_ignores_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(RELEASE_MANIFEST_FILE_NAME),
            "[release]\nversion = \"0.4.0\"\nprotocol = \"1.0\"\n\n[release.state]\nwrites = 1\nreadable_back_to = 1\n",
        )
        .unwrap();

        let declared = read_declared_platform(dir.path()).unwrap();

        assert_eq!(declared.version, Version::new(0, 4, 0));
    }
}
