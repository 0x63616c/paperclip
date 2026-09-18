//! `paperctl sign-release` — signing a GitHub release off the build machine
//! (§12, WWW-63, ADR-0030).
//!
//! `.github/workflows/release.yml` (WWW-62) builds and publishes app
//! archives and platform components to GitHub, unsigned — CI never holds the
//! signing key. This command is the other half: given a release tag, it
//! downloads what CI published, decides how much of that it can
//! independently vouch for, signs the rest with a local key, and uploads the
//! result back to the same release as new assets. Behind the `publishing`
//! feature with everything else here that can hold a [`SecretKey`], for the
//! reason every one of them is: the device build must not carry a code path
//! that can sign a release, and `tools/assert-no-signing-path.sh` /
//! `just signing-boundary` are what confirm that of the built binary rather
//! than assume it.
//!
//! [`SecretKey`]: paper_packages::signing::SecretKey
//!
//! # Rebuild and compare
//!
//! `.paperpkg` archives are deterministic by construction (ADR-0013): entries
//! sorted, uid/gid/mtime zeroed, modes assigned. That is a property of the
//! archive *format*, not of whatever compiler produced the entrypoint binary
//! packed inside it, and CI links that binary with a different compiler than
//! this repository's own default — `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER
//! =aarch64-linux-gnu-gcc` in `release.yml`, versus `.cargo/config.toml`'s
//! `tools/cross/aarch64-linux-gnu-cc` (`zig cc`), because no Mac this runs on
//! has a real cross GCC on `PATH`. Confirmed 2026-09-18, same commit, same
//! source: the two produce different archive bytes (WWW-63).
//!
//! So by default [`run`] rebuilds the app from source with whatever
//! `aarch64-unknown-linux-gnu` linker is configured in the calling
//! environment, and refuses to sign unless the rebuild byte-matches the
//! archive being signed. On a Mac with no cross GCC — true of both machines
//! this runs on today — that refusal fires on every legitimate release, for
//! the benign reason above, not because anything was tampered with.
//! `--trust-ci-build` is the documented way past that: it skips the rebuild
//! and signs CI's bytes as-is, the trust decision
//! `docs/device/www-69-preflight.md` already recorded by hand for the
//! releases signed before this command existed. See ADR-0030 for why this
//! refuses by default rather than skipping the check quietly.
//!
//! The platform half is never rebuilt: four cross-compiled binaries is a
//! full workspace release build, and the Mac mini this normally runs on is
//! an 8 GB M2 that already swaps under less (WWW-63). [`run`] instead checks
//! the downloaded components against their own `SHA256SUMS` — transit
//! corruption only, not independent verification — and signs what CI built,
//! every time, saying so in its own output.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;
use paper_packages::release::{RELEASE_FILE_NAME, SIGNATURE_SUFFIX};
use paper_packages::{AppId, Digest, MANIFEST_FILE_NAME, Manifest};
use semver::Version;

use crate::error::CommandError;
use crate::packaging::{self, CheckArgs, PackageArgs as AppPackageArgs, PublishArgs};
use crate::release_manifest::{DeclaredRelease, RELEASE_MANIFEST_FILE_NAME};
use crate::upgrade::{self, PackageArgs as PlatformPackageArgs};

/// `owner/name`, matching `cargo xtask plan-release`'s own default.
const DEFAULT_REPO: &str = "0x63616c/paperclip";

/// Signs a GitHub release CI built unsigned.
#[derive(Debug, Args)]
pub(crate) struct SignReleaseArgs {
    /// The release tag, e.g. `dev.calum.chess-v0.2.0` or `platform-v0.1.0`
    /// (ADR-0026's tag convention).
    tag: String,
    /// Where signed app releases are written (ADR-0013's catalog layout).
    /// Required for an app tag.
    #[arg(long)]
    catalog: Option<PathBuf>,
    /// The secret key to sign with.
    #[arg(long)]
    key: PathBuf,
    /// Re-verify the signed result with this public key afterwards.
    #[arg(long)]
    trust: Option<PathBuf>,
    /// The GitHub repository the release lives in.
    #[arg(long, default_value = DEFAULT_REPO)]
    repo: String,
    /// The catalog's name, for an app publish.
    #[arg(long, default_value = packaging::DEFAULT_CATALOG)]
    name: String,
    /// A file holding the release notes, for an app publish.
    #[arg(long)]
    notes_file: Option<PathBuf>,
    /// Use this file instead of downloading the app archive from the
    /// release. For proving the rebuild-and-compare refusal offline; see
    /// this module's own doc comment.
    #[arg(long)]
    archive: Option<PathBuf>,
    /// Where the signed platform bundle is written.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Skip the app rebuild-and-compare and sign CI's archive as published.
    #[arg(long)]
    trust_ci_build: bool,
}

