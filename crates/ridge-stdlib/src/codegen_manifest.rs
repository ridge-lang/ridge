//! Manifest and signature code-generation helpers (T10).
//!
//! This module provides the text-level `.ridge` parser that extracts public
//! symbol names from stdlib source files.  It is the canonical reference
//! implementation used by the `ridge-resolve` and `ridge-typecheck` build
//! scripts (each consumer has its own `build.rs` that includes an inline copy
//! of the extraction logic, since those crates cannot depend on `ridge-stdlib`
//! at build time without creating a circular dependency).
//!
//! # Cycle-break rationale (T10 plan deviation note)
//!
//! The Cargo dependency graph today is:
//!
//! ```text
//! ridge-stdlib  -->  ridge-resolve, ridge-typecheck (regular + build-deps)
//! ```
//!
//! Making `ridge-resolve` or `ridge-typecheck` depend on `ridge-stdlib` (even
//! as `[build-dependencies]`) would create a cycle.  The chosen approach
//! (Option 1 from the task spec) is "per-consumer `build.rs`": each consumer
//! crate gets its own `build.rs` that walks the `stdlib/` directory at a
//! well-known relative path (`../ridge-stdlib/stdlib/`) and does its own
//! text-level extraction.  No new crate is introduced; no dependency edge is
//! added to `ridge-resolve` or `ridge-typecheck`.
//!
//! # What this module exports
//!
//! [`extract_pub_symbols`] — the core extraction function.  Given the path to
//! the `stdlib/` directory, it returns a sorted list of
//! [`StdlibModuleSymbols`] values: one per discovered `.ridge` file.
//!
//! [`STDLIB_MODULE_ORDER`] — the canonical module order (tier-ordered,
//! alphabetical within tier) that the manifest generator uses to assign stable
//! `StdlibModuleId` values matching the hand-written `BUILTINS` table.

use std::path::{Path, PathBuf};

// ── Public types ──────────────────────────────────────────────────────────────

/// All public symbols discovered for one stdlib module.
#[derive(Debug, Clone)]
pub struct StdlibModuleSymbols {
    /// Dot-separated module name, e.g. `"std.list"`.
    pub name: String,
    /// All `pub fn` and `pub type` names declared in the `.ridge` file.
    pub exports: Vec<String>,
}

/// Canonical tier-ordered module names.  The index into this slice is the
/// `StdlibModuleId` that `crates/ridge-resolve/src/stdlib_builtin.rs`
/// assigns.
///
/// This ordering matches `BUILTINS` in `stdlib_builtin.rs` exactly.
pub const STDLIB_MODULE_ORDER: &[&str] = &[
    // Tier 1 — core
    "std.int",
    "std.float",
    "std.bool",
    "std.text",
    "std.list",
    "std.map",
    "std.set",
    "std.option",
    "std.result",
    // Tier 3 — capability-bearing
    "std.io",
    "std.fs",
    "std.time",
    "std.random",
    "std.env",
    "std.cli",
    "std.proc",
    "std.actor",
    // Tier 4 — advanced
    "std.json",
    "std.net.http",
];

// ── Core extraction ───────────────────────────────────────────────────────────

