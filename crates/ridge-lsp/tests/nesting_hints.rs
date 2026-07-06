//! The LSP nest-depth hint fires on a `then`-nested `if` staircase in a
//! Result/Unit function — and only there. `else if` chains, shallow nests, and
//! non-Result functions are left alone.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;

use ridge_lexer::LineIndex;
use ridge_lsp::index::collect_nesting_hints;
use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::typecheck_workspace;
use tempfile::TempDir;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Url};

/// Type-check a single-module workspace and run the nest-depth hint pass over it.
fn hints_for(src: &str) -> Vec<Diagnostic> {
    let td = TempDir::new().expect("tempdir");
    let root = td.path();
    fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("app").join("src")).unwrap();
    fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    )
    .unwrap();
    fs::write(root.join("app").join("src").join("Main.ridge"), src).unwrap();

    let disc = discover_workspace(root);
    let resolved = resolve_workspace(disc.graph.expect("workspace graph"));
    let typed = typecheck_workspace(&resolved).typed;

    // Every module slot carries the same source LineIndex and a placeholder URI.
    // Only the module holding the staircase produces a hint, and its byte offsets
    // resolve against this source, so the coordinates are correct.
    let n = typed.modules.len();
    let url = Url::parse("file:///app/src/Main.ridge").unwrap();
    let line_indices: Vec<LineIndex> = (0..n).map(|_| LineIndex::new(src)).collect();
    let module_uris: Vec<Option<Url>> = (0..n).map(|_| Some(url.clone())).collect();

    collect_nesting_hints(&line_indices, &module_uris, &typed)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

const STAIRCASE: &str = "\
pub fn checks (n: Int) -> Result Unit Text =
    if n == 1 then
        if n == 2 then
            if n == 3 then Ok ()
            else Err \"c\"
        else Err \"b\"
    else Err \"a\"
";

#[test]
fn staircase_gets_one_hint() {
    let hints = hints_for(STAIRCASE);
    assert_eq!(
        hints.len(),
        1,
        "one hint for the outermost if; got {hints:?}"
    );
    assert_eq!(hints[0].severity, Some(DiagnosticSeverity::HINT));
    assert!(
        hints[0].message.contains("guard") && hints[0].message.contains('?'),
        "the hint should name the guard/? remedies; got {:?}",
        hints[0].message
    );
    // Anchored at the outermost `if`, on the second line (0-indexed line 1).
    assert_eq!(hints[0].range.start.line, 1, "hint anchors at the outer if");
}

#[test]
fn else_if_chain_is_not_flagged() {
    // A flat multibranch nests through `else`, not `then` — it is the idiom, not
    // a staircase.
    let src = "\
pub fn grade (n: Int) -> Result Unit Text =
    if n == 1 then Ok ()
    else if n == 2 then Err \"two\"
    else if n == 3 then Err \"three\"
    else Err \"other\"
";
    assert!(
        hints_for(src).is_empty(),
        "an else-if chain must not be flagged"
    );
}

#[test]
fn shallow_nest_is_not_flagged() {
    // Two levels is below the threshold.
    let src = "\
pub fn checks (n: Int) -> Result Unit Text =
    if n == 1 then
        if n == 2 then Ok ()
        else Err \"b\"
    else Err \"a\"
";
    assert!(
        hints_for(src).is_empty(),
        "a two-deep nest is below threshold"
    );
}

#[test]
fn non_result_function_is_not_flagged() {
    // The guard/? remedies only apply to Result/Unit functions, so a staircase
    // in an Int-returning function is left alone.
    let src = "\
pub fn depth (n: Int) -> Int =
    if n == 1 then
        if n == 2 then
            if n == 3 then 3 else 2
        else 1
    else 0
";
    assert!(
        hints_for(src).is_empty(),
        "a staircase in a non-Result function must not be flagged"
    );
}

#[test]
fn two_staircases_get_two_hints() {
    let src = format!(
        "{STAIRCASE}\n\
pub fn other (n: Int) -> Result Unit Text =
    if n == 4 then
        if n == 5 then
            if n == 6 then Ok ()
            else Err \"f\"
        else Err \"e\"
    else Err \"d\"
"
    );
    assert_eq!(
        hints_for(&src).len(),
        2,
        "two independent staircases get one hint each"
    );
}
