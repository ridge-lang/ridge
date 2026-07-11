//! A decimal literal in a match pattern matches by value, not structurally.
//!
//! `1.5` and `1.50` are equal as decimals but stored at different scales, so an
//! ordinary structural pattern would match one written form and miss the other.
//! A decimal-literal pattern therefore lowers to a fresh binding plus a
//! `Decimal.compare … == 0` guard: `1.5m` matches a `1.50` scrutinee, a nested
//! decimal in a tuple works, and the guard conjoins with a user `when` clause.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
-- Top-level decimal arms plus a wildcard fall-through. Match is by value, so a
-- `1.50m` scrutinee hits the `1.5m` arm and `2.000m` hits the `2m` arm.
pub fn classify (d: Decimal) -> Text =
    match d
        1.5m -> "half"
        2m   -> "two"
        _    -> "other"

-- A decimal nested in a tuple pattern, combined with a user `when` guard. The
-- first arm fires only when both the value comparison and the guard hold.
pub fn describe (pair: (Decimal, Text)) -> Text =
    match pair
        (0.10m, tag) when tag == "x" -> $"dime-${tag}"
        (x, _)                       -> $"amt-${x}"

pub fn probe () -> Text =
    $"${classify 1.50m}|${classify 2.000m}|${classify 9.9m}|${describe (0.1m, "x")}|${describe (5.0m, "y")}"
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"decimal-match-pattern-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn decimal_literal_patterns_match_by_value() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping decimal_literal_patterns_match_by_value");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-decimal-match-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-decimal-match-e2e-cache-")
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
        "a decimal-literal match pattern must compile cleanly; got {:?}",
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

    let expr = format!("io:format(\"probe=~s~n\",[{module}:probe()]), halt().");
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

    // classify: 1.50m==1.5m -> half, 2.000m==2m -> two, 9.9m -> other (wildcard).
    // describe: (0.1m == 0.10m) && "x"=="x" -> dime-x; (5.0m, "y") falls through
    // to the second arm -> amt-5.0.
    assert!(
        stdout.contains("probe=half|two|other|dime-x|amt-5.0"),
        "expected `probe=half|two|other|dime-x|amt-5.0`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
