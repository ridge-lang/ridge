// build.rs — Ridge stdlib build-script orchestrator + FFI-targets generator.
//
// Includes the driver from `src/build_driver.rs` so that the same logic is
// shared between this build script (which has access to [build-dependencies])
// and the library crate (which exposes it as `ridge_stdlib::build_driver`).
//
// # FFI-targets extractor
//
// Emits `${OUT_DIR}/ffi_targets.rs` containing the generated
// `StdlibFfiTarget`-based lookup table consumed by `src/ffi_targets.rs` via
// `include!`.  This is the canonical extractor.  Relocated from
// `crates/ridge-codegen-erl/build.rs` which held a per-consumer copy as a
// defensive cycle-break.  The cycle is confirmed absent:
// `ridge-codegen-erl → ridge-stdlib` introduces no cycle.
//
// T201 errors: surfaced via eprintln! + process::exit(1) (no panic! per §1.3).

// Suppress lints that are not relevant in a build script context.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;

// The included file brings its own `use` statements and all public items,
// including `use std::path::{Path, PathBuf}`.
include!("src/build_driver.rs");

fn main() {
    // Re-run this script whenever the stdlib source directory changes.
    println!("cargo:rerun-if-changed=stdlib");

    let stdlib_dir = std::path::Path::new("stdlib");

    // ── build_driver (T4) ─────────────────────────────────────────────────────
    match build_all(stdlib_dir) {
        Ok(summary) => {
            // Only emit a warning when modules were actually compiled —
            // stay silent on the empty-stdlib smoke case.
            if !summary.modules_built.is_empty() {
                println!(
                    "cargo:warning=ridge-stdlib: built {} modules across {} tiers",
                    summary.modules_built.len(),
                    summary.tiers_built,
                );
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }

    // ── ffi_targets extractor (T14.5.3) ──────────────────────────────────────
    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| {
        eprintln!("T201 FfiTargetGen: OUT_DIR not set");
        std::process::exit(1);
    });
    let out_path = PathBuf::from(&out_dir).join("ffi_targets.rs");

    match generate_ffi_targets(stdlib_dir, &out_path) {
        Ok(n) => {
            println!("cargo:warning=ridge-stdlib: generated {n} ffi_targets entries");
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }

    // ── Source embedding (runtime stdlib unpacking) ──────────────────────────
    // Embed every `stdlib/**/*.ridge` file via `include_str!` so the resulting
    // binary carries its own stdlib sources. Released binaries can therefore
    // unpack the stdlib at runtime regardless of where they were built.
    let sources_out_path = PathBuf::from(&out_dir).join("stdlib_sources.rs");
    match generate_stdlib_sources_embed(stdlib_dir, &sources_out_path) {
        Ok(n) => {
            println!("cargo:warning=ridge-stdlib: embedded {n} source files");
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

// ── Capability keywords ───────────────────────────────────────────────────────

const CAP_KEYWORDS: &[&str] = &["io", "fs", "net", "time", "random", "env", "proc"];

// Constructor-shaped fns must export arity 0; this invariant catches accidental
// (_unit: Unit) regressions at build time. Hoisted to module scope (out of
// `generate_ffi_targets`) to satisfy `clippy::items_after_statements`.
const ARITY_0_CONSTRUCTORS: &[(&str, &str)] = &[
    ("std.list", "empty"),
    ("std.map", "empty"),
    ("std.set", "empty"),
];

// ── Module order ──────────────────────────────────────────────────────────────

const STDLIB_MODULES: &[&str] = &[
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
    "std.actor",
    // Tier 4
    "std.json",
    "std.net.http",
];

// ── Entry type ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FfiEntry {
    ridge_module: String,
    ridge_fn: String,
    beam_module: String,
    beam_fn: String,
    arity: u32,
}

// ── Generation ────────────────────────────────────────────────────────────────

fn generate_ffi_targets(stdlib_dir: &Path, out_path: &Path) -> Result<usize, String> {
    let mut entries: Vec<FfiEntry> = Vec::new();

    for &dotted in STDLIB_MODULES {
        let rel = module_to_path(dotted);
        let full = stdlib_dir.join(&rel);

        if !full.exists() {
            continue;
        }

        let src = std::fs::read_to_string(&full)
            .map_err(|e| format!("T201 FfiTargetGen: could not read {}: {e}", full.display()))?;

        extract_ffi(&src, dotted, &mut entries);
    }

    // Stable, deterministic sort: (module, fn_name).
    entries.sort();
    let n = entries.len();

    // The invariant table `ARITY_0_CONSTRUCTORS` lives at module scope (above)
    // to satisfy `clippy::items_after_statements`. The panic! is acceptable
    // here per §1.3 hard-constraint #10 — build.rs is a build-script, not a
    // user-reachable path, so the panic surfaces as a cargo error at compile
    // time.
    for (module, name) in ARITY_0_CONSTRUCTORS {
        let found = entries
            .iter()
            .find(|e| e.ridge_module == *module && e.ridge_fn == *name);
        match found {
            Some(entry) if entry.arity != 0 => {
                let arity = entry.arity;
                println!(
                    "cargo:warning=constructor {module}::{name} has arity {arity} (expected 0)"
                );
                panic!(
                    "constructor {module}::{name} declared with arity {arity} but invariant requires arity 0"
                );
            }
            None => {
                println!("cargo:warning=constructor {module}::{name} missing from FFI table");
                panic!(
                    "constructor {module}::{name} missing from FFI table but invariant requires it to be \
                     present at arity 0"
                );
            }
            _ => {}
        }
    }

    let content = emit_rs(&entries);
    std::fs::write(out_path, content).map_err(|e| {
        format!(
            "T201 FfiTargetGen: could not write {}: {e}",
            out_path.display()
        )
    })?;

    Ok(n)
}

// T11.5: extended to emit entries for pure-Ridge `pub fn` in addition to
// `@ffi`-decorated functions.  Pure-Ridge entries use the Ridge module name as
// the BEAM module atom (e.g. `"std.list"`) and the Ridge fn name as the BEAM fn
// name; arity is counted from the signature's top-level `(...)` param groups.
fn extract_ffi(src: &str, module: &str, out: &mut Vec<FfiEntry>) {
    // `pending` holds the parsed @ffi attribute for the immediately following fn.
    let mut pending: Option<(String, String, u32)> = None;

    for line in src.lines() {
        let t = line.trim();

        // Blank lines and comments do NOT reset pending state.
        if t.is_empty() || t.starts_with("--") {
            continue;
        }

        // Detect @ffi attribute.
        if let Some(rest) = t.strip_prefix("@ffi(") {
            if let Some(attr) = parse_ffi_attr(rest) {
                pending = Some(attr);
                continue;
            }
        }

        // Detect fn declaration (public or private).
        let is_pub = t.starts_with("pub fn ");
        let fn_rest_opt = if is_pub {
            t.strip_prefix("pub fn ")
        } else {
            t.strip_prefix("fn ")
        };

        if let Some(rest) = fn_rest_opt {
            if let Some((beam_module, beam_fn, arity)) = pending.take() {
                // @ffi-decorated: use the attribute's BEAM target.
                if let Some(ridge_fn) = extract_fn_name(rest) {
                    out.push(FfiEntry {
                        ridge_module: module.to_owned(),
                        ridge_fn,
                        beam_module,
                        beam_fn,
                        arity,
                    });
                }
            } else if is_pub {
                // T11.5: pure-Ridge public fn (no @ffi) — emit a StdlibFfiTarget
                // entry whose BEAM target is the compiled Ridge stdlib module.
                // Skip private fns: they are implementation helpers, not public API.
                if let Some(ridge_fn) = extract_fn_name(rest) {
                    let arity = count_param_groups(rest, &ridge_fn);
                    out.push(FfiEntry {
                        ridge_module: module.to_owned(),
                        ridge_fn: ridge_fn.clone(),
                        // beam_module = the Ridge dotted module name — the compiled
                        // stdlib BEAM module atom (e.g. 'std.list', 'std.option').
                        beam_module: module.to_owned(),
                        // beam_fn = the Ridge fn name (compiled without mangling).
                        beam_fn: ridge_fn,
                        arity,
                    });
                }
            }
            continue;
        }

        // Any other non-trivial line resets state.
        pending = None;
    }
}

/// Count the number of top-level `(...)` parameter groups in a Ridge fn
/// signature, starting from the text after the fn name.
///
/// The scan terminates at `->` (at paren depth 0) or end of string.
/// Capability keywords between the fn name and the first `(` are skipped.
fn count_param_groups(rest: &str, fn_name: &str) -> u32 {
    // Skip past the fn name token in `rest`.
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
                // Check for '->' (return-type arrow).
                if chars.peek() == Some(&'>') {
                    break;
                }
            }
            _ => {}
        }
    }

    count
}

fn parse_ffi_attr(rest: &str) -> Option<(String, String, u32)> {
    let rest = rest.trim_end_matches(')').trim();
    let parts: Vec<&str> = rest.splitn(3, ',').collect();
    if parts.len() != 3 {
        return None;
    }
    let bm = unquote(parts[0].trim())?;
    let bf = unquote(parts[1].trim())?;
    let ar: u32 = parts[2].trim().parse().ok()?;
    Some((bm, bf, ar))
}

fn unquote(s: &str) -> Option<String> {
    let s = s.strip_prefix('"')?.strip_suffix('"')?;
    Some(s.to_owned())
}

fn extract_fn_name(rest: &str) -> Option<String> {
    let mut tokens = rest.split_whitespace();
    loop {
        let tok = tokens.next()?;
        if CAP_KEYWORDS.contains(&tok) {
            continue;
        }
        let name = tok.trim_end_matches('(');
        if is_valid_ident(name) {
            return Some(name.to_owned());
        }
        return None;
    }
}

fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => chars.all(|c| c.is_alphanumeric() || c == '_'),
        _ => false,
    }
}

