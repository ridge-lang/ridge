//! FFI-targets code-generation helper (T11 / T11.5).
//!
//! This module documents the extraction logic used to generate the
//! `BridgeTarget::RidgeStdlibLocal` lookup table emitted by per-consumer
//! build scripts.  It is the canonical reference implementation; each
//! consumer crate (e.g. `ridge-codegen-erl`) inlines a copy of this logic
//! in its own `build.rs` to avoid creating a Cargo dependency cycle.
//!
//! # What this module exports
//!
//! [`extract_ffi_decls`] — walks every `.ridge` file under `stdlib_dir` and
//! returns a sorted list of [`FfiDecl`] values: one per `@ffi`-decorated
//! `pub fn` (or private `fn`) declaration.
//!
//! [`extract_all_stdlib_decls`] (T11.5) — like `extract_ffi_decls` but also
//! covers pure-Ridge `pub fn` bodies (no `@ffi`).  For pure-Ridge functions
//! the `beam_module` is the Ridge dotted module name (e.g. `"std.list"`) and
//! `beam_fn` is the Ridge function name; `arity` is the count of top-level
//! `(...)` parameter groups on the signature line.
//!
//! # Cycle-break rationale (T11)
//!
//! The Cargo dependency graph today is:
//!
//! ```text
//! ridge-stdlib  -->  ridge-resolve, ridge-typecheck
//! ridge-codegen-erl  -->  ridge-ir, ridge-resolve, ridge-types, ridge-ast
//! ```
//!
//! Adding `ridge-codegen-erl` as a dependency of `ridge-stdlib` would create
//! a cycle (since `ridge-codegen-erl` dev-depends on pipeline crates that
//! depend on `ridge-resolve` which depends on `ridge-stdlib`).  The chosen
//! approach (matching T10) is per-consumer `build.rs`: each consumer that
//! needs the FFI-target table inlines its own text-level extraction.  No new
//! dependency edge is added to any crate.
//!
//! # Stable ordering
//!
//! Entries in the generated table are sorted by `(module, ridge_fn_name)`
//! so that the generated output is deterministic across rebuilds.

use std::path::{Path, PathBuf};

// ── Public types ──────────────────────────────────────────────────────────────

/// One `@ffi`-decorated function declaration discovered in a stdlib `.ridge` file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FfiDecl {
    /// Dotted Ridge module name, e.g. `"std.list"`.
    pub ridge_module: String,
    /// Ridge function name, e.g. `"length"`.
    pub ridge_fn: String,
    /// BEAM module from the `@ffi` attribute, e.g. `"erlang"`.
    pub beam_module: String,
    /// BEAM function name from the `@ffi` attribute, e.g. `"length"`.
    pub beam_fn: String,
    /// Arity from the `@ffi` attribute.
    pub arity: u32,
}

// ── Module order (all tiers) ──────────────────────────────────────────────────

/// All stdlib module names (tier-ordered, alphabetical within tier).
/// Only modules listed here are scanned for `@ffi` declarations.
pub const STDLIB_MODULES: &[&str] = &[
    // Tier 1
    "std.int",
    "std.float",
    "std.bool",
    "std.option",
    "std.result",
    // Tier 2
    "std.text",
    "std.list",
    "std.map",
    "std.set",
    // Tier 3
    "std.io",
    "std.fs",
    "std.time",
    "std.random",
    "std.env",
    "std.cli",
    "std.proc",
    // Tier 4
    "std.json",
    "std.net.http",
];

// ── Core extraction ───────────────────────────────────────────────────────────

