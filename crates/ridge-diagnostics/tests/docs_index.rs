//! `docs/diagnostics.md` is the registry rendered for someone who is not
//! reading the source.
//!
//! Generated rather than written: a table this size maintained in two places
//! drifts on the first hurried afternoon, and the drift is invisible — a page
//! that is merely stale looks exactly like a page that is right. So the page
//! has one author, the table, and this test is what says so out loud.
//!
//! It is also what the editor's diagnostic links point at, through
//! [`ridge_diagnostics::INDEX_URL`]: a heading per code, so every code has an
//! anchor of its own.
//!
//! To regenerate after changing the registry:
//!
//! ```text
//! RIDGE_BLESS=1 cargo test -p ridge-diagnostics --test docs_index
//! ```

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ridge_diagnostics::REGISTRY;

/// Set this to rewrite the page instead of checking it.
const BLESS: &str = "RIDGE_BLESS";

/// The page, relative to the workspace root.
fn index_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("docs")
        .join("diagnostics.md")
}

/// The crates that declare codes under one letter, sorted and deduplicated.
fn owners_of(letter: char) -> Vec<&'static str> {
    REGISTRY
        .iter()
        .filter(|e| e.code.starts_with(letter))
        .map(|e| e.owner)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Join names as prose: `a`, `a and b`, `a, b and c`.
fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("`{one}`"),
        [rest @ .., last] => {
            let head = rest
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{head} and `{last}`")
        }
    }
}

/// Render the whole page.
///
/// Sections are grouped by leading letter because that is the only grouping the
/// codes themselves carry. It is not a compiler phase and the page says so:
/// `P` covers the parser and the package layer, `T` the type checker and the
/// standard library, and their numbers interleave rather than partition — the
/// standard library's `T101`–`T103` sit inside a span the type checker also
/// uses. Naming a range per crate would read better and be false.
fn render() -> String {
    let mut out = String::new();

    out.push_str("# Diagnostic codes\n\n");
    out.push_str(
        "Every error and warning the compiler reports carries a code — `T031`, `P012`, `C001`.\n\
         The code is the stable handle on that failure: it survives rewording, a search box and a\n\
         CI filter can both match on it, and it is what this page is indexed by.\n\n",
    );
    out.push_str("To read one from the terminal instead:\n\n");
    out.push_str("```text\nridge explain T031\n```\n\n");
    out.push_str(
        "`ridge explain --list` prints the whole table. Every code answers — a code with no entry\n\
         cannot ship, because the registry census fails the build first.\n\n",
    );
    out.push_str(
        "The leading letter groups the codes; it is not a compiler phase. `P` covers the parser\n\
         and the package layer, `T` the type checker and the standard library, and the numbers\n\
         within a letter are shared rather than split between them.\n\n",
    );
    out.push_str(
        "*Generated from `crates/ridge-diagnostics/src/registry.rs`. Edit the registry, not this\n\
         page.*\n",
    );

    let mut current: Option<char> = None;
    for entry in REGISTRY {
        let Some(letter) = entry.code.chars().next() else {
            continue;
        };
        if current != Some(letter) {
            // Writing into a `String` cannot fail; the `Result` is discarded
            // rather than unwrapped so this stays panic-free.
            let _ = write!(out, "\n## `{letter}` codes\n\n");
            let _ = writeln!(out, "Declared by {}.", and_list(&owners_of(letter)));
            current = Some(letter);
        }
        let _ = write!(out, "\n### {}\n\n{}\n", entry.code, entry.summary);
    }

    out
}

/// The page and the table say the same thing.
///
/// Line endings are normalised on both sides before comparing: the page is
/// checked out with whatever the platform's Git is configured to produce, and a
/// carriage return is not drift worth failing a build over.
#[test]
fn the_page_matches_the_registry() {
    let expected = render();
    let path = index_path();

    if std::env::var_os(BLESS).is_some() {
        let wrote = std::fs::write(&path, &expected);
        assert!(wrote.is_ok(), "could not write {}", path.display());
        return;
    }

    let found = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        expected.replace("\r\n", "\n"),
        found.replace("\r\n", "\n"),
        "{} is out of date. Regenerate it with:\n    \
         {BLESS}=1 cargo test -p ridge-diagnostics --test docs_index",
        path.display()
    );
}

/// Every code has a heading, so every code has an anchor to link to.
///
/// The editor's `codeDescription` link is built from the code alone, without
/// consulting the page. A code whose heading went missing would be a link that
/// lands on the top of a 241-entry page and leaves the reader to search.
#[test]
fn every_code_has_a_heading_of_its_own() {
    let page = render();
    for entry in REGISTRY {
        assert!(
            page.contains(&format!("\n### {}\n", entry.code)),
            "{} has no heading",
            entry.code
        );
    }
}

/// The page names every crate that declares a code under each letter.
#[test]
fn each_section_names_the_crates_that_declare_it() {
    let page = render();
    for entry in REGISTRY {
        let Some(letter) = entry.code.chars().next() else {
            continue;
        };
        let section = format!("## `{letter}` codes");
        let found = page.find(&section);
        assert!(found.is_some(), "no section for {letter}");
        let Some(start) = found else { continue };
        let heading_line = page[start..].lines().take(3).collect::<String>();
        assert!(
            heading_line.contains(entry.owner),
            "the `{letter}` section does not name {}",
            entry.owner
        );
    }
}
