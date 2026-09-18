//! Scaffolds a new ADR, and the `docs/adr/README.md` row that indexes it.
//!
//! Split so the string logic — numbering, slugifying, rendering the template
//! and the table row — is plain functions over `&str`, testable without
//! touching a filesystem. [`run`] is the only part that does.

use std::fs;
use std::path::Path;

/// Where the ADR files and their index live, relative to the repo root.
pub(crate) const ADR_DIR: &str = "docs/adr";

/// Longest an existing ADR number in `README.md` can be before something is
/// very wrong with the table; guards against a runaway parse rather than any
/// real limit.
const MAX_PLAUSIBLE_NUMBER: u32 = 9999;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AdrError {
    #[error("cannot read {path}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{ADR_DIR}/README.md has no ADR table row to number from")]
    NoExistingAdr,
    #[error("the title has no word characters to slugify")]
    EmptyTitle,
}

/// The highest ADR number already indexed in `README.md`'s table.
///
/// Reads every `[NNNN](NNNN-...)` row rather than trusting the rows are in
/// order, so a table edited out of sequence still yields the right next
/// number.
fn highest_number(readme: &str) -> Option<u32> {
    readme
        .lines()
        .filter_map(|line| {
            let after_bracket = line.trim_start().strip_prefix("| [")?;
            let (number, _) = after_bracket.split_once(']')?;
            number.parse::<u32>().ok()
        })
        .filter(|&n| n <= MAX_PLAUSIBLE_NUMBER)
        .max()
}

/// A title turned into the lowercase, hyphen-separated slug every ADR file
/// name uses (e.g. `docs/adr/0017-chess-rules-library.md`).
fn slugify(title: &str) -> Result<String, AdrError> {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        return Err(AdrError::EmptyTitle);
    }
    Ok(slug)
}

/// The new ADR's file stem, e.g. `0024-a-new-decision`.
fn file_stem(number: u32, slug: &str) -> String {
    format!("{number:04}-{slug}")
}

/// The new file's contents, in the shape every existing ADR follows: a status
/// line, then the sections `docs/adr/README.md` promises every ADR states —
/// what was decided, what it costs, and what would make it wrong.
fn render_template(number: u32, title: &str) -> String {
    format!(
        "# ADR-{number:04} — {title}\n\
         \n\
         **Status:** proposed\n\
         \n\
         ## Context\n\
         \n\
         <!-- What forced this decision, and what was true before it. -->\n\
         \n\
         ## Decision\n\
         \n\
         <!-- What was decided, stated so a reader does not have to reconstruct it from the code. -->\n\
         \n\
         ## What it costs\n\
         \n\
         <!-- The trade-off this decision accepted. -->\n\
         \n\
         ## What would make this wrong\n\
         \n\
         <!-- The fact or measurement that would overturn this decision. -->\n"
    )
}

/// The new `README.md` table row.
fn render_row(number: u32, stem: &str, title: &str) -> String {
    format!("| [{number:04}]({stem}.md) | {title} |")
}

/// Appends `row` as the table's last line, after the last line that starts a
/// table row (`| [`). `README.md` has nothing after the table today, but
/// looking for the last row rather than assuming end-of-file survives a
/// trailing note being added later.
fn insert_row(readme: &str, row: &str) -> String {
    let last_row_line = readme
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("| ["))
        .map(|(i, _)| i)
        .last();

    let Some(last_row_line) = last_row_line else {
        // No existing row to anchor on: `highest_number` would already have
        // refused before this is called, so this path is unreached in
        // practice and appending is the safest fallback.
        let mut out = readme.to_owned();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(row);
        out.push('\n');
        return out;
    };

    let mut lines: Vec<&str> = readme.lines().collect();
    lines.insert(last_row_line + 1, row);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Scaffolds `docs/adr/NNNN-<slug>.md` under `repo_root` and adds its row to
/// `docs/adr/README.md`, numbering one past the highest existing ADR.
pub(crate) fn run(repo_root: &Path, title: &str) -> Result<String, AdrError> {
    let readme_path = repo_root.join(ADR_DIR).join("README.md");
    let readme = fs::read_to_string(&readme_path).map_err(|source| AdrError::Read {
        path: readme_path.display().to_string(),
        source,
    })?;

    let number = highest_number(&readme).ok_or(AdrError::NoExistingAdr)? + 1;
    let slug = slugify(title)?;
    let stem = file_stem(number, &slug);

    let adr_path = repo_root.join(ADR_DIR).join(format!("{stem}.md"));
    fs::write(&adr_path, render_template(number, title)).map_err(|source| AdrError::Write {
        path: adr_path.display().to_string(),
        source,
    })?;

    let updated_readme = insert_row(&readme, &render_row(number, &stem, title));
    fs::write(&readme_path, updated_readme).map_err(|source| AdrError::Write {
        path: readme_path.display().to_string(),
        source,
    })?;

    Ok(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_highest_numbered_row_regardless_of_order() {
        let readme =
            "| [0001](0001-a.md) | A |\n| [0023](0023-b.md) | B |\n| [0009](0009-c.md) | C |\n";
        assert_eq!(highest_number(readme), Some(23));
    }

    #[test]
    fn slugifies_punctuation_and_case() {
        assert_eq!(
            slugify("A Fourth Request: `Launch`").unwrap(),
            "a-fourth-request-launch"
        );
    }

    #[test]
    fn empty_title_is_refused() {
        assert!(matches!(slugify("   —   "), Err(AdrError::EmptyTitle)));
    }

    #[test]
    fn renders_the_file_stem_zero_padded() {
        assert_eq!(file_stem(5, "a-slug"), "0005-a-slug");
        assert_eq!(file_stem(1234, "a-slug"), "1234-a-slug");
    }

    #[test]
    fn inserts_the_new_row_after_the_last_existing_one() {
        let readme = "# Architecture decision records\n\n\
             | ADR | Decision |\n|---|---|\n\
             | [0001](0001-a.md) | A |\n\
             | [0002](0002-b.md) | B |\n";
        let updated = insert_row(readme, "| [0003](0003-c.md) | C |");
        assert_eq!(
            updated,
            "# Architecture decision records\n\n\
             | ADR | Decision |\n|---|---|\n\
             | [0001](0001-a.md) | A |\n\
             | [0002](0002-b.md) | B |\n\
             | [0003](0003-c.md) | C |\n"
        );
    }

    #[test]
    fn end_to_end_scaffolds_a_file_and_a_row() {
        let repo = tempfile::tempdir().unwrap();
        let adr_dir = repo.path().join(ADR_DIR);
        fs::create_dir_all(&adr_dir).unwrap();
        fs::write(
            adr_dir.join("README.md"),
            "# Architecture decision records\n\n\
             | ADR | Decision |\n|---|---|\n\
             | [0023](0023-paperctl-logs-doctor-deploy.md) | `paperctl logs`, `doctor` and `deploy` |\n",
        )
        .unwrap();

        let stem = run(repo.path(), "A New Decision").unwrap();

        assert_eq!(stem, "0024-a-new-decision");
        let file = fs::read_to_string(adr_dir.join("0024-a-new-decision.md")).unwrap();
        assert!(file.starts_with("# ADR-0024 — A New Decision\n"));
        assert!(file.contains("**Status:** proposed"));

        let readme = fs::read_to_string(adr_dir.join("README.md")).unwrap();
        assert!(readme.contains("| [0024](0024-a-new-decision.md) | A New Decision |"));
    }
}
