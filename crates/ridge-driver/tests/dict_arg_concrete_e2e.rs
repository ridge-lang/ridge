//! Two dictionaries of one class inside one function body.
//!
//! A constrained function forwards its own dictionary on one branch and needs
//! a different, concrete one on the other. Both instances are user-defined and
//! their methods reject each other's data, so passing the wrong one crashes
//! rather than quietly returning something plausible — which is what makes
//! this readable as a test at all.
//!
//! The existing `typeclass_dict_e2e` covers a static call site and a
//! forwarding one, in separate functions. One function needing both is the
//! case that was missing, and the one that was wrong.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// `pick` is `false`, so `describe MkTag` runs and must reach `tagToText`.
/// Handing it the `Show Color` dictionary the caller supplied puts a `Tag`
/// into `colorToText`, whose `match` has no clause for it.
const SOURCE: &str = r#"
class Show a =
    toText (x: a) -> Text

type Color = Red | Green | Blue
type Tag = MkTag | NoTag

fn colorToText (c: Color) -> Text =
    match c
        Red   -> "red"
        Green -> "green"
        Blue  -> "blue"

fn tagToText (t: Tag) -> Text =
    match t
        MkTag -> "tag"
        NoTag -> "notag"

instance Show Color =
    toText (c: Color) -> Text = colorToText c

instance Show Tag =
    toText (t: Tag) -> Text = tagToText t

fn describe (x: a) -> Text where Show a =
    $"got:${x}"

fn twoDicts (x: a) (pick: Bool) -> Text where Show a =
    if pick then describe x else describe MkTag

pub fn main_concrete () -> Text =
    twoDicts Red false

pub fn main_forwarded () -> Text =
    twoDicts Green true
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"dict-arg-concrete-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn a_concrete_call_gets_its_own_dictionary_not_the_callers() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping a_concrete_call_gets_its_own_dictionary");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-dict-arg-concrete-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-dict-arg-concrete-cache-")
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
        "expected a clean compile; got {:?}",
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
         lists:foreach(F,['main_concrete','main_forwarded']), halt()."
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

    // The branch the argument named. Before this was fixed the caller's
    // `Show Color` was forwarded here and `colorToText` was handed a `Tag`,
    // so the failure was an `if_clause` crash rather than a wrong string.
    assert!(
        stdout.contains("main_concrete=got:tag"),
        "expected `main_concrete=got:tag`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // The other branch, which must still forward. Without this the fix could
    // be "never consult the caller", which would break every polymorphic call
    // site in the language and pass the assertion above.
    assert!(
        stdout.contains("main_forwarded=got:green"),
        "expected `main_forwarded=got:green`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