/// Extract all `@ffi`-decorated function declarations from every stdlib `.ridge`
/// file under `stdlib_dir`.
///
/// Returns a sorted list of [`FfiDecl`] values (sorted by `ridge_module` then
/// `ridge_fn`), so that the generated Rust output is deterministic.
///
/// Only `pub fn` and private `fn` declarations are matched; other constructs
/// are ignored.  Only modules listed in [`STDLIB_MODULES`] are scanned.
///
/// **T11.5 note:** Use [`extract_all_stdlib_decls`] to also include pure-Ridge
/// `pub fn` bodies (no `@ffi`), which is what `ridge-codegen-erl`'s build
/// script now uses to cover the full path-B table.
///
/// # Errors
///
/// Returns `Err(String)` if any `.ridge` file that exists cannot be read.
pub fn extract_ffi_decls(stdlib_dir: &Path) -> Result<Vec<FfiDecl>, String> {
    let mut decls: Vec<FfiDecl> = Vec::new();

    for &dotted in STDLIB_MODULES {
        let rel = module_name_to_path(dotted);
        let full = stdlib_dir.join(&rel);

        if !full.exists() {
            continue;
        }

        let src = std::fs::read_to_string(&full)
            .map_err(|e| format!("T11 FfiTargetGen: could not read {}: {e}", full.display()))?;

        extract_ffi_from_source(dotted, &src, &mut decls);
    }

    // Stable, deterministic order: sort by (module, ridge_fn).
    decls.sort();

    Ok(decls)
}

/// Map a dotted module name to its relative `.ridge` path under `stdlib/`.
///
/// `"std.int"`      → `int.ridge`
/// `"std.net.http"` → `net/http.ridge`
#[must_use]
pub fn module_name_to_path(dotted: &str) -> PathBuf {
    let rest = dotted.strip_prefix("std.").unwrap_or(dotted);
    let with_slashes = rest.replace('.', "/");
    PathBuf::from(format!("{with_slashes}.ridge"))
}

/// Ridge capability keywords (as of 0.1.0).
const CAP_KEYWORDS: &[&str] = &["io", "fs", "net", "time", "random", "env", "proc"];

/// Extract `@ffi`-decorated `fn` declarations from a single `.ridge` source file,
/// pushing new [`FfiDecl`] values into `out`.
///
/// The extraction is line-based:
/// 1. When a line is `@ffi("module", "fn_name", arity)`, record the attribute.
/// 2. The immediately following `pub fn` or `fn` line provides the Ridge name.
///    Capability keywords between `fn` and the name are skipped.
/// 3. Only one `@ffi` attribute may appear before a declaration; any
///    extra blank lines or comments between `@ffi` and the `fn` reset state.
pub fn extract_ffi_from_source(module: &str, src: &str, out: &mut Vec<FfiDecl>) {
    // Pending @ffi attribute: (beam_module, beam_fn, arity).
    let mut pending: Option<(String, String, u32)> = None;

    for line in src.lines() {
        let trimmed = line.trim();

        // Skip blank lines and comment lines — they do NOT reset the pending
        // @ffi state, because the .ridge files sometimes have blank comment lines
        // between @ffi and the fn declaration.
        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }

        // Detect `@ffi("module", "fn_name", arity)`.
        if let Some(rest) = trimmed.strip_prefix("@ffi(") {
            if let Some(attr) = parse_ffi_attr(rest) {
                pending = Some(attr);
                continue;
            }
        }

        // Detect `pub fn` or `fn` declaration.
        let fn_rest = trimmed
            .strip_prefix("pub fn ")
            .map_or_else(|| trimmed.strip_prefix("fn "), Some);

        if let Some(rest) = fn_rest {
            if let Some((beam_module, beam_fn, arity)) = pending.take() {
                // Extract Ridge function name (skip capability keywords).
                if let Some(ridge_fn) = extract_fn_name(rest) {
                    out.push(FfiDecl {
                        ridge_module: module.to_owned(),
                        ridge_fn,
                        beam_module,
                        beam_fn,
                        arity,
                    });
                }
            }
            // Whether or not we had a pending @ffi, reset on any fn line.
            continue;
        }

        // Any other non-blank, non-comment, non-@ffi, non-fn line resets state.
        pending = None;
    }
}

// ── T11.5: widened extractor ──────────────────────────────────────────────────