fn module_to_path(dotted: &str) -> PathBuf {
    let rest = dotted.strip_prefix("std.").unwrap_or(dotted);
    PathBuf::from(format!("{}.ridge", rest.replace('.', "/")))
}

// ── Emitter ───────────────────────────────────────────────────────────────────

fn emit_rs(entries: &[FfiEntry]) -> String {
    // The generated file provides:
    //   pub fn lookup(module: &str, name: &str) -> Option<&'static StdlibFfiTarget>
    // It is included via `include!(concat!(env!("OUT_DIR"), "/ffi_targets.rs"))` in
    // `src/ffi_targets.rs`.  The function references `StdlibFfiTarget` from the
    // parent module (declared in `src/ffi_targets.rs`).
    //
    // `StdlibFfiTarget` has `String` fields, so we cannot place instances in a
    // `static`.  We use `OnceLock<HashMap<...>>` to initialize lazily on first
    // lookup — mirroring the `BRIDGE_MAP` pattern in `ridge-codegen-erl`.
    // OnceLock cache chosen over per-call clone to avoid repeated allocation.

    let mut out = String::from("// @generated by crates/ridge-stdlib/build.rs\n");
    out.push_str("// Do not edit by hand — re-run cargo build to regenerate.\n");
    out.push_str("//\n");
    out.push_str("// Provides `lookup(module, name) -> Option<&'static StdlibFfiTarget>`\n");
    out.push_str("// consumed by `ridge-codegen-erl` (and future codegen backends) as the\n");
    out.push_str("// single source of truth for path-B stdlib FFI targets.\n");
    out.push_str("// Covers both @ffi stubs and pure-Ridge pub fn bodies.\n\n");

    out.push_str("use std::collections::HashMap;\n");
    out.push_str("use std::sync::OnceLock;\n\n");

    // Use String keys so the HashMap can be queried with &str without lifetime issues.
    out.push_str("type FfiMap = HashMap<String, StdlibFfiTarget>;\n\n");
    out.push_str("static FFI_MAP: OnceLock<FfiMap> = OnceLock::new();\n\n");

    // Emit the map-builder function.
    out.push_str("#[allow(clippy::too_many_lines)]\n");
    out.push_str("fn build_ffi_map() -> FfiMap {\n");
    out.push_str("    let mut m = HashMap::new();\n");
    let _ = writeln!(out, "    m.reserve({});", entries.len());

    for e in entries {
        // Key: "ridge_module::ridge_fn" (double-colon matches BRIDGE_MAP convention).
        let key = format!("{}::{}", e.ridge_module, e.ridge_fn);
        let _ = writeln!(out, "    m.insert(\"{key}\".to_owned(), StdlibFfiTarget {{");
        let _ = writeln!(
            out,
            "        beam_module: \"{}\".to_owned(),",
            e.beam_module
        );
        let _ = writeln!(out, "        fn_name: \"{}\".to_owned(),", e.beam_fn);
        let _ = writeln!(out, "        arity: {},", e.arity);
        out.push_str("    });\n");
    }

    out.push_str("    m\n");
    out.push_str("}\n\n");

    // Emit the lookup function.
    out.push_str("/// Look up the [`StdlibFfiTarget`] for a Ridge stdlib symbol.\n");
    out.push_str("///\n");
    out.push_str("/// Generated from stdlib `.ridge` declarations at build time (T14.5.3).\n");
    out.push_str("/// Covers both `@ffi`-decorated stubs (BEAM target from the attribute) and\n");
    out.push_str("/// pure-Ridge `pub fn` bodies (BEAM target = compiled Ridge stdlib module).\n");
    out.push_str("///\n");
    out.push_str("/// Returns `None` only for unknown symbols.  Consumers adapt the returned\n");
    out.push_str("/// [`StdlibFfiTarget`] into their own target representation at the seam.\n");
    out.push_str("#[must_use]\n");
    out.push_str("pub fn lookup(module: &str, name: &str) -> Option<&'static StdlibFfiTarget> {\n");
    out.push_str("    let map: &'static FfiMap = FFI_MAP.get_or_init(build_ffi_map);\n");
    out.push_str("    let key = format!(\"{module}::{name}\");\n");
    out.push_str("    map.get(&key)\n");
    out.push_str("}\n");

    out
}