/// What a tag names, read from the tree exactly the way `cargo xtask
/// plan-release` derives the same tag (ADR-0026) — not shared with it
/// directly (`xtask` is a separate binary crate `paperctl` does not depend
/// on), but reusing [`Manifest`] and [`DeclaredRelease`] rather than a second
/// hand-rolled TOML reader.
enum Declared {
    App {
        id: AppId,
        version: Version,
        source_dir: PathBuf,
    },
    Platform {
        version: Version,
    },
}

/// Runs `paperctl sign-release`.
pub(crate) fn run(args: &SignReleaseArgs) -> Result<(), CommandError> {
    let declared = resolve(Path::new("."), &args.tag)?;
    let scratch = Scratch::new()?;

    match declared {
        Declared::App {
            id,
            version,
            source_dir,
        } => {
            println!("release    {} ({id} {version})", args.tag);
            sign_app(args, &id, &version, &source_dir, scratch.path())
        }
        Declared::Platform { version } => {
            println!("release    {} (platform {version})", args.tag);
            sign_platform(args, &version, scratch.path())
        }
    }
}

/// Which declared app or platform release `tag` names, read from `root`
/// (the repository root in production — `run` passes `.`, on the same
/// CWD-relative assumption `upgrade::package`'s own `--release-manifest`
/// default already makes; a parameter here rather than a second hardcoded
/// `.` is what lets this be tested against a fixture without touching the
/// process's real working directory).
fn resolve(root: &Path, tag: &str) -> Result<Declared, CommandError> {
    if let Some(rest) = tag.strip_prefix("platform-v") {
        let declared = DeclaredRelease::read(&root.join(RELEASE_MANIFEST_FILE_NAME))?;
        if declared.version.to_string() != rest {
            return Err(CommandError::SignRelease {
                detail: format!(
                    "{tag} names platform version {rest}, but {RELEASE_MANIFEST_FILE_NAME} \
                     declares {}",
                    declared.version
                ),
            });
        }
        return Ok(Declared::Platform {
            version: declared.version,
        });
    }

    let apps_dir = root.join("apps");
    let apps_dir = apps_dir.as_path();
    let entries = fs::read_dir(apps_dir).map_err(|source| CommandError::Read {
        path: apps_dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CommandError::Read {
            path: apps_dir.to_path_buf(),
            source,
        })?;
        let source_dir = entry.path();
        if !source_dir.join(MANIFEST_FILE_NAME).is_file() {
            continue;
        }
        let manifest =
            Manifest::read_package(&source_dir).map_err(|source| CommandError::Manifest {
                path: source_dir.join(MANIFEST_FILE_NAME),
                source,
            })?;
        if format!("{}-v{}", manifest.id(), manifest.version()) == tag {
            return Ok(Declared::App {
                id: manifest.id().clone(),
                version: manifest.version().clone(),
                source_dir,
            });
        }
    }
    Err(CommandError::SignRelease {
        detail: format!(
            "no declared release matches tag {tag} (is {RELEASE_MANIFEST_FILE_NAME}, or an \
             app's {MANIFEST_FILE_NAME}, out of date with what was published?)"
        ),
    })
}