/// Extract **all** public stdlib function declarations from every stdlib `.ridge`
/// file under `stdlib_dir` — both `@ffi`-decorated stubs and pure-Ridge `pub
/// fn` bodies.
///
/// For `@ffi` stubs the emitted `FfiDecl` uses the BEAM target from the
/// attribute (`beam_module`, `beam_fn`, `arity` as declared).
///
/// For pure-Ridge `pub fn` functions (no `@ffi`) the emitted `FfiDecl` uses:
/// - `beam_module` = the Ridge dotted module name (e.g. `"std.list"`), which
///   is the atom of the compiled stdlib BEAM module.
/// - `beam_fn` = the Ridge function name.
/// - `arity` = count of top-level `(...)` parameter groups on the signature.
///
/// Returns a sorted list (by `ridge_module`, then `ridge_fn`).
///
/// # Errors
///
/// Returns `Err(String)` if any `.ridge` file that exists cannot be read.
pub fn extract_all_stdlib_decls(stdlib_dir: &Path) -> Result<Vec<FfiDecl>, String> {
    let mut decls: Vec<FfiDecl> = Vec::new();

    for &dotted in STDLIB_MODULES {
        let rel = module_name_to_path(dotted);
        let full = stdlib_dir.join(&rel);

        if !full.exists() {
            continue;
        }

        let src = std::fs::read_to_string(&full)
            .map_err(|e| format!("T11.5 AllDeclGen: could not read {}: {e}", full.display()))?;

        extract_all_from_source(dotted, &src, &mut decls);
    }

    decls.sort();
    Ok(decls)
}

/// Extract all `pub fn` declarations (both `@ffi`-decorated and pure-Ridge)
/// from a single `.ridge` source file.
///
/// `@ffi`-decorated functions produce entries with the attribute's BEAM target.
/// Pure-Ridge `pub fn` (no `@ffi`) produce entries with `beam_module =
/// ridge_module` and `beam_fn = ridge_fn`.  Private `fn` declarations without
/// `@ffi` are skipped (they are implementation helpers).
pub fn extract_all_from_source(module: &str, src: &str, out: &mut Vec<FfiDecl>) {
    let mut pending: Option<(String, String, u32)> = None;

    for line in src.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("@ffi(") {
            if let Some(attr) = parse_ffi_attr(rest) {
                pending = Some(attr);
                continue;
            }
        }

        let is_pub = trimmed.starts_with("pub fn ");
        let fn_rest_opt = if is_pub {
            trimmed.strip_prefix("pub fn ")
        } else {
            trimmed.strip_prefix("fn ")
        };

        if let Some(rest) = fn_rest_opt {
            if let Some((beam_module, beam_fn, arity)) = pending.take() {
                // @ffi-decorated: emit with BEAM target from attribute.
                if let Some(ridge_fn) = extract_fn_name(rest) {
                    out.push(FfiDecl {
                        ridge_module: module.to_owned(),
                        ridge_fn,
                        beam_module,
                        beam_fn,
                        arity,
                    });
                }
            } else if is_pub {
                // T11.5: pure-Ridge public fn — BEAM target is compiled Ridge module.
                if let Some(ridge_fn) = extract_fn_name(rest) {
                    let arity = count_param_groups(rest, &ridge_fn);
                    out.push(FfiDecl {
                        ridge_module: module.to_owned(),
                        ridge_fn: ridge_fn.clone(),
                        beam_module: module.to_owned(),
                        beam_fn: ridge_fn,
                        arity,
                    });
                }
            }
            // Private fn without @ffi: reset and skip (no entry emitted).
            continue;
        }

        pending = None;
    }
}

/// Count the number of top-level `(...)` parameter groups in a Ridge function
/// signature line, starting from the text after the function name.
///
/// Capability keywords between the name and the first `(` are transparent.
/// Scanning stops at `->` at paren depth 0 (the return-type arrow).
#[must_use]
pub fn count_param_groups(rest: &str, fn_name: &str) -> u32 {
    let after_name = match rest.find(fn_name) {
        Some(idx) => &rest[idx + fn_name.len()..],
        None => return 0,
    };

    let mut count: u32 = 0;
    let mut depth: i32 = 0;
    let mut chars = after_name.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '(' => {
                if depth == 0 {
                    count += 1;
                }
                depth += 1;
            }
            ')' => {
                depth -= 1;
            }
            '-' if depth == 0 => {
                if chars.peek() == Some(&'>') {
                    break;
                }
            }
            _ => {}
        }
    }

    count
}

