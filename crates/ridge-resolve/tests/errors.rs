//! T15 — per-`R###` fixture harness for `ridge-resolve` (plan §10 T15, §9.2).
//!
//! Mirrors the Phase 2 parser fixture harness
//! (`crates/ridge-parser/tests/errors.rs`).  Two complementary mechanisms live
//! here:
//!
//! - **Single-file fixtures** under `tests/fixtures/resolve/r###_*.ridge` — each
//!   declares one or more `-- expect: R### [span=A..B]` headers on the leading
//!   comment lines.  [`all_fixtures_pass`] iterates the directory, builds a
//!   synthetic single-module workspace per fixture, runs the full T1..T13
//!   pipeline, and asserts every expected code appears (and, when given, that
//!   the listed source range is contained in at least one matching error).
//!
//! - **Workspace-shaped programmatic tests** for the `R###` codes that cannot
//!   be modelled as a single source file (cross-project visibility, cyclic
//!   imports, manifest-driven capability enforcement, architectural rules).
//!   Each builds its own tempdir workspace and asserts the expected code.
//!
//! ## Codes intentionally not covered by fixtures
//!
//! - **R018** — RESERVED; the former variant was removed (bare imports are
//!   unambiguous per R001 provisional default accepted 2026-04-25).
//!   The numeric slot is kept to prevent code reuse.
//! - **R019 `UnknownCapabilityKeyword`** — defensive; the parser maps every
//!   recognised keyword to a closed `Capability` enum variant before this
//!   pass, so R019 is unreachable from real Ridge source (`capabilities.rs`
//!   rustdoc explicitly states "no test fixture is provided").
//! - **R020 `CapabilityListOnWrongDecl`** — defensive; the grammar only allows
//!   capability lists on `fn`/`init`/`on`, and the parser enforces this.
//! - **R999 `InternalNodeIdCollision`** — defensive; signals a parser invariant
//!   violation, not a user-authored error.
//!
//! ## `DoD` §10 T15
//!
//! ≥ 20 tests pass; every reachable `R###` code from §5.1 has at least one
//! single-file fixture or programmatic test.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use ridge_resolve::{discover_workspace, resolve_workspace, ResolveError};
use tempfile::TempDir;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/resolve");

// ── Filesystem + workspace helpers ───────────────────────────────────────────

/// Write `content` to `dir/relative_path`, creating parent directories.
fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// Build a synthetic single-module workspace whose only module has FQN
/// `demo.<stem>` (project name `demo`, src/<stem>.ridge holds the fixture body).
///
/// The project exports everything via `[project.exports].public = ["**"]`
/// so cross-project R007 / R009 noise never fires for the single-module
/// fixtures (those scenarios are exercised explicitly by the programmatic
/// tests below).
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

/// Run the full T1..T13 resolve pipeline over the workspace at `td.path()`
/// and return every `R###` error produced (across discovery, symbol
/// collection, import resolution, authoritative cycle detection, walker,
/// capability check, and forbid rules).
///
/// `M###` manifest errors are not returned; they are out of scope for §10
/// T15 (per §5.1, only `R###` codes are listed).
fn run_pipeline(td: &TempDir) -> Vec<ResolveError> {
    let disc = discover_workspace(td.path());
    let mut all_errors: Vec<ResolveError> = disc.resolve_errors;
    let Some(ws) = disc.graph else {
        return all_errors;
    };
    let resolved = resolve_workspace(ws);
    all_errors.extend(resolved.errors.into_iter().map(|(_, e)| e));
    all_errors
}

/// Run the pipeline on a single source string wrapped into a one-module
/// synthetic workspace whose FQN is `demo.<stem>`.
fn run_pipeline_on_source(stem: &str, src: &str) -> Vec<ResolveError> {
    let td = build_single_module_workspace(stem, src);
    run_pipeline(&td)
}

// ── `-- expect:` directive parser ────────────────────────────────────────────

/// One `-- expect: R### [span=A..B]` directive parsed off a fixture's leading
/// comment lines.
#[derive(Debug)]
struct ExpectLine {
    /// Stable error code (e.g. `"R010"`).
    code: String,
    /// Optional half-open byte range `[start, end)` that at least one
    /// matching error's span must contain.
    span: Option<(usize, usize)>,
}

/// Parse `-- expect: R### [span=A..B]` directives from the leading comment
/// block of `src`.  Scanning stops at the first non-`--` line.
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
        let mut span = None;
        for token in tokens {
            if let Some(range) = token.strip_prefix("span=") {
                if let Some((a, b)) = range.split_once("..") {
                    if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                        span = Some((a, b));
                    }
                }
            }
        }
        out.push(ExpectLine {
            code: code.to_uppercase(),
            span,
        });
    }
    out
}

// ── Fixture-driven test ──────────────────────────────────────────────────────

