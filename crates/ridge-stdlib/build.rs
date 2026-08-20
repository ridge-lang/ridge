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
// `enum_variant_names` fires on `FfiDiag` (all variants share the `Ffi`
// prefix); the library crate silences it through the workspace lint config,
// which build scripts do not inherit, so it is repeated here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::enum_variant_names
)]

use std::fmt::Write as _;

// The build driver validates the stdlib's own `@ffi` declarations against the
// closed-list audit table before compiling. Both the validator and the audit
// table are normal library modules (`crate::ffi_validator`,
// `crate::ffi_caps_audit`); pull them in here under the same module paths the
// library exposes so the shared `build_driver.rs` source resolves identically
// whether it is compiled as part of the crate or `include!`d by this script.
#[path = "src/ffi_caps_audit.rs"]
mod ffi_caps_audit;
#[path = "src/ffi_validator.rs"]
mod ffi_validator;

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

    // ── stdlib-target table extractor ────────────────────────────────────────
    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| {
        eprintln!("stdlib target table: OUT_DIR not set");
        std::process::exit(1);
    });
    let out_path = PathBuf::from(&out_dir).join("stdlib_targets.rs");

    match generate_stdlib_targets(stdlib_dir, &out_path) {
        Ok(n) => {
            println!("cargo:warning=ridge-stdlib: generated {n} stdlib target entries");
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

const CAP_KEYWORDS: &[&str] = &["io", "fs", "net", "time", "random", "env", "proc", "db"];

// ── Stdlib typeclass names ────────────────────────────────────────────────────
//
// Typeclasses defined in the stdlib whose instance dictionaries are compiled
// into the stdlib module and must be exported (so user code can reference them
// cross-module). Each entry is `(class_name, home_ridge_module)`.
const STDLIB_CLASSES: &[(&str, &str)] = &[
    ("SqlType", "std.sql"),
    ("Adapter", "std.data"),
    ("Refinable", "std.repo"),
    ("Projectable", "std.repo"),
    ("Orderable", "std.repo"),
    ("Aggregable", "std.repo"),
    ("Fetchable", "std.repo"),
    ("Pageable", "std.repo"),
    ("Countable", "std.repo"),
    ("Every", "std.repo"),
    ("Groupable", "std.repo"),
    ("Summarizable", "std.repo"),
    ("Combinable", "std.repo"),
    ("Joinable", "std.repo"),
    ("JoinShape", "std.repo"),
    ("LeftJoinable", "std.repo"),
    ("RightJoinable", "std.repo"),
    ("FullJoinable", "std.repo"),
];

// Constructor-shaped fns must export arity 0; this invariant catches accidental
// (_unit: Unit) regressions at build time. Hoisted to module scope (out of
// `generate_stdlib_targets`) to satisfy `clippy::items_after_statements`.
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
    "std.decimal",
    "std.uuid",
    "std.bytes",
    "std.date",
    "std.timeofday",
    "std.error",
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
    // Tier 5
    "std.crypto",
    "std.sql",
    "std.schema",
    "std.query",
    "std.data",
    "std.repo",
    "std.migrate",
    "std.raw",
    "std.test",
];

// ── Entry type ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FfiEntry {
    ridge_module: String,
    ridge_fn: String,
    arity: u32,
    kind: EntryKind,
}

/// Which of the three answers a declaration gave about its implementation.
///
/// `RidgeModule` carries no names because there are none to carry: the module
/// and function are the Ridge ones, and writing them down a second time only
/// creates somewhere for a copy to be wrong.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EntryKind {
    Foreign { module: String, fn_name: String },
    RidgeModule,
    Primitive,
}

// ── Generation ────────────────────────────────────────────────────────────────

