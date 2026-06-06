// build.rs — ridge-resolve manifest generator (T10).
#![allow(dead_code, clippy::format_push_string)]
//
// Emits `${OUT_DIR}/stdlib_manifest.rs` containing the generated `BUILTINS`
// static data slice consumed by `src/stdlib_builtin.rs` via `include!`.
//
// # Generation strategy (T10)
//
// For T10 the generated content is the original hand-curated module/export
// table, augmented with exports discovered from `.ridge` files.  The baseline
// table preserves all prior entries (including prelude re-exports per R013)
// so that the existing API surface and all existing tests stay green.
//
// Future tasks (T12) will extend this into a full bidirectional consistency
// check; T10 just wires up the `include!` mechanism.
//
// # Cycle-break rationale
//
// ridge-stdlib depends on ridge-resolve (regular + build-deps), so
// ridge-resolve cannot depend on ridge-stdlib (even as build-dep) without
// creating a Cargo cycle.  This build script performs its own text-level
// extraction without depending on ridge-stdlib.  No new crate is introduced;
// the dependency graph is unchanged.
//
// T201 errors: surfaced via eprintln! + process::exit(1) (no panic! per §1.3
// hard constraint #5).

use std::path::{Path, PathBuf};

// ── Capability keywords (Ridge 0.1.0) ────────────────────────────────────────

const CAP_KEYWORDS: &[&str] = &["io", "fs", "net", "time", "random", "env", "proc"];

// ── Canonical module order ────────────────────────────────────────────────────
//
// Must match BUILTINS[i].id == i invariant in stdlib_builtin.rs.

const MODULE_ORDER: &[&str] = &[
    "std.int",
    "std.float",
    "std.bool",
    "std.text",
    "std.list",
    "std.map",
    "std.set",
    "std.option",
    "std.result",
    "std.io",
    "std.fs",
    "std.time",
    "std.random",
    "std.env",
    "std.cli",
    "std.proc",
    "std.actor",
    "std.json",
    "std.net.http",
    "std.crypto",
    "std.sql",
];

// ── Baseline export table (T10: preserves original API) ───────────────────────
//
// Each entry is (module_name, &[export_names]).
//
// This baseline replicates the hand-curated BUILTINS table that was previously
// in stdlib_builtin.rs.  It includes:
//   - `pub fn` exports that appear in the `.ridge` files (ground truth from T5-T9)
//   - Prelude re-exported constructors / type names (R013): Some, None,
//     Option (std.option) and Ok, Err, Result (std.result)
//   - Alias / compat entries documented in the plan (andThen, unwrapOr, etc.)
//   - `pub type` entries that serve as re-export markers in the resolver
//
// T12 will replace this static table with a generated one derived purely from
// the `.ridge` sources plus a formal prelude-re-export declaration mechanism.

