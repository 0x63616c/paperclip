//! `paperctl new app` — scaffold a working app from `sdk-examples/counter`
//! (WWW-51).
//!
//! Copies that crate into `apps/<name>`, substitutes the new app's own
//! identity into every file, and registers the crate as a workspace member
//! so `cargo build -p paper-<name>` works the moment this returns. The
//! template is not a separate, hand-maintained string embedded in this
//! binary: it is a real, tested crate that ships in this checkout, so it
//! cannot bit-rot the way a template only `paperctl` itself ever reads
//! would.
//!
//! What a fresh app gets, unmodified from the template: `layout`/`draw`
//! split as pure functions (no `layout: Option<Layout>`, no canvas-only
//! hit-testing — see the crate's own doc), an `App` impl that persists one
//! field, a `[[bin]]` entrypoint per ADR-0022, an entrypoint test that spawns
//! the built binary, and a `--features preview --example preview` that opens
//! it in a real window on the Mac.

use std::fs;
use std::path::{Path, PathBuf};

use paper_packages::{AppId, Manifest};

use crate::error::CommandError;

/// The template every scaffolded app is copied from.
const TEMPLATE_DIR: &str = "sdk-examples/counter";

/// Files copied from the template, relative to its root. Not a directory
/// walk: an explicit list is what keeps a stray file under
/// `sdk-examples/counter` (a `target/` from a local build, say) from ending
/// up in every app scaffolded after it.
const TEMPLATE_FILES: &[&str] = &[
    "Cargo.toml",
    "paper.toml",
    "src/lib.rs",
    "src/app.rs",
    "src/screen.rs",
    "src/main.rs",
    "examples/preview.rs",
    "tests/entrypoint.rs",
];

/// `paperctl new` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum NewCommand {
    /// Scaffold a new app from `sdk-examples/counter`.
    App(NewAppArgs),
}

/// Runs a `paperctl new` subcommand.
pub(crate) fn run_command(command: NewCommand) -> Result<(), CommandError> {
    match command {
        NewCommand::App(args) => run(&args),
    }
}

/// `paperctl new app`.
#[derive(Debug, clap::Args)]
pub(crate) struct NewAppArgs {
    /// The app's name — lowercase letters, digits and `-`, starting with a
    /// letter. Becomes `dev.calum.<name>`, `paper-<name>`, `apps/<name>` and
    /// the `[[bin]]`/entrypoint name, all at once, so this is the one
    /// question this command asks.
    name: String,
}

