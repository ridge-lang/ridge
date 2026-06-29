//! End-to-end check for the typed `DbErrorKind` classification — running on the BEAM.
//!
//! `dbErrorKind` reads a raw storage `Error`'s code into a typed kind, so consumer
//! code branches on a failure's cause — recover from a `UniqueViolation`, retry a
//! `ConnectionError` — rather than string-matching the code. The accessors
//! `dbErrorConstraint`/`dbErrorColumn` read the constraint or column a backend
//! named.
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
import std.data (memAdapter, MemAdapter, dbErrorKind, dbErrorConstraint, DbErrorKind, UniqueViolation, ForeignKeyViolation, NotNullViolation, CheckViolation, ConnectionError, DecodeError, Unsupported, QueryError)
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
-- `raw.unsupported` code — a real `Error` to classify.
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
        .find(|stem| stem.starts_with("ridge_module_"))
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