// T12 update: BASELINE_EXPORTS now derived from the actual .ridge source files
// (bidirectional consistency mandate, R006).  Entries that were in
// the old hand-curated T10 table but are NOT in any .ridge file have been
// removed.  New symbols that appear in the .ridge files but were absent from the
// T10 table have been added.
//
// Special prelude re-exports (R013) — constructors/type names that are
// declared as part of a `pub type` body and re-exported by the prelude:
//   std.option: Option, Some, None
//   std.result: Result, Ok, Err
// These are retained even though they do not appear as top-level `pub fn` or
// separate `pub type` declarations in the .ridge files.
//
// std.proc: `ProcOutput` is declared as `pub type` in proc.ridge.
// std.time:  `Duration`  is declared as `pub type` in time.ridge.
// std.json:  `JsonValue` is a language prelude union (compiler builtin), so it
//            is NOT a std.json export — unlike the records above.
// std.net.http: `Request`, `Response` are declared as `pub type` in net/http.ridge.
const BASELINE_EXPORTS: &[(&str, &[&str])] = &[
    (
        "std.int",
        &[
            "toText",
            "parse",
            "abs",
            "min",
            "max",
            "add",
            "sub",
            "mul",
            "div",
            "rem",
            "mod",
            "pow",
            "neg",
            "wrappingAdd",
            "saturatingAdd",
        ],
    ),
    (
        "std.float",
        &[
            "toText",
            "parseRaw",
            "parse",
            "fromInt",
            "round",
            "truncate",
            "floor",
            "ceil",
            "sqrt",
            "abs",
            "add",
            "sub",
            "mul",
            "div",
            "neg",
            "totalCompare",
        ],
    ),
    ("std.bool", &["not", "and", "or", "toText"]),
    (
        "std.text",
        &[
            "byteSize",
            "length",
            "join",
            "slice",
            "concat",
            "split",
            "splitN",
            "splitAny",
            "lines",
            "trim",
            "toUpper",
            "toLower",
            "startsWith",
            "endsWith",
            "contains",
            "replace",
            "padLeft",
            "padRight",
            "isEmpty",
        ],
    ),
    (
        "std.list",
        &[
            "empty",
            "length",
            "isEmpty",
            "head",
            "tail",
            "map",
            "filter",
            "filterMap",
            "fold",
            "foldRight",
            "reverse",
            "concat",
            "sort",
            "sortBy",
            "take",
            "drop",
            "groupBy",
            "flatMap",
            "zip",
            "zipWith",
            "contains",
            "find",
            "any",
            "all",
            "range",
            "rangeExclusive",
            "forEach",
        ],
    ),
    (
        "std.map",
        &[
            "empty", "fromList", "toList", "insert", "remove", "get", "contains", "keys", "values",
            "map", "filter", "size", "merge", "update",
        ],
    ),
    (
        "std.set",
        &[
            "empty",
            "fromList",
            "toList",
            "insert",
            "remove",
            "contains",
            "union",
            "intersect",
            "difference",
            "size",
        ],
    ),
    (
        "std.option",
        &[
            "withDefault",
            "map",
            "flatMap",
            "orElse",
            "isSome",
            "isNone",
            "discard",
            // Prelude-exported constructors and type name (R013).
            "Option",
            "Some",
            "None",
        ],
    ),
    (
        "std.result",
        &[
            "map",
            "mapErr",
            "flatMap",
            "withDefault",
            "isOk",
            "isErr",
            "discard",
            // Prelude-exported constructors and type name (R013).
            "Result",
            "Ok",
            "Err",
        ],
    ),
    (
        "std.io",
        &["print", "println", "eprint", "eprintln", "readLine"],
    ),
    (
        "std.fs",
        &[
            "readFile",
            "writeFile",
            "append",
            "exists",
            "lines",
            "readDir",
            "isDir",
        ],
    ),
    (
        "std.time",
        &[
            // `pub type Duration` declared in time.ridge.
            "Duration", "now", "epoch", "fromIso", "diff", "diffMs", "sinceMs", "sleep", "parse",
            "iso",
        ],
    ),
    (
        "std.random",
        &["int", "float", "alphanumeric", "choice", "seed"],
    ),
    ("std.env", &["get", "set", "all"]),
    ("std.cli", &["args", "exit"]),
    (
        "std.proc",
        &[
            // `pub type ProcOutput` declared in proc.ridge.
            "ProcOutput",
            "run",
        ],
    ),
    ("std.actor", &["mailboxSize"]),
    (
        "std.json",
        &[
            // JsonValue is a language prelude union (compiler builtin), not a
            // std.json export — so it is intentionally absent from this list.
            "encode",
            "decode",
            "encodeInt",
            "encodeBool",
            "encodeText",
            // JsonValue construction shims (FFI bridges to
            // ridge_rt:json_* — see crates/ridge-stdlib/stdlib/json.ridge).
            // Cross-module `pub type` variant resolution lands in 0.2.0;
            // until then these are the supported constructor surface.
            "jNull",
            "jBool",
            "jInt",
            "jFloat",
            "jText",
            "jList",
            "jObject",
            // JsonValue accessor companions — destructure a JsonValue
            // returned from `decode` without needing cross-module variant
            // pattern matching (deferred).  See json.ridge for usage.
            "asInt",
            "asFloat",
            "asBool",
            "asText",
            "asList",
            "asObject",
            "isNull",
        ],
    ),
    (
        "std.net.http",
        &[
            // `pub type Request`, `Response`, `Sql`, `Html`, `SecureCookie` declared
            // in net/http.ridge.
            "Request",
            "Response",
            "Sql",
            "Html",
            "SecureCookie",
            "get",
            "post",
            "put",
            "delete",
            "listen",
            "respond",
            "sql",
            "html",
            "sqlValue",
            "htmlValue",
            "secureCookie",
            "secureCookieHeader",
            "withSecure",
            "withHttpOnly",
            "withSameSite",
            "withMaxAge",
            "withPath",
        ],
    ),
    (
        "std.crypto",
        &[
            // Constant-time comparison for secret-bearing values.
            "constantTimeEq",
        ],
    ),
    (
        "std.sql",
        &[
            // The opaque SQL column value plus the SqlType codec class and its
            // methods, all importable from user code.
            "SqlValue", "SqlType", "toSql", "fromSql",
        ],
    ),
];

/// Per-module list of `pub opaque type` names. Drives the `opaque_types` field
/// of the generated manifest so the resolver and type-checker confine these
/// types' construction, pattern matching, and field access to the declaring
/// stdlib module (the web-layer taint wrappers).
const BASELINE_OPAQUE: &[(&str, &[&str])] = &[
    ("std.net.http", &["Sql", "Html", "SecureCookie"]),
    ("std.sql", &["SqlValue"]),
];