fn sign_app(
    args: &SignReleaseArgs,
    id: &AppId,
    version: &Version,
    source_dir: &Path,
    scratch: &Path,
) -> Result<(), CommandError> {
    let catalog = args
        .catalog
        .clone()
        .ok_or_else(|| CommandError::SignRelease {
            detail: "--catalog is required for an app release".to_owned(),
        })?;

    let archive_name = format!("{id}-{version}.paperpkg");
    let archive = scratch.join(&archive_name);
    match &args.archive {
        Some(path) => {
            fs::copy(path, &archive).map_err(|source| CommandError::Write {
                path: archive.clone(),
                source,
            })?;
        }
        None => download(&args.tag, &args.repo, scratch, &[&archive_name])?,
    }
    println!(
        "archive    {} ({})",
        archive.display(),
        Digest::of_bytes(&read(&archive)?)
    );
    packaging::check(&CheckArgs {
        target: archive.clone(),
        trust: None,
    })?;

    if args.trust_ci_build {
        println!(
            "rebuild    skipped (--trust-ci-build): signing CI's bytes as-is, not independently rebuilt"
        );
    } else {
        rebuild_and_compare(&archive, source_dir, scratch)?;
    }

    packaging::publish(&PublishArgs {
        package: archive,
        catalog: catalog.clone(),
        key: args.key.clone(),
        name: args.name.clone(),
        notes_file: args.notes_file.clone(),
    })?;

    if let Some(trust) = &args.trust {
        packaging::check(&CheckArgs {
            target: catalog.clone(),
            trust: Some(trust.clone()),
        })?;
    }

    let release_dir = catalog
        .join("apps")
        .join(id.to_string())
        .join(version.to_string());
    let release_toml = release_dir.join(RELEASE_FILE_NAME);
    let signature = release_dir.join(format!("{RELEASE_FILE_NAME}{SIGNATURE_SUFFIX}"));
    for path in [&release_toml, &signature] {
        if !path.is_file() {
            return Err(CommandError::SignRelease {
                detail: format!("publish did not write {}", path.display()),
            });
        }
    }
    run_checked(
        "gh",
        [
            "release",
            "upload",
            &args.tag,
            &path_arg(&release_toml)?,
            &path_arg(&signature)?,
            "--repo",
            &args.repo,
            "--clobber",
        ],
    )?;
    println!(
        "uploaded   {RELEASE_FILE_NAME}, {RELEASE_FILE_NAME}{SIGNATURE_SUFFIX} -> {}",
        args.tag
    );
    Ok(())
}

/// Rebuilds `source_dir` and refuses to continue unless the result
/// byte-matches `archive`. See this module's own doc comment for what
/// "matches" depends on, and why a mismatch here is not necessarily
/// tampering.
fn rebuild_and_compare(
    archive: &Path,
    source_dir: &Path,
    scratch: &Path,
) -> Result<(), CommandError> {
    let app_dir_name = source_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CommandError::SignRelease {
            detail: format!(
                "{} is not a usable app directory name",
                source_dir.display()
            ),
        })?;
    let linker =
        std::env::var("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER").unwrap_or_else(|_| {
            "(repository default: tools/cross/aarch64-linux-gnu-cc, zig cc)".to_owned()
        });
    println!("rebuild    {} (linker={linker})", source_dir.display());
    run_checked("./tools/package-app.sh", [app_dir_name])?;

    let rebuilt = scratch.join("rebuilt.paperpkg");
    packaging::package(&AppPackageArgs {
        source: source_dir.to_path_buf(),
        out: Some(rebuilt.clone()),
    })?;

    let expected = Digest::of_bytes(&read(archive)?);
    let got = Digest::of_bytes(&read(&rebuilt)?);
    if expected != got {
        return Err(CommandError::SignRelease {
            detail: format!(
                "rebuilt archive does not match {}\n\
                 \x20 published (signing this): {expected}\n\
                 \x20 rebuilt here:              {got}\n\
                 If this is every legitimate release, not just this one, the likely cause \
                 is a toolchain difference from CI, not tampering (this module's own doc \
                 comment, and WWW-63). Either export \
                 CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc with a \
                 matching cross toolchain installed and rerun, or pass --trust-ci-build to \
                 sign this archive unverified and record that decision in your result.",
                archive.display()
            ),
        });
    }
    println!("rebuild    matches CI's bytes ({got})");
    Ok(())
}