fn generate_stdlib_targets(stdlib_dir: &Path, out_path: &Path) -> Result<usize, String> {
    let mut entries: Vec<FfiEntry> = Vec::new();

    for &dotted in STDLIB_MODULES {
        let rel = module_to_path(dotted);
        let full = stdlib_dir.join(&rel);

        if !full.exists() {
            continue;
        }

        let src = std::fs::read_to_string(&full).map_err(|e| {
            format!(
                "stdlib target table: could not read {}: {e}",
                full.display()
            )
        })?;

        extract_ffi(&src, dotted, &mut entries)?;
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
            "stdlib target table: could not write {}: {e}",
            out_path.display()
        )
    })?;

    Ok(n)
}

// T11.5: extended to emit entries for pure-Ridge `pub fn` in addition to
// `@ffi`-decorated functions.  Pure-Ridge entries use the Ridge module name as
// the BEAM module atom (e.g. `"std.list"`) and the Ridge fn name as the BEAM fn
// name; arity is counted from the signature's top-level `(...)` param groups.
//
// Also emits bridge entries for generated instance-dict consts of stdlib-defined
// typeclasses (e.g. `$inst_SqlType_Int/0` in `std.sql`), so that
// `SymbolRef::Stdlib { module: "std.sql", name: "$inst_SqlType_Int" }` resolves
// to arity 0 and codegen emits `call 'std.sql':'$inst_SqlType_Int' ()`.
fn extract_ffi(src: &str, module: &str, out: &mut Vec<FfiEntry>) -> Result<(), String> {
    // What the attribute above the next `fn` line said, if anything.
    let mut pending: Option<EntryKind> = None;
    // `@ffi` declares its own arity; `@primitive` leaves this `None` and the
    // parameter list on the `fn` line supplies it.
    let mut pending_arity: Option<u32> = None;

    for line in src.lines() {
        let t = line.trim();

        // Blank lines and comments do NOT reset pending state.
        if t.is_empty() || t.starts_with("--") {
            continue;
        }

        // Detect @ffi attribute.
        if let Some(rest) = t.strip_prefix("@ffi(") {
            if let Some((module, fn_name, arity)) = parse_ffi_attr(rest) {
                pending = Some(EntryKind::Foreign { module, fn_name });
                pending_arity = Some(arity);
                continue;
            }
        }

        // Detect @primitive. It carries no arguments: the arity is the
        // declared parameter count, read off the `fn` line below.
        if t == "@primitive" {
            pending = Some(EntryKind::Primitive);
            pending_arity = None;
            continue;
        }

        // Any other attribute is one this scanner has not been taught. Refusing
        // is the whole point: falling through would read the declaration below
        // as an ordinary Ridge body and emit an entry pointing at a function
        // that does not exist, which no test downstream is looking for.
        if t.starts_with('@') {
            return Err(format!(
                concat!(
                    "stdlib target table: {m} declares `{a}`, an attribute this ",
                    "extractor does not know. Teach `extract_ffi` what it means ",
                    "before using it in the standard library."
                ),
                m = module,
                a = t
            ));
        }

        // Detect instance declarations for stdlib-defined typeclasses and emit a
        // bridge entry for the generated `$inst_<Class>_<Type>/0` dict const.
        // Matches: `instance <ClassName> <TypeName>` with optional trailing `=`.
        if let Some(rest) = t.strip_prefix("instance ") {
            if let Some((class_name, type_name, is_parametric)) = parse_instance_head(rest) {
                let is_stdlib_class = STDLIB_CLASSES
                    .iter()
                    .any(|(c, home)| *c == class_name && *home == module);
                if is_stdlib_class {
                    let dict_name = format!("$inst_{class_name}_{type_name}");
                    // A parametric instance (`instance SqlType (Option a) where
                    // SqlType a`) compiles its dict const as a function of one
                    // dictionary per `where` constraint; a monomorphic instance's
                    // dict const is a plain arity-0 value.
                    let arity = if is_parametric {
                        count_where_constraints(rest)
                    } else {
                        0
                    };
                    out.push(FfiEntry {
                        ridge_module: module.to_owned(),
                        ridge_fn: dict_name,
                        arity,
                        kind: EntryKind::RidgeModule,
                    });
                }
            }
            // Instance lines are not fn declarations; reset pending state.
            pending = None;
            pending_arity = None;
            continue;
        }

        // Detect fn declaration (public or private).
        let is_pub = t.starts_with("pub fn ");
        let fn_rest_opt = if is_pub {
            t.strip_prefix("pub fn ")
        } else {
            t.strip_prefix("fn ")
        };

        if let Some(rest) = fn_rest_opt {
            if let Some(kind) = pending.take() {
                if let Some(ridge_fn) = extract_fn_name(rest) {
                    // `@ffi` states its own arity; `@primitive` has only the
                    // parameter list, which is the arity by construction.
                    let arity = pending_arity.unwrap_or_else(|| {
                        count_param_groups(rest, &ridge_fn) + count_where_constraints(rest)
                    });
                    out.push(FfiEntry {
                        ridge_module: module.to_owned(),
                        ridge_fn,
                        arity,
                        kind,
                    });
                }
            } else if is_pub {
                // A pure-Ridge public fn compiles into the stdlib module of its
                // own name. Private fns are implementation helpers, not API.
                if let Some(ridge_fn) = extract_fn_name(rest) {
                    // A constrained fn (`where C a, D b`) compiles with one
                    // dictionary parameter prepended per constraint, so its BEAM
                    // arity is the value-parameter count plus the constraint
                    // count; call sites prepend the matching dict args.
                    let arity = count_param_groups(rest, &ridge_fn) + count_where_constraints(rest);
                    out.push(FfiEntry {
                        ridge_module: module.to_owned(),
                        ridge_fn,
                        arity,
                        kind: EntryKind::RidgeModule,
                    });
                }
            }
            pending_arity = None;
            continue;
        }

        // Any other non-trivial line resets state.
        pending = None;
        pending_arity = None;
    }

    Ok(())
}

