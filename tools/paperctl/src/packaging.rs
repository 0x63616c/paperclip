//! `paperctl key`, `package`, `publish`, `check` — the publishing side (§12).
//!
//! Everything here runs on the Mac. The secret key never leaves it, and these
//! are the only commands that touch one.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use paper_packages::archive::{self, ARCHIVE_EXTENSION, ArchiveLimits};
use paper_packages::publish::Publisher;
use paper_packages::signing::{PublicKey, SecretKey, TrustedKeys};
use paper_packages::store;
use paper_packages::{MANIFEST_FILE_NAME, Manifest, ObjectKind, PackageCheck};

use crate::error::CommandError;
use crate::install::read_text;

/// The default name of a personal catalog.
pub(crate) const DEFAULT_CATALOG: &str = "calum-home";

/// Key management.
#[derive(Debug, Subcommand)]
pub(crate) enum KeyCommand {
    /// Generate a publishing key pair.
    Generate(GenerateArgs),
    /// Print a public key's id.
    Show {
        /// A public or secret key file.
        path: PathBuf,
    },
}

/// Where to write a new key pair.
#[derive(Debug, Args)]
pub(crate) struct GenerateArgs {
    /// Directory to write `paperclip.key` and `paperclip.pub` into.
    #[arg(long, default_value = ".")]
    out_dir: PathBuf,
    /// Overwrite an existing key.
    ///
    /// Off by default, and worth leaving off: overwriting a signing key makes
    /// every release already published unverifiable on any device that trusts
    /// the old one.
    #[arg(long)]
    force: bool,
}

/// Build a `.paperpkg` from a package directory.
#[derive(Debug, Args)]
pub(crate) struct PackageArgs {
    /// The package directory, containing `paper.toml`.
    pub(crate) source: PathBuf,
    /// Where to write the archive. Defaults to `<id>-<version>.paperpkg`.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

/// Publish a package into a catalog.
#[derive(Debug, Args)]
pub(crate) struct PublishArgs {
    /// The `.paperpkg` to publish.
    pub(crate) package: PathBuf,
    /// The catalog directory. Created if it does not exist.
    #[arg(long)]
    pub(crate) catalog: PathBuf,
    /// The secret key to sign with.
    #[arg(long)]
    pub(crate) key: PathBuf,
    /// The catalog's name. Serials are only comparable within one name.
    #[arg(long, default_value = DEFAULT_CATALOG)]
    pub(crate) name: String,
    /// A file holding the release notes.
    #[arg(long)]
    pub(crate) notes_file: Option<PathBuf>,
}

/// Verify a catalog, or a single package.
#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    /// A catalog directory, a package source directory, or a `.paperpkg` file.
    pub(crate) target: PathBuf,
    /// The public key a catalog must be signed with.
    #[arg(long)]
    pub(crate) trust: Option<PathBuf>,
}

/// Runs a key subcommand.
pub(crate) fn key(command: KeyCommand) -> Result<(), CommandError> {
    match command {
        KeyCommand::Generate(args) => generate(&args),
        KeyCommand::Show { path } => show(&path),
    }
}

fn generate(args: &GenerateArgs) -> Result<(), CommandError> {
    let secret_path = args.out_dir.join("paperclip.key");
    let public_path = args.out_dir.join("paperclip.pub");
    if !args.force && secret_path.exists() {
        return Err(CommandError::KeyExists { path: secret_path });
    }
    std::fs::create_dir_all(&args.out_dir).map_err(|source| CommandError::Write {
        path: args.out_dir.clone(),
        source,
    })?;

    let secret = SecretKey::generate()?;
    write_secret(&secret_path, &secret.to_armoured())?;
    write_file(&public_path, secret.public_key().to_armoured().as_bytes())?;

    println!("key id     {}", secret.public_key().id());
    println!(
        "secret     {} (keep on this machine)",
        secret_path.display()
    );
    println!("public     {} (copy to the tablet)", public_path.display());
    Ok(())
}

/// Writes a secret key with an owner-only mode, set before any bytes land.
#[cfg(unix)]
fn write_secret(path: &Path, armoured: &str) -> Result<(), CommandError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        // 0o600 at creation, not a `chmod` afterwards: between a default-mode
        // create and a later chmod there is a window in which the key is
        // world-readable, and a key that was ever readable is a key to replace.
        .mode(0o600)
        .open(path)
        .map_err(|source| CommandError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(armoured.as_bytes())
        .map_err(|source| CommandError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn write_secret(path: &Path, armoured: &str) -> Result<(), CommandError> {
    write_file(path, armoured.as_bytes())
}

fn show(path: &Path) -> Result<(), CommandError> {
    let text = read_text(path)?;
    if let Ok(public) = text.parse::<PublicKey>() {
        println!("public key {}", public.id());
        return Ok(());
    }
    let secret: SecretKey = text.parse()?;
    println!("secret key {} (public half)", secret.public_key().id());
    Ok(())
}

/// Runs `paperctl package`.
pub(crate) fn package(args: &PackageArgs) -> Result<(), CommandError> {
    let manifest =
        Manifest::read_package(&args.source).map_err(|source| CommandError::Manifest {
            path: args.source.join(paper_packages::MANIFEST_FILE_NAME),
            source,
        })?;
    let out = args.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "{}-{}.{ARCHIVE_EXTENSION}",
            manifest.id(),
            manifest.version()
        ))
    });

    let mut bytes = Vec::new();
    let built = archive::build(&args.source, &mut bytes)?;
    write_file(&out, &bytes)?;

    println!(
        "app        {} {}",
        built.manifest.id(),
        built.manifest.version()
    );
    println!(
        "files      {} ({} bytes uncompressed)",
        built.files, built.bytes
    );
    println!("archive    {} ({} bytes)", out.display(), bytes.len());
    println!("digest     {}", paper_packages::Digest::of_bytes(&bytes));
    Ok(())
}