// ── Stdlib source embedding ───────────────────────────────────────────────────

/// Walk `stdlib_dir` recursively, collect every `.ridge` file, and emit a
/// generated Rust file containing a `STDLIB_SOURCES` slice with one
/// `include_str!` entry per file. The slice is consumed at runtime to unpack
/// the stdlib into a tempdir before the driver compiles it.
fn generate_stdlib_sources_embed(stdlib_dir: &Path, out_path: &Path) -> Result<usize, String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "stdlib-sources-embed: CARGO_MANIFEST_DIR not set".to_string())?;
    let abs_stdlib_dir = PathBuf::from(&manifest_dir).join(stdlib_dir);

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_ridge_files(&abs_stdlib_dir, &abs_stdlib_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::from("// @generated by crates/ridge-stdlib/build.rs\n");
    out.push_str("// Do not edit by hand — re-run cargo build to regenerate.\n");
    out.push_str("//\n");
    out.push_str("// Embedded `.ridge` sources for the standard library. Each entry is\n");
    out.push_str("// `(relative_path, file_contents)`; `write_stdlib_sources_to` unpacks the\n");
    out.push_str("// slice into a destination directory at runtime.\n\n");
    out.push_str("pub static STDLIB_SOURCES: &[(&str, &str)] = &[\n");
    for (rel, abs) in &files {
        let abs_str = abs.to_string_lossy().replace('\\', "/");
        let _ = writeln!(out, "    ({rel:?}, include_str!({abs_str:?})),");
    }
    out.push_str("];\n");

    std::fs::write(out_path, &out).map_err(|e| {
        format!(
            "stdlib-sources-embed: could not write {}: {e}",
            out_path.display()
        )
    })?;

    Ok(files.len())
}

/// Recursive walk for `.ridge` files. `root` is the dir whose relative paths
/// we want in the output; `dir` is the current directory under traversal.
fn collect_ridge_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("stdlib-sources-embed: read_dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| format!("stdlib-sources-embed: dir entry {}: {e}", dir.display()))?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| format!("stdlib-sources-embed: file_type {}: {e}", path.display()))?;
        if ft.is_dir() {
            collect_ridge_files(root, &path, out)?;
        } else if ft.is_file() && path.extension().is_some_and(|e| e == "ridge") {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| format!("stdlib-sources-embed: strip_prefix: {e}"))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    Ok(())
}