/// Parse the head of an `instance` declaration to `(class_name, type_name,
/// is_parametric)`.
///
/// Accepts both `ClassName TypeName =` and `ClassName TypeName` (the `=` may be
/// on the same line or a subsequent line), plus a parametric head such as
/// `ClassName (Option a)`, whose type constructor is the first token inside the
/// parens. The `is_parametric` flag is `true` for the parenthesised form, so the
/// caller can size the dict const's arity by its `where` constraints.
fn parse_instance_head(rest: &str) -> Option<(String, String, bool)> {
    let rest = rest.trim();
    // The class name is the first whitespace-delimited token.
    let class_end = rest.find(char::is_whitespace)?;
    let class_name = &rest[..class_end];
    if !is_valid_ident(class_name) {
        return None;
    }

    // The head atoms run from after the class name to the body `=` or a `where`
    // clause. A single-parameter class (`SqlType Int`, `Adapter MemAdapter`) has
    // one atom; a multi-parameter class (`Refinable (Query e a) (fn e -> Bool)`)
    // has one per type argument, joined with `_` so the generated dict const name
    // matches the call-site reference (`$inst_Refinable_Query_Fn1`). A function-
    // type atom keys by its arity tycon `Fn{n}`, exactly as the arena and the
    // instance-definition lowering name it.
    let mut head = &rest[class_end..];
    if let Some(w) = head.find(" where ") {
        head = &head[..w];
    }

    let bytes = head.as_bytes();
    let mut i = 0;
    let mut names: Vec<String> = Vec::new();
    let mut any_paren = false;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'=' {
            break;
        }
        if bytes[i] == b'(' {
            any_paren = true;
            let start = i;
            let mut depth = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let inner = head[start..i].trim_matches(|c| c == '(' || c == ')').trim();
            names.push(instance_head_atom_name(inner)?);
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'(' {
                i += 1;
            }
            let tok = head[start..i].trim_end_matches('=').trim();
            if tok.is_empty() {
                continue;
            }
            names.push(instance_head_atom_name(tok)?);
        }
    }

    if names.is_empty() {
        return None;
    }
    // A fundep terminal class (`Refinable`/`Projectable`/…) over a nested-join
    // composite receiver (`Joined`/`LeftJoined`/`RightJoined`/`FullJoined`) keys its
    // dict by the RECEIVER ALONE: the dependency collapses the predicate, whose leaf
    // arity grows with the join depth, so the per-arity predicate atom is dropped to
    // match the receiver-only instance the typechecker resolves (see `discharge` in
    // ridge-typecheck and `lower_instance` in ridge-lower). A multi-atom head over one
    // of these composites is only ever a fundep terminal.
    let receiver_is_composite_join = matches!(
        names[0].as_str(),
        "Joined" | "LeftJoined" | "RightJoined" | "FullJoined"
    );
    let type_name = if names.len() > 1 && receiver_is_composite_join {
        names[0].clone()
    } else {
        names.join("_")
    };
    Some((class_name.to_owned(), type_name, any_paren))
}