// ── Internal parsers ──────────────────────────────────────────────────────────

/// Parse the `@ffi(...)` attribute suffix (everything after the opening `(`).
///
/// Expected format: `"beam_module", "beam_fn", arity)`
///
/// Returns `None` on any parse failure (malformed attribute — generator skips it).
fn parse_ffi_attr(rest: &str) -> Option<(String, String, u32)> {
    // Strip optional trailing whitespace and the closing `)`.
    let rest = rest.trim_end_matches(')').trim();

    // Split on commas — expect exactly 3 parts.
    let parts: Vec<&str> = rest.splitn(3, ',').collect();
    if parts.len() != 3 {
        return None;
    }

    let beam_module = unquote(parts[0].trim())?;
    let beam_fn = unquote(parts[1].trim())?;
    let arity: u32 = parts[2].trim().parse().ok()?;

    Some((beam_module, beam_fn, arity))
}

/// Remove surrounding double-quotes from a string literal token.
fn unquote(s: &str) -> Option<String> {
    let s = s.strip_prefix('"')?.strip_suffix('"')?;
    Some(s.to_owned())
}

/// Extract the Ridge function name from the text after `fn ` (or `pub fn `),
/// skipping capability keywords.
fn extract_fn_name(rest: &str) -> Option<String> {
    let mut tokens = rest.split_whitespace();
    loop {
        let tok = tokens.next()?;
        if CAP_KEYWORDS.contains(&tok) {
            continue;
        }
        // First non-capability token is the function name; trim trailing `(`.
        let name = tok.trim_end_matches('(');
        if is_valid_ridge_ident(name) {
            return Some(name.to_owned());
        }
        return None;
    }
}