/// Why `paperctl new app` could not scaffold one.
#[derive(Debug, thiserror::Error)]
pub(crate) enum NewAppError {
    /// The name given is not a valid app id component.
    #[error("`{name}` cannot be part of an app id (dev.calum.{name})")]
    InvalidName {
        /// What was typed.
        name: String,
        /// Why.
        #[source]
        source: paper_packages::IdError,
    },
    /// Something already exists where the new app would go.
    #[error("{path} already exists")]
    AlreadyExists {
        /// Where.
        path: PathBuf,
    },
    /// A template file could not be read.
    #[error("cannot read the template file {path}")]
    ReadTemplate {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The scaffolded app could not be written.
    #[error("cannot write {path}")]
    Write {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The new crate could not be registered as a workspace member.
    #[error("cannot register apps/{name} in the workspace root Cargo.toml")]
    RegisterMember {
        /// The app name that could not be registered.
        name: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The workspace root `Cargo.toml`'s `members` list does not look the way
    /// this command expects to edit it.
    #[error(
        "cannot find the apps/render-test-card line in the workspace Cargo.toml \
         to add apps/{name} next to it; add it by hand"
    )]
    MembersListShapeChanged {
        /// The app name that still needs adding by hand.
        name: String,
    },
    /// The manifest this command just wrote does not parse.
    ///
    /// A bug in this command, not in anything the user typed — the template
    /// is a real, tested manifest, so a substitution above must have broken
    /// it.
    #[error("the scaffolded app's own paper.toml does not parse; this is a bug in paperctl")]
    ScaffoldedManifest(#[source] paper_packages::ManifestError),
}

/// Runs `paperctl new app <name>`.
pub(crate) fn run(args: &NewAppArgs) -> Result<(), CommandError> {
    let name = &args.name;
    validate_name(name)?;

    let workspace = workspace_root();
    let template = workspace.join(TEMPLATE_DIR);
    let destination = workspace.join("apps").join(name);
    if destination.exists() {
        return Err(NewAppError::AlreadyExists { path: destination }.into());
    }

    scaffold(&template, &destination, name)?;
    register_workspace_member(&workspace, name)?;

    let manifest = Manifest::read_package(&destination).map_err(NewAppError::ScaffoldedManifest)?;
    println!(
        "created apps/{name} \u{2014} {} ({})",
        manifest.name(),
        manifest.id()
    );
    println!();
    println!("next steps:");
    println!("  cargo test -p paper-{name}");
    println!("  cargo run -p paper-{name} --features preview --example preview");
    println!("  paperctl package apps/{name}   # once bin/{name} is built (ADR-0022)");
    Ok(())
}

/// The workspace root, resolved at compile time from where `paperctl`'s own
/// `Cargo.toml` lives — reliable regardless of the directory `paperctl` is
/// invoked from, which a relative path would not be. Duplicated from
/// `crate::dev`'s own copy rather than shared: that one is gated on the
/// `desktop` feature, this on `not(target_os = "linux")`, and the two gates
/// do not imply each other.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `name` has to be a valid segment of an [`AppId`] — lowercase ASCII,
/// digits and `-` — because it becomes one (`dev.calum.<name>`). Reusing
/// [`AppId`]'s own parser is what keeps this in sync with that rule rather
/// than a second, looser copy of it.
fn validate_name(name: &str) -> Result<(), NewAppError> {
    format!("dev.calum.{name}")
        .parse::<AppId>()
        .map_err(|source| NewAppError::InvalidName {
            name: name.to_owned(),
            source,
        })?;
    Ok(())
}

/// PascalCase from `name`'s hyphen-separated segments — `task-list` becomes
/// `TaskList`. Used for the type names every screen module exports
/// (`<Name>App`, `<Name>Screen`, `<Name>Layout`).
fn pascal_case(name: &str) -> String {
    name.split('-')
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Copies [`TEMPLATE_FILES`] from `template` to `destination`, substituting
/// `name` into each one.
fn scaffold(template: &Path, destination: &Path, name: &str) -> Result<(), NewAppError> {
    for relative in TEMPLATE_FILES {
        let source_path = template.join(relative);
        let content =
            fs::read_to_string(&source_path).map_err(|source| NewAppError::ReadTemplate {
                path: source_path.clone(),
                source,
            })?;
        let rewritten = substitute(&content, name);

        let target_path = destination.join(relative);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|source| NewAppError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&target_path, rewritten).map_err(|source| NewAppError::Write {
            path: target_path,
            source,
        })?;
    }
    Ok(())
}

/// Rewrites the template's own identity (`counter`/`Counter`/`COUNTER`,
/// `paper-counter`, `dev.calum.counter`, ...) into `name`'s.
///
/// Ordered longest-and-most-specific first: `CounterApp` is replaced whole
/// before the bare `Counter` pass ever runs, so that pass never has a
/// `Counter` substring left inside an already-rewritten identifier to double
/// up on. The same reasoning orders `bin/counter` and `CARGO_BIN_EXE_counter`
/// ahead of the plain `counter` pass at the end.
fn substitute(content: &str, name: &str) -> String {
    let pascal = pascal_case(name);
    let underscored = name.replace('-', "_");
    let screaming = name.to_uppercase();

    content
        .replace("paper-counter", &format!("paper-{name}"))
        .replace("paper_counter", &format!("paper_{underscored}"))
        .replace("dev.calum.counter", &format!("dev.calum.{name}"))
        .replace(
            "CARGO_BIN_EXE_counter",
            &format!("CARGO_BIN_EXE_{underscored}"),
        )
        .replace("bin/counter", &format!("bin/{name}"))
        .replace("CounterApp", &format!("{pascal}App"))
        .replace("CounterScreen", &format!("{pascal}Screen"))
        .replace("CounterLayout", &format!("{pascal}Layout"))
        .replace("COUNTER", &screaming)
        .replace("Counter", &pascal)
        .replace("counter", name)
}

/// Adds `apps/<name>` to the workspace root `Cargo.toml`'s `members` list, so
/// `cargo build -p paper-<name>` works without a manual edit first.
///
/// A targeted line insertion, not a TOML round-trip through the `toml`
/// crate: that would parse and re-serialize the whole file, and this
/// project's own root `Cargo.toml` carries comments a generic serializer
/// does not know how to keep. Anchored on the `apps/render-test-card` line,
/// the last app already in the list, rather than on `members = [` itself,
/// so the new entry lands inside the apps group instead of after every
/// unrelated crate the file also lists.
fn register_workspace_member(workspace: &Path, name: &str) -> Result<(), NewAppError> {
    let cargo_toml = workspace.join("Cargo.toml");
    let contents =
        fs::read_to_string(&cargo_toml).map_err(|source| NewAppError::RegisterMember {
            name: name.to_owned(),
            source,
        })?;

    const ANCHOR: &str = "\"apps/render-test-card\",";
    let Some(anchor_at) = contents.find(ANCHOR) else {
        return Err(NewAppError::MembersListShapeChanged {
            name: name.to_owned(),
        });
    };
    let insert_at = anchor_at + ANCHOR.len();
    let indent = contents[..anchor_at]
        .rsplit('\n')
        .next()
        .unwrap_or_default();
    let mut rewritten = contents.clone();
    rewritten.insert_str(insert_at, &format!("\n{indent}\"apps/{name}\","));

    fs::write(&cargo_toml, rewritten).map_err(|source| NewAppError::RegisterMember {
        name: name.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{NewAppArgs, pascal_case, run, scaffold, substitute, workspace_root};
    use paper_packages::Manifest;

    #[test]
    fn pascal_case_joins_hyphenated_segments() {
        assert_eq!(pascal_case("counter"), "Counter");
        assert_eq!(pascal_case("task-list"), "TaskList");
        assert_eq!(pascal_case("a-b-c"), "ABC");
    }

    #[test]
    fn substitute_rewrites_every_form_of_the_template_name() {
        let rewritten = substitute(
            "paper-counter paper_counter dev.calum.counter CARGO_BIN_EXE_counter \
             bin/counter CounterApp CounterScreen CounterLayout \"COUNTER\" \
             \"Counter\" name = \"counter\"",
            "task-list",
        );
        assert!(!rewritten.contains("counter"), "{rewritten}");
        assert!(!rewritten.contains("Counter"), "{rewritten}");
        assert!(rewritten.contains("paper-task-list"));
        assert!(rewritten.contains("paper_task_list"));
        assert!(rewritten.contains("dev.calum.task-list"));
        assert!(rewritten.contains("CARGO_BIN_EXE_task_list"));
        assert!(rewritten.contains("bin/task-list"));
        assert!(rewritten.contains("TaskListApp"));
        assert!(rewritten.contains("TaskListScreen"));
        assert!(rewritten.contains("TaskListLayout"));
        assert!(rewritten.contains("\"TASK-LIST\""));
        assert!(rewritten.contains("\"TaskList\""));
        assert!(rewritten.contains("name = \"task-list\""));
    }

    /// Scaffolds a real app from the real template into a scratch directory
    /// — not into `apps/`, so this test never touches this checkout's own
    /// workspace — and checks the manifest it produced actually parses with
    /// the new identity substituted throughout.
    #[test]
    fn scaffolding_from_the_real_template_produces_a_valid_manifest() {
        let scratch = tempfile::tempdir().expect("a temp dir");
        let template = workspace_root().join(super::TEMPLATE_DIR);
        let destination = scratch.path().join("demo-app");

        scaffold(&template, &destination, "demo-app").expect("scaffolds without error");

        let manifest = Manifest::read_package(&destination).expect("a valid manifest");
        assert_eq!(manifest.id().as_str(), "dev.calum.demo-app");
        assert_eq!(manifest.entrypoint().as_str(), "bin/demo-app");

        let cargo_toml =
            std::fs::read_to_string(destination.join("Cargo.toml")).expect("reads Cargo.toml");
        assert!(cargo_toml.contains("name = \"paper-demo-app\""));
        assert!(cargo_toml.contains("name = \"demo-app\""));

        let lib_rs = std::fs::read_to_string(destination.join("src/lib.rs")).expect("reads lib.rs");
        assert!(lib_rs.contains("DemoAppApp"), "{lib_rs}");
        assert!(!lib_rs.contains("Counter"), "{lib_rs}");
    }

    #[test]
    fn a_name_that_is_not_a_valid_app_id_segment_is_refused() {
        let error = run(&NewAppArgs {
            name: "Not_Valid".to_owned(),
        })
        .expect_err("uppercase and underscore are not valid app id characters");
        assert!(error.to_string().contains("could not create"));
    }
}
