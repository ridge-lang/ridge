//! T17 — capability-leak fixture harness for `ridge-typecheck` (plan §10 T17,
//! §9.4, §11.3 `DoD` line 1547).
//!
//! Six fixtures under `tests/fixtures/capability/*.ridge` each exercise one
//! decision-tagged capability rule (D018 Model B, D040, D041, D058).  Each
//! fixture starts with one of two directives:
//!
//! - `-- expect: T###` — assert this T-code appears among the emitted errors.
//! - `-- expect-clean` — assert *no* T-errors are emitted.
//!
//! The harness mirrors the structure of `tests/errors.rs` but supports the
//! "clean" form so positive (well-typed) capability scenarios are first-class.
//!
//! Per `DoD`: all six fixtures must be present and the suite must pass.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};
use tempfile::TempDir;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/capability");

const REQUIRED_FIXTURES: usize = 6;

// ── Workspace setup helpers ──────────────────────────────────────────────────

fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

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

fn run_typecheck_on_source(stem: &str, src: &str) -> Vec<TypeError> {
    let td = build_single_module_workspace(stem, src);
    let disc = discover_workspace(td.path());
    let Some(ws_graph) = disc.graph else {
        return Vec::new();
    };
    let resolved = resolve_workspace(ws_graph);
    let result = typecheck_workspace(&resolved);
    result.errors.into_iter().map(|(_, e)| e).collect()
}

// ── Directive parser ────────────────────────────────────────────────────────

#[derive(Debug)]
enum Expectation {
    /// `-- expect-clean` — no T-errors must be emitted.
    Clean,
    /// `-- expect: T###` — the listed code must appear among the emitted
    /// errors (other codes are ignored).
    Codes(Vec<String>),
}

fn parse_expectation(src: &str) -> Option<Expectation> {
    let mut codes: Vec<String> = Vec::new();
    let mut clean = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("--") {
            break;
        }
        let after_dashes = trimmed.trim_start_matches('-').trim();
        if after_dashes == "expect-clean" {
            clean = true;
            continue;
        }
        if let Some(rest) = after_dashes.strip_prefix("expect:") {
            if let Some(code) = rest.split_whitespace().next() {
                codes.push(code.to_uppercase());
            }
        }
    }
    if clean {
        return Some(Expectation::Clean);
    }
    if !codes.is_empty() {
        return Some(Expectation::Codes(codes));
    }
    None
}

// ── Fixture-driven test ──────────────────────────────────────────────────────

#[test]
fn capability_fixtures_pass() {
    let dir = PathBuf::from(FIXTURE_DIR);
    assert!(
        dir.is_dir(),
        "capability fixture directory does not exist: {}",
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

        let Some(expect) = parse_expectation(&src) else {
            failures.push(format!(
                "{file_name}: missing `-- expect: T###` or `-- expect-clean` directive"
            ));
            continue;
        };
        count += 1;

        let errors = run_typecheck_on_source(&stem, &src);
        let actual_codes: Vec<&str> = errors.iter().map(TypeError::code).collect();

        match expect {
            Expectation::Clean => {
                if !errors.is_empty() {
                    failures.push(format!(
                        "{file_name}: expected no T-errors but got {actual_codes:?}"
                    ));
                }
            }
            Expectation::Codes(expected) => {
                for code in &expected {
                    if !errors.iter().any(|e| e.code() == code) {
                        failures.push(format!(
                            "{file_name}: expected {code} but got {actual_codes:?}"
                        ));
                    }
                }
            }
        }
    }

    assert!(
        count >= REQUIRED_FIXTURES,
        "DoD requires {REQUIRED_FIXTURES} capability fixtures; got {count}"
    );
    assert!(
        failures.is_empty(),
        "capability fixture failures:\n  {}",
        failures.join("\n  ")
    );
}