fn sign_platform(
    args: &SignReleaseArgs,
    version: &Version,
    scratch: &Path,
) -> Result<(), CommandError> {
    download(
        &args.tag,
        &args.repo,
        scratch,
        &["platform-components.tar.gz", "SHA256SUMS"],
    )?;

    let components = scratch.join("components");
    fs::create_dir_all(&components).map_err(|source| CommandError::Write {
        path: components.clone(),
        source,
    })?;
    run_checked(
        "tar",
        [
            "xzf",
            &path_arg(&scratch.join("platform-components.tar.gz"))?,
            "-C",
            &path_arg(&components)?,
        ],
    )?;

    println!(
        "rebuild    skipped: full platform components are a workspace release build, not \
         attempted on this machine (WWW-63, ADR-0030)"
    );
    println!(
        "checked    platform-components.tar.gz against its own SHA256SUMS (transit only, \
         not an independent rebuild — CI's binaries are trusted as declared)"
    );
    verify_sha256sums(&scratch.join("SHA256SUMS"), &components)?;

    let bundle = args
        .out
        .clone()
        .unwrap_or_else(|| scratch.join(format!("paperclip-{version}.tar.gz")));
    upgrade::package(&PlatformPackageArgs {
        source: components,
        release_manifest: PathBuf::from(RELEASE_MANIFEST_FILE_NAME),
        version: None,
        protocol: None,
        state_version: None,
        rollback_to_state: None,
        notes: String::new(),
        include: Vec::new(),
        key: args.key.clone(),
        out: bundle.clone(),
    })?;

    run_checked(
        "gh",
        [
            "release",
            "upload",
            &args.tag,
            &path_arg(&bundle)?,
            "--repo",
            &args.repo,
            "--clobber",
        ],
    )?;
    println!(
        "uploaded   {} -> {}",
        bundle.file_name().map_or_else(
            || bundle.display().to_string(),
            |name| name.to_string_lossy().into_owned()
        ),
        args.tag
    );
    Ok(())
}

/// Checks every file `sums_path` lists, relative to `base`, against its own
/// recorded SHA-256 — the same format `shasum -a 256` writes (`<hex>␠␠<path>`
/// per line). Catches corruption in transit; says nothing about who produced
/// the bytes, which is exactly the limit this module's doc comment names for
/// the platform half.
fn verify_sha256sums(sums_path: &Path, base: &Path) -> Result<(), CommandError> {
    let text = fs::read_to_string(sums_path).map_err(|source| CommandError::Read {
        path: sums_path.to_path_buf(),
        source,
    })?;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let malformed = || CommandError::SignRelease {
            detail: format!(
                "{} has a line that is not `<hex> <path>`",
                sums_path.display()
            ),
        };
        let expected = fields.next().ok_or_else(malformed)?;
        let relative = fields.next().ok_or_else(malformed)?;
        let file_path = base.join(relative);
        let got = Digest::of_bytes(&read(&file_path)?).to_hex();
        if got != expected {
            return Err(CommandError::SignRelease {
                detail: format!(
                    "{} does not match {}: expected {expected}, got {got}",
                    file_path.display(),
                    sums_path.display()
                ),
            });
        }
        println!("{relative}: OK");
    }
    Ok(())
}

/// `gh release download <tag> --dir <scratch> --clobber`, one `--pattern` per
/// name in `names`.
fn download(tag: &str, repo: &str, scratch: &Path, names: &[&str]) -> Result<(), CommandError> {
    let mut argv = vec![
        "release".to_owned(),
        "download".to_owned(),
        tag.to_owned(),
        "--repo".to_owned(),
        repo.to_owned(),
        "--dir".to_owned(),
        path_arg(scratch)?,
        "--clobber".to_owned(),
    ];
    for name in names {
        argv.push("--pattern".to_owned());
        argv.push((*name).to_owned());
    }
    run_checked("gh", argv.iter().map(String::as_str))
}

/// Runs `program` with `args`, inheriting this process's stdio (so `gh`'s own
/// download/upload progress is visible), and turns a non-zero exit into a
/// [`CommandError::SignRelease`].
fn run_checked<'a>(
    program: &str,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<(), CommandError> {
    let args: Vec<&str> = args.into_iter().collect();
    let status = Command::new(program)
        .args(&args)
        .status()
        .map_err(|source| CommandError::SignRelease {
            detail: format!("cannot run `{program} {}`: {source}", args.join(" ")),
        })?;
    if !status.success() {
        return Err(CommandError::SignRelease {
            detail: format!("`{program} {}` exited with {status}", args.join(" ")),
        });
    }
    Ok(())
}