/// Return `true` if `s` is a plausible Ridge identifier.
fn is_valid_ridge_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => chars.all(|c| c.is_alphanumeric() || c == '_'),
        _ => false,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn decls_from(module: &str, src: &str) -> Vec<FfiDecl> {
        let mut out = Vec::new();
        extract_ffi_from_source(module, src, &mut out);
        out
    }

    #[test]
    fn parses_simple_ffi_pub_fn() {
        let src = "@ffi(\"erlang\", \"length\", 1)\npub fn length (xs: List a) -> Int\n";
        let decls = decls_from("std.list", src);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].ridge_fn, "length");
        assert_eq!(decls[0].beam_module, "erlang");
        assert_eq!(decls[0].beam_fn, "length");
        assert_eq!(decls[0].arity, 1);
    }

    #[test]
    fn parses_ffi_private_fn() {
        let src = "@ffi(\"lists\", \"nthtail\", 2)\nfn _nthtail (n: Int) (xs: List a) -> List a\n";
        let decls = decls_from("std.list", src);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].ridge_fn, "_nthtail");
    }

    #[test]
    fn skips_capability_keywords_in_fn_name() {
        let src = "@ffi(\"ridge_rt\", \"println\", 1)\npub fn io println (s: Text) -> Unit\n";
        let decls = decls_from("std.io", src);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].ridge_fn, "println");
        assert_eq!(decls[0].beam_module, "ridge_rt");
    }

    #[test]
    fn blank_line_between_ffi_and_fn_is_ok() {
        let src = "@ffi(\"erlang\", \"+\", 2)\n\npub fn add (a: Int) (b: Int) -> Int\n";
        let decls = decls_from("std.int", src);
        assert_eq!(
            decls.len(),
            1,
            "blank line between @ffi and fn must not reset state"
        );
        assert_eq!(decls[0].ridge_fn, "add");
    }

    #[test]
    fn comment_line_between_ffi_and_fn_is_ok() {
        let src =
            "@ffi(\"erlang\", \"+\", 2)\n-- some comment\npub fn add (a: Int) (b: Int) -> Int\n";
        let decls = decls_from("std.int", src);
        assert_eq!(
            decls.len(),
            1,
            "comment between @ffi and fn must not reset state"
        );
    }

    #[test]
    fn non_ffi_fn_produces_no_decl() {
        let src = "pub fn isEmpty (xs: List a) -> Bool =\n    match xs\n";
        let decls = decls_from("std.list", src);
        assert!(decls.is_empty());
    }

    #[test]
    fn multiple_ffi_decls_extracted() {
        let src = "\
@ffi(\"erlang\", \"not\", 1)
pub fn not (b: Bool) -> Bool

@ffi(\"erlang\", \"and\", 2)
pub fn and (a: Bool) (b: Bool) -> Bool
";
        let decls = decls_from("std.bool", src);
        assert_eq!(decls.len(), 2);
    }

    #[test]
    fn output_is_deterministically_sorted() {
        // Even if we push in reverse order, sort() must normalize.
        let src = "\
@ffi(\"erlang\", \"or\", 2)
pub fn or (a: Bool) (b: Bool) -> Bool

@ffi(\"erlang\", \"and\", 2)
pub fn and (a: Bool) (b: Bool) -> Bool
";
        let mut out = Vec::new();
        extract_ffi_from_source("std.bool", src, &mut out);
        // Do NOT sort here — test that extract_ffi_from_source itself is order-preserving;
        // it is extract_ffi_decls (the public API) that sorts.
        // Sorting test covered by extract_ffi_decls.
        assert_eq!(out[0].ridge_fn, "or");
        assert_eq!(out[1].ridge_fn, "and");
    }

    #[test]
    fn extract_ffi_decls_returns_sorted() {
        let stdlib_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib");
        if !stdlib_dir.exists() {
            return; // Guard: skip in bare source trees.
        }
        let decls = extract_ffi_decls(&stdlib_dir)
            .unwrap_or_else(|e| panic!("extract_ffi_decls failed: {e}"));
        // Verify sorted order.
        let mut prev: Option<&FfiDecl> = None;
        for d in &decls {
            if let Some(p) = prev {
                assert!(
                    p <= d,
                    "extract_ffi_decls must return sorted output; found {p:?} > {d:?}"
                );
            }
            prev = Some(d);
        }
        // Every decl must have non-empty fields.
        for d in &decls {
            assert!(!d.ridge_module.is_empty());
            assert!(!d.ridge_fn.is_empty());
            assert!(!d.beam_module.is_empty());
            assert!(!d.beam_fn.is_empty());
        }
    }

    // ── T11.5 tests ───────────────────────────────────────────────────────────

    #[test]
    fn count_param_groups_single_param() {
        // pub fn head (xs: List a) -> Option a
        assert_eq!(
            count_param_groups("head (xs: List a) -> Option a", "head"),
            1
        );
    }

    #[test]
    fn count_param_groups_two_params() {
        // pub fn drop (n: Int) (xs: List a) -> List a
        assert_eq!(
            count_param_groups("drop (n: Int) (xs: List a) -> List a", "drop"),
            2
        );
    }

    #[test]
    fn count_param_groups_nested_type() {
        // pub fn filterMap (f: fn a -> Option b) (xs: List a) -> List b
        assert_eq!(
            count_param_groups(
                "filterMap (f: fn a -> Option b) (xs: List a) -> List b",
                "filterMap"
            ),
            2
        );
    }

    #[test]
    fn count_param_groups_deeply_nested() {
        // pub fn update (k: k) (f: fn (Option v) -> v) (m: Map k v) -> Map k v
        assert_eq!(
            count_param_groups(
                "update (k: k) (f: fn (Option v) -> v) (m: Map k v) -> Map k v",
                "update"
            ),
            3
        );
    }

    #[test]
    fn extract_all_from_source_covers_pure_ridge_pub_fn() {
        // A pure-Ridge pub fn (no @ffi) must appear with beam_module = ridge_module.
        let src = "pub fn head (xs: List a) -> Option a =\n    match xs\n";
        let mut out = Vec::new();
        extract_all_from_source("std.list", src, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ridge_fn, "head");
        assert_eq!(out[0].beam_module, "std.list");
        assert_eq!(out[0].beam_fn, "head");
        assert_eq!(out[0].arity, 1);
    }

    #[test]
    fn extract_all_from_source_ffi_fn_uses_attribute() {
        // An @ffi-decorated pub fn must use the attribute's BEAM target.
        let src = "@ffi(\"erlang\", \"length\", 1)\npub fn length (xs: List a) -> Int\n";
        let mut out = Vec::new();
        extract_all_from_source("std.list", src, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ridge_fn, "length");
        assert_eq!(out[0].beam_module, "erlang");
        assert_eq!(out[0].beam_fn, "length");
        assert_eq!(out[0].arity, 1);
    }

    #[test]
    fn extract_all_from_source_private_fn_skipped() {
        // Private fn without @ffi must NOT appear in the output.
        let src = "fn _helper (x: Int) -> Int =\n    x\n";
        let mut out = Vec::new();
        extract_all_from_source("std.list", src, &mut out);
        assert!(out.is_empty(), "private fn without @ffi must not appear");
    }

    #[test]
    fn extract_all_from_source_no_duplicates_for_ffi_pub_fn() {
        // @ffi-decorated pub fn must appear exactly once (not once for @ffi
        // and once for pure-Ridge).
        let src = "@ffi(\"erlang\", \"length\", 1)\npub fn length (xs: List a) -> Int\n\
                   pub fn head (xs: List a) -> Option a =\n    match xs\n";
        let mut out = Vec::new();
        extract_all_from_source("std.list", src, &mut out);
        assert_eq!(out.len(), 2, "expected 1 @ffi entry + 1 pure-Ridge entry");
        // The @ffi entry should use erlang:length, not std.list:length.
        let length = out.iter().find(|d| d.ridge_fn == "length").unwrap();
        assert_eq!(length.beam_module, "erlang");
        // The pure-Ridge head entry should use std.list:head.
        let head = out.iter().find(|d| d.ridge_fn == "head").unwrap();
        assert_eq!(head.beam_module, "std.list");
    }

    #[test]
    fn extract_all_stdlib_decls_covers_pure_ridge_entries() {
        let stdlib_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib");
        if !stdlib_dir.exists() {
            return; // Guard: skip in bare source trees.
        }
        let decls = extract_all_stdlib_decls(&stdlib_dir)
            .unwrap_or_else(|e| panic!("extract_all_stdlib_decls failed: {e}"));

        // Smoke test: std.list.head must appear with beam_module = "std.list" (pure-Ridge).
        let head = decls
            .iter()
            .find(|d| d.ridge_module == "std.list" && d.ridge_fn == "head");
        assert!(
            head.is_some(),
            "std.list.head must be in extract_all_stdlib_decls output"
        );
        let head = head.unwrap();
        assert_eq!(
            head.beam_module, "std.list",
            "pure-Ridge head must use ridge module as beam_module"
        );
        assert_eq!(head.arity, 1);

        // std.option.withDefault must appear as pure-Ridge.
        let wd = decls
            .iter()
            .find(|d| d.ridge_module == "std.option" && d.ridge_fn == "withDefault");
        assert!(
            wd.is_some(),
            "std.option.withDefault must be in extract_all_stdlib_decls output"
        );
        let wd = wd.unwrap();
        assert_eq!(wd.beam_module, "std.option");
        assert_eq!(wd.arity, 2);

        // std.list.map must appear with @ffi BEAM target (erlang is NOT the target; it's lists:map).
        let map = decls
            .iter()
            .find(|d| d.ridge_module == "std.list" && d.ridge_fn == "map");
        assert!(
            map.is_some(),
            "std.list.map must be in extract_all_stdlib_decls output"
        );
        let map = map.unwrap();
        assert_eq!(
            map.beam_module, "lists",
            "std.list.map must use @ffi's beam_module = 'lists'"
        );

        // Verify sorted order.
        let mut prev: Option<&FfiDecl> = None;
        for d in &decls {
            if let Some(p) = prev {
                assert!(p <= d, "must be sorted; {p:?} > {d:?}");
            }
            prev = Some(d);
        }
    }
}
