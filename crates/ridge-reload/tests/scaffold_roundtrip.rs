//! Scaffold roundtrip: the migration scaffold `--check` prints for a
//! hole-free change must parse AND compile. A scaffold with `???` holes must
//! parse-fail (holes can never compile — that is what makes them safe).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;

use ridge_reload::snapshot;
use ridge_types::VersionHistory;

fn write_file(dir: &std::path::Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// Lay out a one-project workspace containing `src` and return its path.
fn workspace(src: &str) -> tempfile::TempDir {
    let td = tempfile::TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"t\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "apps/demo/src/main.ridge", src);
    td
}

/// Compile `src` through the full front-end (parse → resolve → typecheck)
/// with `history` injected the way the driver injects the previous build's
/// snapshot history, and return the accumulated error codes (empty = clean).
fn compile_codes_with(src: &str, history: &VersionHistory) -> Vec<String> {
    let td = workspace(src);
    let disc = ridge_resolve::discover_workspace(td.path());
    let ws = disc.graph.expect("graph");
    let resolved = ridge_resolve::resolve_workspace(ws);
    assert!(
        resolved.parse_errors.is_empty(),
        "scaffold must PARSE: {:?}",
        resolved.parse_errors
    );
    let result = ridge_typecheck::typecheck_workspace_with_history(&resolved, history);
    result
        .errors
        .iter()
        .map(|(_, e)| e.code().to_string())
        .collect()
}

/// Fresh-build codes, with no previous version history.
fn compile_codes(src: &str) -> Vec<String> {
    compile_codes_with(src, &VersionHistory::default())
}

/// Compile `src` as the PREVIOUS build and return its snapshot history —
/// the exact data the driver would thread into the next typecheck.
fn snapshot_history_of(src: &str) -> VersionHistory {
    let td = workspace(src);
    let disc = ridge_resolve::discover_workspace(td.path());
    let ws = disc.graph.expect("graph");
    let resolved = ridge_resolve::resolve_workspace(ws);
    assert!(
        resolved.parse_errors.is_empty(),
        "previous build must PARSE: {:?}",
        resolved.parse_errors
    );
    let checked = ridge_typecheck::typecheck_workspace(&resolved);
    let snap = snapshot::extract_snapshot(&resolved, &checked.typed, None);
    snapshot::history_of(&snap)
}

#[test]
fn rename_record_scaffold_parses_and_compiles() {
    // Simulate: v1 User{name,age} → v2 User{full_name,age} (pure rename).
    let old = vec![
        snapshot::FieldSnap {
            name: "name".into(),
            ty: "Text".into(),
        },
        snapshot::FieldSnap {
            name: "age".into(),
            ty: "Int".into(),
        },
    ];
    let new = vec![
        snapshot::FieldSnap {
            name: "full_name".into(),
            ty: "Text".into(),
        },
        snapshot::FieldSnap {
            name: "age".into(),
            ty: "Int".into(),
        },
    ];
    let plan = ridge_reload::scaffold::field_plan(&old, &new);
    let scaffold = ridge_reload::scaffold::record_migrate("demo.main", "User", 1, &new, &plan, &[]);
    assert!(!scaffold.contains("???"), "rename scaffold is hole-free");
    // The scaffold IS the new source (minus its leading comment line).
    let src: String = scaffold
        .lines()
        .filter(|l| !l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    // `User@1` resolves against the previous build's snapshot history —
    // the same history the driver injects on a real rebuild.
    let history = snapshot_history_of("pub type User = { name: Text, age: Int }\n");
    let codes = compile_codes_with(&src, &history);
    assert!(
        codes.is_empty(),
        "scaffold must compile clean: {codes:?}\n{src}"
    );
}

#[test]
fn actor_scaffold_parses_and_compiles() {
    let plan = vec![ridge_reload::scaffold::FieldAction::Rename {
        from: "count".into(),
        to: "total".into(),
    }];
    let hook = ridge_reload::scaffold::actor_migrate("Counter", 1, &plan, &[]);
    // `actor_migrate` emits at column 0; actor members live one indent
    // level inside the `actor` block.
    let hook: String = hook
        .lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    let src = format!(
        "pub actor Counter =\n    state total: Int = 0\n{hook}\n    on bump =\n        total <- total + 1\n"
    );
    let codes = compile_codes(&src);
    // The hook references Counter@1 with NO history (fresh build) — T049 is
    // expected and correct here; everything else must be clean and the
    // parse must have succeeded (asserted inside compile_codes).
    assert!(codes.iter().all(|c| c == "T049"), "{codes:?}\n{src}");
}

#[test]
fn scaffold_with_hole_does_not_parse() {
    let new = vec![
        snapshot::FieldSnap {
            name: "name".into(),
            ty: "Text".into(),
        },
        snapshot::FieldSnap {
            name: "email".into(),
            ty: "Text".into(),
        },
    ];
    let plan = ridge_reload::scaffold::field_plan(&new[..1], &new);
    let scaffold = ridge_reload::scaffold::record_migrate("demo.main", "User", 1, &new, &plan, &[]);
    assert!(scaffold.contains("???"));
    let src: String = scaffold
        .lines()
        .filter(|l| !l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let td = workspace(&src);
    let disc = ridge_resolve::discover_workspace(td.path());
    let ws = disc.graph.expect("graph");
    let resolved = ridge_resolve::resolve_workspace(ws);
    assert!(
        !resolved.parse_errors.is_empty(),
        "a hole must NOT parse — that is the safety property"
    );
}