/// Runs `paperctl publish`.
pub(crate) fn publish(args: &PublishArgs) -> Result<(), CommandError> {
    let secret: SecretKey = read_text(&args.key)?.parse()?;
    let notes = match &args.notes_file {
        Some(path) => read_text(path)?,
        None => String::new(),
    };
    let publisher = Publisher::open(&args.catalog, &args.name)?;
    let published = publisher.publish(&args.package, &secret, notes.trim(), store::now())?;

    println!(
        "published  {} {}{}",
        published.app,
        published.version,
        if published.already_published {
            " (already published, unchanged)"
        } else {
            ""
        }
    );
    println!("digest     {}", published.digest);
    println!("size       {} bytes", published.size);
    println!("archive    {}", published.archive);
    println!("catalog    {} serial {}", args.name, published.serial);
    println!("signed by  {}", secret.public_key().id());
    Ok(())
}

/// Runs `paperctl check`.
pub(crate) fn check(args: &CheckArgs) -> Result<(), CommandError> {
    if args.target.is_file() {
        return check_package(&args.target);
    }
    // A directory with a `paper.toml` in it is a package source, not a
    // catalog. Checking one is the package-time layer of the app contract
    // (§8), and it answers a different question from the two below: "could
    // this run", rather than "who vouched for these bytes". A source tree has
    // nobody vouching for it yet, which is why it is checked before it is
    // packaged rather than after.
    if args.target.join(MANIFEST_FILE_NAME).is_file() {
        return check_source(&args.target);
    }
    let path = args.trust.clone().ok_or(CommandError::TrustRequired)?;
    let mut keys = TrustedKeys::none();
    keys.trust(read_text(&path)?.parse::<PublicKey>()?);

    let publisher = Publisher::open(&args.target, DEFAULT_CATALOG)?;
    let report = publisher.check(&keys)?;
    println!("catalog    {} serial {}", report.catalog, report.serial);
    for release in &report.verified {
        println!("verified   {release}");
    }
    if report.verified.is_empty() {
        println!("verified   nothing; the catalog is empty");
    }
    Ok(())
}

/// Runs the package-time conformance layer over a package source directory.
///
/// Everything decidable before anything runs it: the manifest, the protocol it
/// asks for, its payload, its sizes, and the entrypoint's ELF header — the
/// only check that distinguishes a program from a shell script, and the one
/// that catches a binary packaged from the Mac's own `cargo build` instead of
/// the cross-compiled one.
///
/// It says nothing about authenticity and has no way to. The bytes a signature
/// covers are the archive's, which is what the other two branches of `check`
/// are about.
fn check_source(root: &Path) -> Result<(), CommandError> {
    let check = PackageCheck::run(root).map_err(|source| CommandError::PackageSource {
        path: root.to_path_buf(),
        source: Box::new(source),
    })?;

    let manifest = check.manifest();
    println!("source     {}", root.display());
    println!("id         {}", manifest.id());
    println!("name       {}", manifest.name());
    println!("version    {}", manifest.version());
    println!(
        "protocol   {} (platform speaks {})",
        manifest.protocol(),
        paper_protocol::CURRENT
    );
    println!("entrypoint {}", manifest.entrypoint());
    println!(
        "target     aarch64 ELF, {}",
        match check.target().kind {
            ObjectKind::Executable => "fixed-position executable",
            ObjectKind::SharedObject => "position-independent executable",
            // `ObjectKind` is `#[non_exhaustive]`; a kind this build has no
            // name for is still one `require_device_entrypoint` accepted.
            _ => "executable",
        }
    );
    println!("assets     {}", manifest.assets().len());
    println!("size       {} bytes", check.total_bytes());
    println!(
        "signature  none \u{2014} a source tree is not a published thing; `paperctl publish` signs the archive"
    );
    Ok(())
}

/// Opens a package the way a device would, and reports what is inside.
///
/// Unpacked into a scratch directory that is removed afterwards. Checking a
/// package by looking at it is how a hostile one gets in; this runs the same
/// extraction the tablet would.
fn check_package(path: &Path) -> Result<(), CommandError> {
    let bytes = std::fs::read(path).map_err(|source| CommandError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let scratch = path.with_extension("paperclip-check");
    store::remove_tree(&scratch)?;
    store::create_dir_if_missing(&scratch)?;
    let extracted = archive::extract(&bytes[..], &scratch, ArchiveLimits::DEFAULT);
    store::remove_tree(&scratch)?;
    let extracted = extracted?;

    println!(
        "app        {} {}",
        extracted.manifest.id(),
        extracted.manifest.version()
    );
    println!("protocol   {}", extracted.manifest.protocol());
    println!("entrypoint {}", extracted.manifest.entrypoint());
    println!(
        "files      {} ({} bytes uncompressed)",
        extracted.files, extracted.bytes
    );
    println!("size       {} bytes", bytes.len());
    println!("digest     {}", paper_packages::Digest::of_bytes(&bytes));
    match extracted.manifest.ensure_runnable() {
        Ok(()) => println!(
            "runnable   yes (platform speaks {})",
            paper_protocol::CURRENT
        ),
        Err(_) => println!(
            "runnable   no; built for protocol {} and this platform speaks {}",
            extracted.manifest.protocol(),
            paper_protocol::CURRENT
        ),
    }
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), CommandError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|source| CommandError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, bytes).map_err(|source| CommandError::Write {
        path: path.to_path_buf(),
        source,
    })
}