fn path_arg(path: &Path) -> Result<String, CommandError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| CommandError::SignRelease {
            detail: format!("{} is not valid UTF-8", path.display()),
        })
}

fn read(path: &Path) -> Result<Vec<u8>, CommandError> {
    fs::read(path).map_err(|source| CommandError::Read {
        path: path.to_path_buf(),
        source,
    })
}

/// A scratch directory, removed on drop the way the shell scripts elsewhere
/// in this tree use `trap 'rm -rf ...' EXIT`.
struct Scratch(PathBuf);

/// Disambiguates [`Scratch`] directories created within one process — a
/// process id alone is not enough: the test binary is one process that
/// creates many.
static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Scratch {
    fn new() -> Result<Self, CommandError> {
        let unique = SCRATCH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "paperctl-sign-release-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).map_err(|source| CommandError::Write {
            path: dir.clone(),
            source,
        })?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256sums_accepts_a_file_that_matches_its_own_recorded_digest() {
        let scratch = Scratch::new().unwrap();
        let base = scratch.path();
        fs::write(base.join("bin"), b"hello").unwrap();
        let digest = Digest::of_bytes(b"hello").to_hex();
        fs::write(base.join("SHA256SUMS"), format!("{digest}  bin\n")).unwrap();

        verify_sha256sums(&base.join("SHA256SUMS"), base).unwrap();
    }

    #[test]
    fn sha256sums_refuses_a_file_that_does_not_match() {
        let scratch = Scratch::new().unwrap();
        let base = scratch.path();
        fs::write(base.join("bin"), b"tampered").unwrap();
        let digest = Digest::of_bytes(b"hello").to_hex();
        fs::write(base.join("SHA256SUMS"), format!("{digest}  bin\n")).unwrap();

        assert!(matches!(
            verify_sha256sums(&base.join("SHA256SUMS"), base),
            Err(CommandError::SignRelease { .. })
        ));
    }

    /// The repository root — `cargo test -p paperctl` runs with the crate
    /// directory (`tools/paperctl`) as the working directory, not the
    /// workspace root `resolve`'s production caller assumes, so tests give
    /// it an explicit root instead.
    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
    }

    #[test]
    fn resolve_matches_the_platform_tag_against_release_toml() {
        // release.toml declares whatever version is currently checked in —
        // read it back rather than hardcoding one, so this test does not
        // need updating every time a release is cut.
        let root = repo_root();
        let declared = DeclaredRelease::read(&root.join(RELEASE_MANIFEST_FILE_NAME)).unwrap();
        let tag = format!("platform-v{}", declared.version);

        assert!(matches!(
            resolve(&root, &tag),
            Ok(Declared::Platform { version }) if version == declared.version
        ));
    }

    #[test]
    fn resolve_refuses_a_platform_tag_at_the_wrong_version() {
        assert!(matches!(
            resolve(&repo_root(), "platform-v0.0.0-this-will-never-be-real"),
            Err(CommandError::SignRelease { .. })
        ));
    }

    #[test]
    fn resolve_matches_an_app_tag_against_its_paper_toml() {
        let root = repo_root();
        let manifest = Manifest::read_package(&root.join("apps/chess")).unwrap();
        let tag = format!("{}-v{}", manifest.id(), manifest.version());

        match resolve(&root, &tag).unwrap() {
            Declared::App { id, source_dir, .. } => {
                assert_eq!(id, *manifest.id());
                assert_eq!(source_dir, root.join("apps/chess"));
            }
            Declared::Platform { .. } => panic!("apps/chess resolved as the platform"),
        }
    }

    #[test]
    fn resolve_refuses_an_unknown_tag() {
        assert!(matches!(
            resolve(&repo_root(), "dev.calum.nonexistent-v9.9.9"),
            Err(CommandError::SignRelease { .. })
        ));
    }
}
