//! End-to-end check for the typed `DbErrorKind` classification — running on the BEAM.
//!
//! `dbErrorKind` reads the kind off a typed `DbError`, so consumer code branches
//! on a failure's cause — recover from a `UniqueViolation`, retry a
//! `ConnectionError` — rather than string-matching the code. The accessors
//! `dbErrorConstraint`/`dbErrorColumn` read the constraint or column a backend
//! named. The data layer reports every failure as the typed record, so the kind
//! arrives already classified.
//!
//! User code cannot build an `Error` directly (it is nominal and has no source
//! constructor), so this drives a genuine failure: the in-memory adapter has no SQL
//! engine, so a raw statement fails with `raw.unsupported`. Classifying that error
//! exercises the whole consumer path — importing and matching the reconciled
//! `DbErrorKind`, and reading an accessor — proving the union is usable across the
//! module boundary. The full SQLSTATE-to-kind table (`db.error.235xx` →
//! unique/foreign-key/not-null/check) is covered against a real Postgres in the
//! database e2e.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (memAdapter, MemAdapter, dbErrorKind, dbErrorConstraint, dbErrorIsTransient, mkDbError, DbErrorKind, UniqueViolation, ForeignKeyViolation, NotNullViolation, CheckViolation, ConnectionError, DecodeError, Unsupported, QueryError)
import std.raw as Raw
import std.text as Text

-- Tag a classified error by its kind.
fn tag (k: DbErrorKind) -> Text =
    match k
        UniqueViolation -> "unique"
        ForeignKeyViolation -> "fk"
        NotNullViolation -> "notnull"
        CheckViolation -> "check"
        ConnectionError -> "connection"
        DecodeError -> "decode"
        Unsupported -> "unsupported"
        QueryError -> "query"

-- The in-memory adapter has no SQL engine, so a raw statement fails with the
-- `raw.unsupported` code — a typed `DbError` whose kind reads as `Unsupported`.
pub fn db unsupportedKind () -> Text =
    let conn = memAdapter ()
    match Raw.exec conn "DELETE FROM t" []
        Err e -> tag (dbErrorKind e)
        Ok _ -> "unexpected-ok"

-- The constraint accessor reads empty on a non-constraint error, wrapped so the
-- emptiness is visible in the assertion.
pub fn db unsupportedConstraint () -> Text =
    let conn = memAdapter ()
    match Raw.exec conn "DELETE FROM t" []
        Err e -> Text.concat "[" (Text.concat (dbErrorConstraint e) "]")
        Ok _ -> "unexpected-ok"

-- `Raw.exec` already reports the failure as a typed `DbError`; matching its
-- `kind` field tags it, the consumer shape data-layer callers use.
pub fn db typedKind () -> Text =
    let conn = memAdapter ()
    match Raw.exec conn "DELETE FROM t" []
        Err e ->
            let typed = e
            match typed.kind
                Unsupported -> "unsupported"
                _ -> "other"
        Ok _ -> "unexpected-ok"

-- `dbErrorIsTransient` on the typed error: a serialization-failure code is
-- transient, the unique-violation one is not. `mkDbError` fabricates the typed
-- error from a code, the way the stdlib's own raised errors are built.
pub fn db transiency () -> Text =
    let serialization = mkDbError "db.error.40001" "could not serialize"
    let unique = mkDbError "db.error.23505" "duplicate key"
    if dbErrorIsTransient serialization then
        if dbErrorIsTransient unique then "both" else "transient-only"
    else
        "neither"
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"db-error-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"db\"]\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn db_error_classifies_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping db_error_classifies_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-db-error-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-db-error-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    assert!(
        artefacts.diagnostics.is_empty(),
        "expected a clean compile, got diagnostics: {:?}",
        artefacts.diagnostics
    );

    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|stem| {
            stem.starts_with("ridge_")
                && !matches!(
                    *stem,
                    "ridge_rt"
                        | "ridge_main_runner"
                        | "ridge_test_runner"
                        | "ridge_pg"
                        | "ridge_sup"
                        | "ridge_sqlite"
                        | "ridge_bench_runner"
                )
        })
        .expect("a user module")
        .to_owned();

    let expr = format!(
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['unsupportedKind','unsupportedConstraint']), halt()."
    );
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let want = |needle: &str| {
        assert!(
            stdout.contains(needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    };

    // A real `raw.unsupported` error classifies to `Unsupported`, matched through
    // the reconciled `DbErrorKind` in consumer code.
    want("unsupportedKind=unsupported");
    // The constraint accessor resolves and reads empty on a non-constraint error.
    want("unsupportedConstraint=[]");
}

/// Compile the workspace and evaluate each named export on the BEAM, returning
/// the `erl` run's `(stdout, stderr)`.
fn eval_exports(exports: &[&str]) -> (String, String) {
    let dir = tempfile::Builder::new()
        .prefix("ridge-db-error-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-db-error-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    assert!(
        artefacts.diagnostics.is_empty(),
        "expected a clean compile, got diagnostics: {:?}",
        artefacts.diagnostics
    );

    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|stem| {
            stem.starts_with("ridge_")
                && !matches!(
                    *stem,
                    "ridge_rt"
                        | "ridge_main_runner"
                        | "ridge_test_runner"
                        | "ridge_pg"
                        | "ridge_sup"
                        | "ridge_sqlite"
                        | "ridge_bench_runner"
                )
        })
        .expect("a user module")
        .to_owned();

    let names = exports
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(",");
    let expr = format!(
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,[{names}]), halt()."
    );
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");

    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn to_db_error_lifts_raw_error_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping to_db_error_lifts_raw_error_on_beam");
        return;
    }

    let (stdout, stderr) = eval_exports(&["typedKind", "transiency"]);

    // The same `raw.unsupported` failure, arriving typed from `Raw.exec` and
    // matched through the typed record's `kind` field.
    assert!(
        stdout.contains("typedKind=unsupported"),
        "expected `typedKind=unsupported`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // `dbErrorIsTransient` on typed errors fabricated by `mkDbError`: the
    // serialization-failure code is transient, the unique-violation one is not.
    assert!(
        stdout.contains("transiency=transient-only"),
        "expected `transiency=transient-only`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