/// Iterate every `tests/fixtures/resolve/*.ridge` file, run the resolve pipeline
/// over a synthetic single-module workspace, and assert every `-- expect:`
/// directive is satisfied.
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

        let errors = run_pipeline_on_source(&stem, &src);
        let actual_codes: Vec<&str> = errors.iter().map(ResolveError::code).collect();

        for exp in &expects {
            let matches: Vec<&ResolveError> =
                errors.iter().filter(|e| e.code() == exp.code).collect();
            if matches.is_empty() {
                failures.push(format!(
                    "{file_name}: expected {} but got codes {:?}",
                    exp.code, actual_codes
                ));
                continue;
            }
            if let Some((a, b)) = exp.span {
                let span_ok = matches.iter().any(|e| {
                    let s = e.span();
                    (s.start as usize) <= a && b <= (s.end as usize)
                });
                if !span_ok {
                    let actual_spans: Vec<String> = matches
                        .iter()
                        .map(|e| {
                            let s = e.span();
                            format!("{}..{}", s.start, s.end)
                        })
                        .collect();
                    failures.push(format!(
                        "{file_name}: {} found but [{a}..{b}] not contained in any: {actual_spans:?}",
                        exp.code
                    ));
                }
            }
        }
    }

    assert!(
        count >= 20,
        "DoD requires at least 20 single-file fixtures; got {count}"
    );
    assert!(
        failures.is_empty(),
        "fixture failures:\n  {}",
        failures.join("\n  ")
    );
}

// ── Programmatic R### tests (workspace-shaped) ───────────────────────────────

/// `R001 MissingWorkspaceManifest` — `discover_workspace` walked from a
/// directory with no `ridge.toml` ancestor.
#[test]
fn r001_missing_workspace_manifest() {
    let td = TempDir::new().unwrap();
    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.iter().any(|e| e.code() == "R001"),
        "expected R001; errors: {:?}",
        disc.resolve_errors
    );
    drop(td);
}

/// `R002 DuplicateModule` — two projects whose dot-prefixed module trees
/// overlap on a common FQN (the canonical portable recipe from
/// `discovery.rs::tests::r002_*`).
#[test]
fn r002_duplicate_module_across_projects() {
    let td = TempDir::new().unwrap();
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/acme/ridge.toml",
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        td.path(),
        "libs/acme/src/domain/Foo.ridge",
        "fn noop = ()\n",
    );
    write_file(
        td.path(),
        "libs/acmedomain/ridge.toml",
        "[project]\nname = \"acme.domain\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "libs/acmedomain/src/Foo.ridge", "fn noop = ()\n");

    let errors = run_pipeline(&td);
    assert!(
        errors.iter().any(|e| e.code() == "R002"),
        "expected R002; errors: {errors:?}"
    );
    drop(td);
}

/// `R003 CyclicImport` — module `lib.A` imports `lib.B`, `lib.B` imports
/// `lib.A`.  The authoritative cycle detector (T7 + §4.4) emits one R003.
#[test]
fn r003_cyclic_import_two_modules() {
    let td = TempDir::new().unwrap();
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/lib/ridge.toml",
        "[project]\n\
         name = \"lib\"\n\
         version = \"0.1.0\"\n\
         kind = \"library\"\n\
         \n\
         [project.exports]\n\
         public = [\"**\"]\n",
    );
    write_file(
        td.path(),
        "libs/lib/src/A.ridge",
        "import lib.B as B\n\nfn noopA = ()\n",
    );
    write_file(
        td.path(),
        "libs/lib/src/B.ridge",
        "import lib.A as A\n\nfn noopB = ()\n",
    );

    let errors = run_pipeline(&td);
    assert!(
        errors.iter().any(|e| e.code() == "R003"),
        "expected R003; errors: {errors:?}"
    );
    drop(td);
}

/// `R007 ProjectExportViolation` — project `alpha` imports `beta.Mod`, but
/// project `beta` declares no `[project.exports]` table at all and so
/// nothing is exported (`exports_public = []`, `exports_internal = []`).
/// Different first segments rule out the namespace-internal fallback.
#[test]
fn r007_project_export_violation_cross_project() {
    let td = TempDir::new().unwrap();
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/alpha/ridge.toml",
        "[project]\nname = \"alpha\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        td.path(),
        "libs/alpha/src/Use.ridge",
        "import beta.Mod as M\n\nfn noop = ()\n",
    );
    write_file(
        td.path(),
        "libs/beta/ridge.toml",
        "[project]\nname = \"beta\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "libs/beta/src/Mod.ridge", "fn helper = ()\n");

    let errors = run_pipeline(&td);
    assert!(
        errors.iter().any(|e| e.code() == "R007"),
        "expected R007; errors: {errors:?}"
    );
    drop(td);
}