/// The dict-const name fragment for one instance-head atom. A function type
/// (`fn e -> Bool`) keys by its arity tycon (`Fn1`); any other atom keys by its
/// head type constructor (`Query e a` → `Query`, `Int` → `Int`).
fn instance_head_atom_name(inner: &str) -> Option<String> {
    let inner = inner.trim();
    let is_fn = inner == "fn" || inner.starts_with("fn ");
    if is_fn {
        let after = inner.strip_prefix("fn").unwrap_or("").trim_start();
        let params = after.split("->").next().unwrap_or("");
        // Count top-level params: whitespace-separated groups at paren depth 0, so
        // a parenthesised parameter type counts once. `fn e f` is arity 2;
        // `fn e (Option f)` is also arity 2 (the left-join projection's right side),
        // matching how the type system keys the `Fn{n}` dispatch tycon — a naive
        // `split_whitespace` would miscount `(Option f)` as two and key `Fn3`.
        let mut arity = 0u32;
        let mut depth = 0i32;
        let mut prev_was_sep = true;
        for ch in params.chars() {
            match ch {
                '(' => {
                    if depth == 0 && prev_was_sep {
                        arity += 1;
                    }
                    depth += 1;
                    prev_was_sep = false;
                }
                ')' => {
                    depth -= 1;
                    prev_was_sep = false;
                }
                c if c.is_whitespace() => {
                    if depth == 0 {
                        prev_was_sep = true;
                    }
                }
                _ => {
                    if depth == 0 && prev_was_sep {
                        arity += 1;
                    }
                    prev_was_sep = false;
                }
            }
        }
        return Some(format!("Fn{arity}"));
    }
    let ctor = inner.split([' ', ')']).next().unwrap_or("").trim();
    if ctor.is_empty() || !is_valid_ident(ctor) {
        return None;
    }
    Some(ctor.to_owned())
}

/// Count the number of top-level `(...)` parameter groups in a Ridge fn
/// signature, starting from the text after the fn name.
///
/// Count the class constraints in a signature's `where` clause — one dictionary
/// parameter is compiled per constraint. `where Adapter a` yields 1, `where
/// Adapter a, Row e` yields 2, no `where` yields 0. The list runs from `where`
/// to the body's `=` and is comma-separated.
fn count_where_constraints(rest: &str) -> u32 {
    let Some(idx) = rest.find(" where ") else {
        return 0;
    };
    let after = &rest[idx + " where ".len()..];
    let list = after.split('=').next().unwrap_or(after).trim();
    if list.is_empty() {
        return 0;
    }
    u32::try_from(list.split(',').filter(|c| !c.trim().is_empty()).count()).unwrap_or(0)
}

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
                    // An empty top-level group `()` is the unit parameter list — zero
                    // params, matching how codegen compiles `fn f ()` to a 0-arity BEAM
                    // function; a non-empty group is one param group. Counting `()` as a
                    // param made a nullary stdlib fn's `ffi_targets` arity 1, so the
                    // Unit-paren call shim never dropped the `()` and a cross-module
                    // `f ()` compiled to an arity-1 call that was `undef`. Skip whitespace
                    // to the group's first char; only a non-`)` char is a real parameter.
                    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
                        chars.next();
                    }
                    if chars.peek() != Some(&')') {
                        count += 1;
                    }
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
    // The generated file is included by `src/stdlib_targets.rs`, which
    // declares `StdlibTarget`; everything below is written against that type.
    //
    // `StdlibTarget` has `String` fields, so instances cannot live in a
    // `static`. A `OnceLock<HashMap<..>>` initialises lazily on first lookup,
    // mirroring the `BRIDGE_MAP` pattern in `ridge-codegen-erl`, and is chosen
    // over a per-call clone to avoid repeated allocation.
    //
    // Written with raw strings and `writeln!` rather than escaped newlines:
    // the template reads as the file it produces, and a stray backslash
    // cannot quietly change what gets generated.
    const PREAMBLE: &str = r"