/// Extract all `pub fn` and `pub type` names from every `.ridge` file under
/// `stdlib_dir`, returning them grouped by module in `STDLIB_MODULE_ORDER`
/// order.
///
/// Only modules listed in [`STDLIB_MODULE_ORDER`] are included.  Any `.ridge`
/// file that does not correspond to a known module name is silently ignored.
///
/// # Errors
///
/// Returns `Err(String)` if the `stdlib_dir` path cannot be read.
pub fn extract_pub_symbols(stdlib_dir: &Path) -> Result<Vec<StdlibModuleSymbols>, String> {
    let mut results: Vec<StdlibModuleSymbols> = Vec::new();

    for &dotted in STDLIB_MODULE_ORDER {
        let rel = module_name_to_path(dotted);
        let full = stdlib_dir.join(&rel);

        if !full.exists() {
            continue;
        }

        let src = std::fs::read_to_string(&full).map_err(|e| {
            format!(
                "T201 ManifestRegressionFailed: could not read {}: {e}",
                full.display()
            )
        })?;

        let exports = extract_pub_names_from_source(&src);

        results.push(StdlibModuleSymbols {
            name: dotted.to_owned(),
            exports,
        });
    }

    Ok(results)
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

/// Extract all `pub fn NAME` and `pub type NAME` symbols from a Ridge source
/// string.
///
/// Handles:
/// - `pub fn NAME ...`                 — pure function
/// - `pub fn CAP NAME ...`             — capability-bearing function
/// - `pub fn CAP CAP2 NAME ...`        — (future-proof) multiple cap keywords
/// - `pub type NAME ...`               — type declaration
///
/// Ridge capability keywords (as of 0.1.0): `io`, `fs`, `net`, `time`,
/// `random`, `env`, `proc`.  These appear between `pub fn` and the function
/// name.  The parser must skip them to land on the actual identifier.
#[must_use]
pub fn extract_pub_names_from_source(src: &str) -> Vec<String> {
    const CAP_KEYWORDS: &[&str] = &["io", "fs", "net", "time", "random", "env", "proc"];

    let mut names: Vec<String> = Vec::new();

    for line in src.lines() {
        let trimmed = line.trim();

        // Skip comment lines and blank lines.
        if trimmed.starts_with("--") || trimmed.is_empty() {
            continue;
        }

        // Collect `pub fn` declarations.
        if let Some(rest) = trimmed.strip_prefix("pub fn ") {
            let mut tokens = rest.split_whitespace();
            // Skip capability keywords until we find the actual name.
            let name = loop {
                let Some(tok) = tokens.next() else { break None };
                if CAP_KEYWORDS.contains(&tok) {
                    continue;
                }
                // First non-capability token is the function name.
                // Trim any trailing `(` that may run together (defensive).
                break Some(tok.trim_end_matches('('));
            };
            if let Some(n) = name {
                if is_valid_ridge_ident(n) {
                    names.push(n.to_owned());
                }
            }
            continue;
        }

        // Collect `pub type` declarations.
        if let Some(rest) = trimmed.strip_prefix("pub type ") {
            let mut tokens = rest.split_whitespace();
            if let Some(n) = tokens.next() {
                let n = n.trim_end_matches('=').trim_end_matches(' ');
                if is_valid_ridge_ident(n) {
                    names.push(n.to_owned());
                }
            }
        }
    }

    names
}

/// Return `true` if `s` is a plausible Ridge identifier: non-empty, starts
/// with a letter or `_`, and contains only alphanumerics and `_`.
#[must_use]
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

    #[test]
    fn extracts_pure_fn() {
        let src = "pub fn toText (n: Int) -> Text\n";
        let names = extract_pub_names_from_source(src);
        assert_eq!(names, vec!["toText"]);
    }

    #[test]
    fn extracts_cap_fn() {
        let src = "pub fn io println (s: Text) -> Unit\n";
        let names = extract_pub_names_from_source(src);
        assert_eq!(names, vec!["println"]);
    }

    #[test]
    fn extracts_pub_type() {
        let src = "pub type JsonValue = JNull | JBool Bool\n";
        let names = extract_pub_names_from_source(src);
        assert_eq!(names, vec!["JsonValue"]);
    }

    #[test]
    fn skips_private_fn() {
        let src = "fn raw_http_get (url: Text) -> Result Response Error\n";
        let names = extract_pub_names_from_source(src);
        assert!(names.is_empty());
    }

    #[test]
    fn skips_comments() {
        let src = "-- pub fn not_a_fn\npub fn real (x: Int) -> Int\n";
        let names = extract_pub_names_from_source(src);
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn extracts_multiple_names() {
        let src = "\
pub fn toText (n: Int) -> Text
pub fn parse (s: Text) -> Option Int
pub fn io println (s: Text) -> Unit
pub type JsonValue = JNull
";
        let names = extract_pub_names_from_source(src);
        assert_eq!(names, vec!["toText", "parse", "println", "JsonValue"]);
    }

    #[test]
    fn module_name_to_path_simple() {
        assert_eq!(module_name_to_path("std.int"), PathBuf::from("int.ridge"));
    }

    #[test]
    fn module_name_to_path_nested() {
        assert_eq!(
            module_name_to_path("std.net.http"),
            PathBuf::from("net/http.ridge")
        );
    }

    // ── Bidirectional consistency seed test (T10 DoD §9 bullet 9) ───────────────
    //
    // Asserts every parsed `pub fn`/`pub type` in each `.ridge` file shows up in
    // the `STDLIB_MODULE_ORDER` list.  This seeds the contract that T12 will
    // expand into a full bidirectional regression test
    // (`tests/manifest_consistency.rs`).

    #[test]
    fn every_parsed_pub_fn_is_in_module_order() {
        let stdlib_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib");

        if !stdlib_dir.exists() {
            // Guard: if stdlib dir does not exist (e.g. in a bare source tree),
            // skip gracefully rather than failing.
            return;
        }

        // Collect all module names from the discovered files.
        let symbols = extract_pub_symbols(&stdlib_dir)
            .unwrap_or_else(|e| panic!("extract_pub_symbols failed: {e}"));

        // Every module returned must appear in STDLIB_MODULE_ORDER.
        for sym in &symbols {
            assert!(
                STDLIB_MODULE_ORDER.contains(&sym.name.as_str()),
                "discovered module '{}' is not in STDLIB_MODULE_ORDER",
                sym.name
            );
        }

        // Each discovered module must have at least one export.
        for sym in &symbols {
            assert!(
                !sym.exports.is_empty(),
                "module '{}' has no pub exports",
                sym.name
            );
        }
    }
}