/// `R009 VisibilityViolation` — project `alpha` does
/// `import beta.Mod (helper)`.  `beta` exports the module publicly, so R007
/// does not fire, but `helper` has no `pub` modifier (default
/// `ResolvedVisibility::ProjectPrivate`) and is invisible across project
/// boundaries.
#[test]
fn r009_visibility_violation_cross_project() {
    let td = TempDir::new().unwrap();
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/alpha/ridge.toml",
        "[project]\nname = \"alpha\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        td.path(),
        "libs/alpha/src/Use.ridge",
        "import beta.Mod (helper)\n\nfn noop = ()\n",
    );
    write_file(
        td.path(),
        "libs/beta/ridge.toml",
        "[project]\n\
         name = \"beta\"\n\
         version = \"0.1.0\"\n\
         kind = \"library\"\n\
         \n\
         [project.exports]\n\
         public = [\"**\"]\n",
    );
    write_file(td.path(), "libs/beta/src/Mod.ridge", "fn helper = ()\n");

    let errors = run_pipeline(&td);
    assert!(
        errors.iter().any(|e| e.code() == "R009"),
        "expected R009; errors: {errors:?}"
    );
    drop(td);
}

/// `R013 ForbidViolation` — canonical `acme.domain → acme.infra` rule
/// mirroring spec §8.6.  Exactly one R013 must be emitted.
#[test]
fn r013_forbid_violation_acme_workspace() {
    let td = TempDir::new().unwrap();
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\n\
         name = \"acme\"\n\
         version = \"0.1.0\"\n\
         members = [\"libs/*\"]\n\
         \n\
         [workspace.rules]\n\
         forbid = [{ from = \"acme.domain.**\", to = \"acme.infra.**\" }]\n",
    );
    write_file(
        td.path(),
        "libs/domain/ridge.toml",
        "[project]\n\
         name = \"acme.domain\"\n\
         version = \"0.1.0\"\n\
         kind = \"library\"\n\
         \n\
         [project.exports]\n\
         public = [\"**\"]\n",
    );
    write_file(
        td.path(),
        "libs/domain/src/RegisterUser.ridge",
        "import acme.infra.Postgres as Pg\n\nfn doIt = ()\n",
    );
    write_file(
        td.path(),
        "libs/infra/ridge.toml",
        "[project]\n\
         name = \"acme.infra\"\n\
         version = \"0.1.0\"\n\
         kind = \"library\"\n\
         \n\
         [project.exports]\n\
         public = [\"**\"]\n",
    );
    write_file(
        td.path(),
        "libs/infra/src/Postgres.ridge",
        "fn connect = ()\n",
    );

    let errors = run_pipeline(&td);
    let r013_count = errors.iter().filter(|e| e.code() == "R013").count();
    assert_eq!(r013_count, 1, "expected exactly 1 R013; errors: {errors:?}");
    drop(td);
}

/// `R015 CapabilityDenied` — workspace `[capabilities].deny = ["ffi"]` rules
/// out `fn ffi load = ...` regardless of project policy.
#[test]
fn r015_capability_denied_by_workspace() {
    let td = TempDir::new().unwrap();
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\n\
         name = \"ws\"\n\
         version = \"0.1.0\"\n\
         members = [\"apps/*\"]\n\
         \n\
         [workspace.capabilities]\n\
         deny = [\"ffi\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        td.path(),
        "apps/demo/src/UsesFfi.ridge",
        "fn ffi load () -> Unit = ()\n",
    );

    let errors = run_pipeline(&td);
    assert!(
        errors.iter().any(|e| e.code() == "R015"),
        "expected R015; errors: {errors:?}"
    );
    drop(td);
}

/// `R016 CapabilityNotAllowed` — project `[capabilities].allow = ["io"]` is
/// a whitelist; `fn net listen = ...` is not in it.
#[test]
fn r016_capability_not_allowed_by_project() {
    let td = TempDir::new().unwrap();
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
         [capabilities]\n\
         allow = [\"io\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/src/UsesNet.ridge",
        "fn net listen () -> Unit = ()\n",
    );

    let errors = run_pipeline(&td);
    assert!(
        errors.iter().any(|e| e.code() == "R016"),
        "expected R016; errors: {errors:?}"
    );
    drop(td);
}

// ── parse_expects unit tests ─────────────────────────────────────────────────

#[cfg(test)]
mod unit {
    use super::parse_expects;

    #[test]
    fn parses_single_code() {
        let src = "-- expect: R010\n-- explanation\nfn f = 1\n";
        let v = parse_expects(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].code, "R010");
        assert!(v[0].span.is_none());
    }

    #[test]
    fn parses_code_with_span() {
        let src = "-- expect: R010 span=27..30\n";
        let v = parse_expects(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].code, "R010");
        assert_eq!(v[0].span, Some((27, 30)));
    }

    #[test]
    fn stops_at_first_non_comment() {
        let src = "-- expect: R001\nfn f = 1\n-- expect: R002\n";
        let v = parse_expects(src);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn lowercase_code_is_normalised() {
        let src = "-- expect: r010\n";
        let v = parse_expects(src);
        assert_eq!(v[0].code, "R010");
    }

    #[test]
    fn malformed_span_is_ignored() {
        let src = "-- expect: R010 span=abc..def\n";
        let v = parse_expects(src);
        assert_eq!(v.len(), 1);
        assert!(v[0].span.is_none());
    }
}