// @generated by crates/ridge-stdlib/build.rs
// Do not edit by hand — re-run cargo build to regenerate.
//
// Provides `lookup(module, name) -> Option<&'static StdlibTarget>`, read by
// every codegen backend as the single source of truth for how a Ridge stdlib
// symbol resolves: a host function, a compiled Ridge body, or a language
// primitive the backend supplies itself.

use std::collections::HashMap;
use std::sync::OnceLock;

type TargetMap = HashMap<String, StdlibTarget>;

static TARGET_MAP: OnceLock<TargetMap> = OnceLock::new();

#[allow(clippy::too_many_lines)]
fn build_target_map() -> TargetMap {
    let mut m = HashMap::new();
";

    const EPILOGUE: &str = r#"
    m
}

/// Look up how a Ridge stdlib symbol resolves.
///
/// Generated from the stdlib `.ridge` declarations at build time. A `None`
/// means the symbol is unknown — a `@primitive` declaration answers
/// `Some(StdlibTarget::Primitive { .. })`, so a backend can tell an operation
/// it has to supply from a name nobody declared.
#[must_use]
pub fn lookup(module: &str, name: &str) -> Option<&'static StdlibTarget> {
    let map: &TargetMap = TARGET_MAP.get_or_init(build_target_map);
    let key = format!("{module}::{name}");
    map.get(&key)
}
"#;

    let mut out = String::from(PREAMBLE);
    let _ = writeln!(out, "    m.reserve({});", entries.len());

    for e in entries {
        // Key: "ridge_module::ridge_fn" (double-colon matches BRIDGE_MAP).
        let key = format!("{}::{}", e.ridge_module, e.ridge_fn);
        let _ = write!(out, r#"    m.insert("{key}".to_owned(), "#);
        match &e.kind {
            EntryKind::Foreign { module, fn_name } => {
                let _ = writeln!(out, "StdlibTarget::Foreign {{");
                let _ = writeln!(out, r#"        module: "{module}".to_owned(),"#);
                let _ = writeln!(out, r#"        fn_name: "{fn_name}".to_owned(),"#);
            }
            EntryKind::RidgeModule => {
                // The module and function are the Ridge ones; a compiled
                // Ridge body is reached under the name it was written with.
                let _ = writeln!(out, "StdlibTarget::RidgeModule {{");
                let _ = writeln!(out, r#"        module: "{}".to_owned(),"#, e.ridge_module);
                let _ = writeln!(out, r#"        fn_name: "{}".to_owned(),"#, e.ridge_fn);
            }
            EntryKind::Primitive => {
                // No module, no function name: that is the whole point.
                let _ = writeln!(out, "StdlibTarget::Primitive {{");
            }
        }
        let _ = writeln!(out, "        arity: {},", e.arity);
        let _ = writeln!(out, "    }});");
    }

    out.push_str(EPILOGUE);
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
            // `codec.ridge` is the canonical, human-readable declaration of the
            // built-in Encode/Decode classes (registered in Rust, not compiled).
            // It must NOT be embedded: the driver compiles every unpacked source,
            // and codec.ridge's `instance Encode Int` would overlap the prelude
            // instance (T032). A consistency test reads it straight from disk.
            if path.file_name().is_some_and(|n| n == "codec.ridge") {
                continue;
            }
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
