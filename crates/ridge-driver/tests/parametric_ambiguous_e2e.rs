//! Compile-error checks for parametric instances whose element type the caller
//! never pins.
//!
//! A parametric instance such as `Encode (Option a)` needs the element's
//! dictionary at runtime. When a call fixes the container head but leaves the
//! element open — a bare `toJson None`, or `toJson []` with no annotation — the
//! element type is genuinely ambiguous: there is no way to know which encoder to
//! use. The compiler must reject this with `T030` (ambiguous constraint) rather
//! than silently encoding the wrong value or emitting an unbound dictionary
//! variable that crashes on the BEAM.
//!
//! These tests run the type checker only (no OTP required).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace, CheckOptions};

fn write_workspace_source(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"ambig-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

fn diagnostic_codes(source: &str) -> Vec<&'static str> {
    let dir = tempfile::Builder::new()
        .prefix("ridge-ambig-e2e-")
        .tempdir()
        .expect("temp dir");
    write_workspace_source(dir.path(), source);
    let artefacts =
        check_workspace(CheckOptions::new(dir.path().to_path_buf())).expect("check workspace");
    artefacts.diagnostics.iter().map(|d| d.code).collect()
}

/// A bare `toJson None` — the `Option`'s element type is never constrained, so
/// the encoder for it cannot be chosen. The checker must report `T030`.
#[test]
fn ambiguous_option_element_emits_t030() {
    const SOURCE: &str = r"
class Encode a =
    encode (x: a) -> JsonValue

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main () -> Text =
    toJson None
";
    let codes = diagnostic_codes(SOURCE);
    assert!(
        codes.contains(&"T030"),
        "expected T030 for an unconstrained Option element; got {codes:?}"
    );
}

/// A bare `toJson []` — the list element type is open. Same ambiguity, same
/// `T030`.
#[test]
fn ambiguous_empty_list_element_emits_t030() {
    const SOURCE: &str = r"
class Encode a =
    encode (x: a) -> JsonValue

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main () -> Text =
    toJson []
";
    let codes = diagnostic_codes(SOURCE);
    assert!(
        codes.contains(&"T030"),
        "expected T030 for an unconstrained list element; got {codes:?}"
    );
}

/// Pinning the element type removes the ambiguity: `let o : Option Int = None`
/// then `toJson o` checks clean.
#[test]
fn pinned_option_element_checks_clean() {
    const SOURCE: &str = r"
class Encode a =
    encode (x: a) -> JsonValue

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main () -> Text =
    let o : Option Int = None
    toJson o
";
    let codes = diagnostic_codes(SOURCE);
    assert!(
        codes.is_empty(),
        "expected a clean check once the element type is pinned; got {codes:?}"
    );
}
