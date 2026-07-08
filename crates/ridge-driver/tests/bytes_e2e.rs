//! End-to-end check for the `Bytes` primitive and the `std.bytes` module on a real
//! BEAM.
//!
//! Bytes is carried as a raw binary, so this proves the whole loop through codegen
//! and the runtime:
//! - a hex string parses and renders back as canonical lowercase hex,
//! - an upper-case hex string normalises to lowercase,
//! - a UTF-8 string round-trips through `fromUtf8`/`toUtf8`, and its bytes render
//!   as the expected hex,
//! - `length`/`concat`/`empty` operate over the bytes (not their hex spelling),
//! - ordering compares byte by byte, prefix-shorter sorting first,
//! - `gen` mints n bytes from the `random` capability, and
//! - a malformed hex string is a recoverable `Err`, not a crash.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
-- `Bytes` is a prelude primitive and `std.bytes` is aliased as `Bytes` with no
-- import, the same as `Int`/`Float`/`Uuid`.

-- Parse hex or fall back to the empty byte string, so each probe is total.
fn bh (s: Text) -> Bytes =
    match Bytes.fromHex s
        Ok b  -> b
        Err _ -> Bytes.empty ()

-- parse then render: a canonical hex string comes back unchanged.
pub fn roundTrip () -> Text = Bytes.toHex (bh "deadbeef")

-- an upper-case hex string is normalised to canonical lowercase.
pub fn normalizeCase () -> Text = Bytes.toHex (bh "DEADBEEF")

-- a UTF-8 string round-trips: its bytes decode back to the same text.
pub fn utf8RoundTrip () -> Text =
    match Bytes.toUtf8 (Bytes.fromUtf8 "hello")
        Ok s  -> s
        Err _ -> "err"

-- the bytes of "AB" are 0x41 0x42, so they render as "4142".
pub fn utf8Hex () -> Text = Bytes.toHex (Bytes.fromUtf8 "AB")

-- length counts bytes, not hex characters: "deadbeef" is four bytes.
pub fn lenBytes () -> Text = Int.toText (Bytes.length (bh "deadbeef"))

-- the empty byte string has length zero.
pub fn emptyLen () -> Text = Int.toText (Bytes.length (Bytes.empty ()))

-- concat joins end to end: two bytes plus three bytes is five.
pub fn concatLen () -> Text =
    Int.toText (Bytes.length (Bytes.concat (bh "0102") (bh "030405")))

-- ordering compares byte by byte: 0x00 is below 0xff.
pub fn lessThan () -> Text =
    if Bytes.lt (bh "00") (bh "ff") then "lt" else "notlt"

-- a shorter byte string that is a prefix of a longer one sorts first.
pub fn prefixLess () -> Text =
    if Bytes.lt (bh "01") (bh "0100") then "lt" else "notlt"

-- a non-hex string is a recoverable Err, not a runtime failure.
pub fn badHex () -> Text =
    match Bytes.fromHex "zz"
        Ok _  -> "ok"
        Err _ -> "err"

-- an odd-length hex string is rejected.
pub fn oddHex () -> Text =
    match Bytes.fromHex "abc"
        Ok _  -> "ok"
        Err _ -> "err"

-- a generated value has the requested length.
pub fn random genLen () -> Text = Int.toText (Bytes.length (Bytes.gen 16))

-- two generated values differ (a collision on 8 random bytes is unlikely).
pub fn random genDistinct () -> Text =
    if Bytes.eq (Bytes.gen 8) (Bytes.gen 8) then "same" else "distinct"
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"bytes-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn bytes_module_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping bytes_module_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-bytes-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-bytes-e2e-cache-")
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
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|stem| stem.starts_with("ridge_module_"))
        .expect("a user module")
        .to_owned();

    let expr = format!(
        "io:format(\"roundTrip=~s~n\",[{module}:roundTrip()]), \
         io:format(\"normalizeCase=~s~n\",[{module}:normalizeCase()]), \
         io:format(\"utf8RoundTrip=~s~n\",[{module}:utf8RoundTrip()]), \
         io:format(\"utf8Hex=~s~n\",[{module}:utf8Hex()]), \
         io:format(\"lenBytes=~s~n\",[{module}:lenBytes()]), \
         io:format(\"emptyLen=~s~n\",[{module}:emptyLen()]), \
         io:format(\"concatLen=~s~n\",[{module}:concatLen()]), \
         io:format(\"lessThan=~s~n\",[{module}:lessThan()]), \
         io:format(\"prefixLess=~s~n\",[{module}:prefixLess()]), \
         io:format(\"badHex=~s~n\",[{module}:badHex()]), \
         io:format(\"oddHex=~s~n\",[{module}:oddHex()]), \
         io:format(\"genLen=~s~n\",[{module}:genLen()]), \
         io:format(\"genDistinct=~s~n\",[{module}:genDistinct()]), \
         halt()."
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

    for (probe, why) in [
        (
            "roundTrip=deadbeef",
            "a canonical hex string round-trips unchanged",
        ),
        (
            "normalizeCase=deadbeef",
            "an upper-case hex string is normalised to lowercase",
        ),
        (
            "utf8RoundTrip=hello",
            "a UTF-8 string round-trips through fromUtf8/toUtf8",
        ),
        ("utf8Hex=4142", "the bytes of \"AB\" render as hex 4142"),
        ("lenBytes=4", "length counts bytes, not hex characters"),
        ("emptyLen=0", "the empty byte string has length zero"),
        ("concatLen=5", "concat joins two and three bytes into five"),
        ("lessThan=lt", "ordering compares byte by byte"),
        (
            "prefixLess=lt",
            "a shorter prefix sorts before the longer string",
        ),
        ("badHex=err", "a non-hex string is a recoverable Err"),
        ("oddHex=err", "an odd-length hex string is rejected"),
        ("genLen=16", "a generated value has the requested length"),
        ("genDistinct=distinct", "two generated values differ"),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
