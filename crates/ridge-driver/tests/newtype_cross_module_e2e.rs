//! End-to-end cross-module dispatch on the BEAM.
//!
//! A library module defines `opaque type Money = { cents: Int } deriving
//! (SqlType, …)`; a separate app module imports it and round-trips a `Money`
//! through `toSql`/`fromSql`. This exercises two cross-module paths that were
//! previously unsupported (`SymbolRef::External` was unimplemented in codegen):
//!
//! 1. cross-module **instance dispatch** — the consumer fetches `Money`'s
//!    `SqlType` dictionary from the module that declares `Money`;
//! 2. cross-module **function calls** — the consumer calls the `money`/`cents`
//!    helpers exported by that module.
//!
//! Both lower to qualified `'ridge_module_<id>':'fn'(…)` calls; the instance
//! dictionary const is exported from the producer module.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Sources ─────────────────────────────────────────────────────────────────

/// The producer module: an opaque newtype that derives `SqlType`, plus the
/// constructor/accessor helpers a consumer needs (the field itself is opaque).
const MODELS_SRC: &str = r#"
pub opaque type Money = { cents: Int } deriving (SqlType, Eq, Ord)

pub fn money (n: Int) -> Money = Money { cents = n }

pub fn cents (m: Money) -> Int = m.cents
"#;

/// The consumer module: imports `Money` and its helpers from another member and
/// `toSql`/`fromSql` from `std.sql`, then round-trips a value across modules.
const APP_SRC: &str = r#"
import models.Models (Money, money, cents)
import std.sql (toSql, fromSql, SqlValue)

fn fromMoney (v: SqlValue) -> Result Money Error = fromSql v

pub fn roundTrip (n: Int) -> Int =
    match fromMoney (toSql (money n))
        Ok m  -> cents m
        Err _ -> 0 - 1
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"nt-xmod-e2e\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    )
    .expect("write workspace manifest");

    let models_src = root.join("apps").join("models").join("src");
    std::fs::create_dir_all(&models_src).expect("create models dirs");
    std::fs::write(
        root.join("apps").join("models").join("ridge.toml"),
        "[project]\nname = \"models\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"**\"]\n",
    )
    .expect("write models manifest");
    std::fs::write(models_src.join("Models.ridge"), MODELS_SRC).expect("write models source");

    let app_src = root.join("apps").join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create app dirs");
    std::fs::write(
        root.join("apps").join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"**\"]\n",
    )
    .expect("write app manifest");
    std::fs::write(app_src.join("App.ridge"), APP_SRC).expect("write app source");
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn newtype_sqltype_roundtrip_across_modules_survives_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping cross-module newtype e2e");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-nt-xmod-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-nt-xmod-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    if !artefacts.diagnostics.is_empty() {
        eprintln!("COMPILE DIAGNOSTICS:");
        for d in &artefacts.diagnostics {
            eprintln!("  {d:?}");
        }
    }
    assert!(
        artefacts.diagnostics.is_empty(),
        "no compile errors expected; got {:?}",
        artefacts.diagnostics
    );

    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();

    // Two user modules are emitted (`ridge_module_<id>` each); only the app one
    // exports `roundTrip`. Try each under `catch` and assert one returns 1000.
    let modules: Vec<String> = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .filter(|stem| stem.starts_with("ridge_module_"))
        .map(str::to_owned)
        .collect();
    assert!(
        modules.len() >= 2,
        "expected at least two user modules (producer + consumer), got {modules:?}"
    );

    let module_list = modules
        .iter()
        .map(|m| format!("'{m}'"))
        .collect::<Vec<_>>()
        .join(",");
    let expr = format!(
        "lists:foreach(fun(M) -> catch io:format(\"r=~w~n\", [M:roundTrip(1000)]) end, [{module_list}]), halt()."
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

    assert!(
        stdout.contains("r=1000"),
        "expected `r=1000` — cross-module SqlType round-trip failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