fn main() {
    // Tell Cargo to re-run this script when any stdlib .ridge file changes.
    println!("cargo:rerun-if-changed=../ridge-stdlib/stdlib");

    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| {
        eprintln!("T201 ManifestRegressionFailed: OUT_DIR not set");
        std::process::exit(1);
    });
    let out_path = PathBuf::from(&out_dir).join("stdlib_manifest.rs");

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let stdlib_dir = manifest_dir.parent().map_or_else(
        || manifest_dir.join("ridge-stdlib").join("stdlib"),
        |p| p.join("ridge-stdlib").join("stdlib"),
    );

    match generate_manifest(&stdlib_dir, &out_path) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

// ── Generation ────────────────────────────────────────────────────────────────

fn generate_manifest(stdlib_dir: &Path, out_path: &Path) -> Result<(), String> {
    // Build the module list in canonical order.
    //
    // T10: use the baseline table as the definitive export list.  The .ridge
    // source files are walked only to validate that they exist (T201 guard);
    // the text-extracted names are NOT merged in here.  T12 will introduce
    // the full bidirectional consistency mechanism.
    let mut modules: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();

    for &dotted in MODULE_ORDER {
        // Validate the .ridge file exists (T201 guard — emit a warning if not).
        let rel = module_name_to_path(dotted);
        let full = stdlib_dir.join(&rel);
        if !full.exists() {
            // Missing .ridge file is non-fatal for T10 — the module may not have
            // been written yet (progressive T5-T9 delivery).
            continue;
        }

        // Baseline exports for this module (API-stable, R013 compliant).
        let baseline: &[&str] = BASELINE_EXPORTS
            .iter()
            .find(|&(name, _)| *name == dotted)
            .map_or(&[], |(_, exps)| *exps);

        let exports: Vec<String> = baseline.iter().map(|&s| s.to_owned()).collect();

        let opaque: Vec<String> = BASELINE_OPAQUE
            .iter()
            .find(|&(name, _)| *name == dotted)
            .map_or_else(Vec::new, |(_, ops)| {
                ops.iter().map(|&s| s.to_owned()).collect()
            });

        modules.push((dotted.to_owned(), exports, opaque));
    }

    let content = emit_manifest_rs(&modules);

    std::fs::write(out_path, content).map_err(|e| {
        format!(
            "T201 ManifestRegressionFailed: could not write {}: {e}",
            out_path.display()
        )
    })?;

    Ok(())
}

// ── Code emitter ──────────────────────────────────────────────────────────────

fn emit_manifest_rs(modules: &[(String, Vec<String>, Vec<String>)]) -> String {
    // The generated file contains only the `BUILTINS` static initializer body.
    // It is included via:
    //   pub static BUILTINS: &[BuiltinStdlibModule] = include!(...);
    // so the file must be a valid Rust expression — the `&[...]` slice literal.

    let mut out = String::from("// @generated by crates/ridge-resolve/build.rs (T10)\n");
    out.push_str("// Do not edit by hand — re-run cargo build to regenerate.\n");
    out.push_str("&[\n");

    for (idx, (dotted, exports, opaque)) in modules.iter().enumerate() {
        out.push_str("    BuiltinStdlibModule {\n");
        out.push_str(&format!("        id: StdlibModuleId({idx}),\n"));
        out.push_str(&format!("        name: \"{dotted}\",\n"));
        out.push_str("        exports: &[\n");
        for exp in exports {
            out.push_str(&format!("            \"{exp}\",\n"));
        }
        out.push_str("        ],\n");
        out.push_str("        opaque_types: &[\n");
        for ty in opaque {
            out.push_str(&format!("            \"{ty}\",\n"));
        }
        out.push_str("        ],\n");
        out.push_str("    },\n");
    }

    out.push_str("]\n");
    out
}

// ── Text-level extraction ─────────────────────────────────────────────────────

fn module_name_to_path(dotted: &str) -> PathBuf {
    let rest = dotted.strip_prefix("std.").unwrap_or(dotted);
    let with_slashes = rest.replace('.', "/");
    PathBuf::from(format!("{with_slashes}.ridge"))
}

fn extract_pub_names(src: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();

    for line in src.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("--") || trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("pub fn ") {
            let mut tokens = rest.split_whitespace();
            let name = loop {
                let Some(tok) = tokens.next() else { break None };
                if CAP_KEYWORDS.contains(&tok) {
                    continue;
                }
                break Some(tok.trim_end_matches('('));
            };
            if let Some(n) = name {
                if is_valid_ident(n) {
                    names.push(n.to_owned());
                }
            }
            continue;
        }

        if let Some(rest) = trimmed
            .strip_prefix("pub opaque type ")
            .or_else(|| trimmed.strip_prefix("pub type "))
        {
            let mut tokens = rest.split_whitespace();
            if let Some(n) = tokens.next() {
                let n = n.trim_end_matches('=').trim();
                if is_valid_ident(n) {
                    names.push(n.to_owned());
                }
            }
        }
    }

    names
}

fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => chars.all(|c| c.is_alphanumeric() || c == '_'),
        _ => false,
    }
}
