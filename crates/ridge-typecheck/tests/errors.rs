//! T17 — per-`T###` fixture harness for `ridge-typecheck` (plan §10 T17, §9.2).
//!
//! Mirrors Phase 3's `crates/ridge-resolve/tests/errors.rs`.  Each fixture file
//! under `tests/fixtures/typecheck/*.ridge` declares one or more
//! `-- expect: T###` directives.  [`all_fixtures_pass`] iterates the directory,
//! builds a synthetic single-module workspace per fixture, runs the full
//! resolve+typecheck pipeline, and asserts every expected code appears.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};
use tempfile::TempDir;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/typecheck");

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// Wrap a source string in a one-module synthetic workspace with FQN
/// `demo.<stem>`.
fn build_single_module_workspace(stem: &str, src: &str) -> TempDir {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/ridge.toml",
        "[project]\n\
         name = \"demo\"\n\
         version = \"0.1.0\"\n\
         kind = \"library\"\n\
         \n\
         [project.exports]\n\
         public = [\"**\"]\n",
    );
    write_file(td.path(), &format!("apps/demo/src/{stem}.ridge"), src);
    td
}

/// Run the full resolve+typecheck pipeline over the workspace at `td.path()`.
/// Returns the combined vector of T### errors (module attribution stripped —
/// tests care about the error code, not the source module).
fn run_typecheck_pipeline(td: &TempDir) -> Vec<TypeError> {
    let disc = discover_workspace(td.path());
    let Some(ws_graph) = disc.graph else {
        return Vec::new();
    };
    let resolved = resolve_workspace(ws_graph);
    // We deliberately ignore R-errors here — we're testing T-errors only.
    let result = typecheck_workspace(&resolved);
    result.errors.into_iter().map(|(_, e)| e).collect()
}

fn run_typecheck_on_source(stem: &str, src: &str) -> Vec<TypeError> {
    let td = build_single_module_workspace(stem, src);
    run_typecheck_pipeline(&td)
}

// ── `-- expect:` directive parser ─────────────────────────────────────────────

#[derive(Debug)]
struct ExpectLine {
    code: String,
}

fn parse_expects(src: &str) -> Vec<ExpectLine> {
    let mut out = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("--") {
            break;
        }
        let after_dashes = trimmed.trim_start_matches('-').trim();
        let Some(rest) = after_dashes.strip_prefix("expect:") else {
            continue;
        };
        let mut tokens = rest.split_whitespace();
        let Some(code) = tokens.next() else { continue };
        out.push(ExpectLine {
            code: code.to_uppercase(),
        });
    }
    out
}

// ── Fixture-driven test ───────────────────────────────────────────────────────

/// Iterate every `tests/fixtures/typecheck/*.ridge` file, run the typecheck
/// pipeline, and assert every `-- expect: T###` directive is satisfied.
///
/// `DoD` §9.2: ≥ 25 single-file fixtures; every reachable T### code must have
/// at least one fixture.
#[test]
fn all_fixtures_pass() {
    let dir = PathBuf::from(FIXTURE_DIR);
    assert!(
        dir.is_dir(),
        "fixture directory does not exist: {}",
        dir.display()
    );

    let mut entries: Vec<_> = fs::read_dir(&dir)
        .expect("read fixture dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "ridge"))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    let mut failures: Vec<String> = Vec::new();
    let mut count = 0usize;

    for entry in entries {
        let path = entry.path();
        let stem = path
            .file_stem()
            .expect("fixture stem")
            .to_string_lossy()
            .into_owned();
        let file_name = path
            .file_name()
            .expect("fixture filename")
            .to_string_lossy()
            .into_owned();

        let src = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));

        let expects = parse_expects(&src);
        if expects.is_empty() {
            failures.push(format!("{file_name}: no `-- expect:` directive"));
            continue;
        }
        count += 1;

        let errors = run_typecheck_on_source(&stem, &src);
        let actual_codes: Vec<&str> = errors.iter().map(TypeError::code).collect();

        for exp in &expects {
            let found = errors.iter().any(|e| e.code() == exp.code);
            if !found {
                failures.push(format!(
                    "{file_name}: expected {} but got codes {:?}",
                    exp.code, actual_codes
                ));
            }
        }
    }

    assert!(
        count >= 25,
        "DoD requires at least 25 single-file fixtures; got {count}"
    );
    assert!(
        failures.is_empty(),
        "fixture failures:\n  {}",
        failures.join("\n  ")
    );
}

/// Regression: an actor whose state field is `Handle <ActorB>` where
/// `<ActorB>` is declared LATER in the same source file must typecheck
/// without errors.  Before the two-pass `collect_user_tycons` refactor,
/// `ActorB` was not yet in the user-tycon name map when pass 2 resolved
/// `Handle ActorB`, so the field type fell through to a fresh `Type::Var`
/// and any `state.fieldB ! msg` later raised `T020 send on non-actor`
/// with the polymorphic stub type embedded in the message.
#[test]
fn forward_actor_type_reference_typechecks_cleanly() {
    let src = "\
actor Caller =\n\
    state target: Handle Receiver\n\
\n\
    init (r: Handle Receiver) =\n\
        target <- r\n\
\n\
    on poke =\n\
        target ! ping\n\
\n\
actor Receiver =\n\
    state count: Int = 0\n\
\n\
    on ping =\n\
        count <- count + 1\n\
";
    let errors = run_typecheck_on_source("forward_actor", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        !codes.contains(&"T020"),
        "forward-referenced actor handle must NOT raise T020; got: {codes:?}"
    );
    assert!(
        !codes.contains(&"T999"),
        "forward-referenced actor handle must NOT raise T999; got: {codes:?}"
    );
}
