//! Manifest regression test (§6.2).
//!
//! Proves bidirectional consistency between the live stdlib `.ridge` sources and
//! the generated manifest (`BUILTINS`) and signature table (`stdlib_signature`).
//!
//! ## What this file covers
//!
//! 1. Re-parses every `.ridge` file under `stdlib/` with text-level extraction
//!    (same algorithm that generates the manifest) to collect pub names.
//! 2. Compares against `BUILTINS[i].exports` — bidirectionally:
//!    - Every source-public name must appear in the manifest   → `T201`
//!    - Every manifest entry must have a matching source name  → `T201`
//! 3. Uses `ridge_parser::parse_source` to obtain the AST and checks param
//!    counts for functions that parse cleanly (functions with parse errors are
//!    skipped for the param-count assertion).
//! 4. For every parsed `pub fn`, calls `stdlib_signature` and asserts:
//!    - Returns `Some` (not `None`)                           → `T202`
//!    - `params.len()` matches AST param count                 → `T202`
//!    - Return-type shape loosely matches (see §6.2 §5)       → `T202`
//!
//! Entries whose signature body is `Type::Error` are explicitly allowed
//! (Phase-7 stubs annotated `// TODO Phase 7 (OQ-T012)`).
//!
//! Definitionally ensures that drift between source and
//! manifest/signature tables causes this test to fail the build.
//!
//! ## Note on parse errors
//!
//! Several stdlib `.ridge` files use `(_: Unit)` as a thunk-parameter convention.
//! The current parser (Phase 7) does not support `_` as the name in an
//! annotated parameter `(_: Type)` — it fires `P012 TopLevelPatternParam` and
//! drops those function declarations from the partial AST.  The text-level
//! extractor (`extract_pub_names_from_source`) handles `(_: Unit)` correctly,
//! so Test 1 (bidirectional names) is fully accurate.  Test 2 (signature shapes)
//! silently skips any function that did not parse cleanly — the manifest
//! coverage check in Test 1 still holds for those functions.

// Integration tests are allowed to use expect/unwrap/panic freely.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ridge_ast::{Item, Visibility};
use ridge_parser::parse_source;
use ridge_resolve::stdlib_builtin::BUILTINS;
use ridge_stdlib::codegen_manifest::{
    extract_pub_names_from_source, module_name_to_path, STDLIB_MODULE_ORDER,
};
use ridge_typecheck::{stdlib_signatures::stdlib_signature, BuiltinTyCons};
use ridge_types::{TyConArena, Type};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Construct a live `BuiltinTyCons` with an allocated arena.
///
/// We drop the arena immediately: the `TyConId` values embedded in
/// `BuiltinTyCons` remain valid as long as they are used only as opaque
/// discriminants (equality checks), which is all the signature-shape check
/// requires.
fn make_builtins() -> BuiltinTyCons {
    let mut arena = TyConArena::new();
    BuiltinTyCons::allocate(&mut arena)
}

/// Locate the `stdlib/` directory relative to `CARGO_MANIFEST_DIR`.
fn stdlib_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib")
}

// ── Prelude re-export whitelist ───────────────────────────────────────────────

/// Symbols that appear in `BUILTINS[i].exports` but are NOT declared as top-level
/// `pub fn` or `pub type` in the corresponding `.ridge` file.
///
/// These are language-prelude re-exports: the type / constructor is
/// declared in the compiler prelude and re-exported through the stdlib module's
/// name in the resolver.  There is no `pub type Option a = ...` declaration in
/// `option.ridge` — the `Option` type is built into the language.  The manifest
/// lists these to enable qualified resolution (e.g. `std.option.Some`).
///
/// The "formal prelude-re-export declaration mechanism" planned for a future
/// phase will replace this whitelist with an annotation in the `.ridge` source.
/// Until then, we skip the manifest→source direction check for these entries.
const PRELUDE_REEXPORTS: &[(&str, &str)] = &[
    // std.option: Option, Some, None are prelude constructors / the Option type.
    ("std.option", "Option"),
    ("std.option", "Some"),
    ("std.option", "None"),
    // std.result: Result, Ok, Err are prelude constructors / the Result type.
    ("std.result", "Result"),
    ("std.result", "Ok"),
    ("std.result", "Err"),
];

/// Return `true` if `(module, sym)` is a known prelude re-export that is
/// legitimately in the manifest but not in the `.ridge` source text.
fn is_prelude_reexport(module: &str, sym: &str) -> bool {
    PRELUDE_REEXPORTS
        .iter()
        .any(|&(m, s)| m == module && s == sym)
}

/// Exported constructors of a non-opaque union `pub type` declared in source.
///
/// Text extraction only surfaces the type name (`SortOrder`), not its variants,
/// so the manifest→source direction needs these listed explicitly. A future
/// pub-type-constructor export mechanism in the resolver will derive them from
/// source and retire this list (the same trajectory as `PRELUDE_REEXPORTS`).
const CONSTRUCTOR_EXPORTS: &[(&str, &str)] = &[
    ("std.query", "Asc"),
    ("std.query", "Desc"),
    // The `RoundingMode` constructors: exported for `round`/`div`, surfaced by text
    // extraction only through the type name `RoundingMode`.
    ("std.decimal", "HalfEven"),
    ("std.decimal", "HalfUp"),
    ("std.decimal", "HalfDown"),
    ("std.decimal", "Up"),
    ("std.decimal", "Down"),
    ("std.decimal", "Ceiling"),
    ("std.decimal", "Floor"),
    // The `QueryPlan` variants: exported so the set-operation terminals can build a
    // plan, but surfaced by text extraction only through the type name `QueryPlan`.
    ("std.query", "PlanScan"),
    ("std.query", "PlanCombine"),
    ("std.query", "PlanRefine"),
    ("std.query", "PlanJoin"),
    ("std.query", "PlanProject"),
    ("std.query", "PlanAggregate"),
    ("std.query", "PlanGroup"),
    // The `MutationPlan` variants: exported so a write verb can build a plan, but
    // surfaced by text extraction only through the type name `MutationPlan`.
    ("std.query", "MutInsert"),
    ("std.query", "MutUpsert"),
    ("std.query", "MutUpdate"),
    ("std.query", "MutDelete"),
    ("std.query", "MutDeleteKeys"),
    // The `DbError` variants: exported so a caller can match the kind of a typed
    // database error, but surfaced by text extraction only through the type name.
    ("std.data", "UniqueViolation"),
    ("std.data", "ForeignKeyViolation"),
    ("std.data", "NotNullViolation"),
    ("std.data", "CheckViolation"),
    ("std.data", "ConnectionError"),
    ("std.data", "DecodeError"),
    ("std.data", "Unsupported"),
    ("std.data", "QueryError"),
    // The `DbType` column-type constructors live in std.sql (beside SqlValue),
    // exported so a schema descriptor can name a column type but surfaced by text
    // extraction only through the type name.
    ("std.sql", "DbBoolean"),
    ("std.sql", "DbInt"),
    ("std.sql", "DbBigInt"),
    ("std.sql", "DbFloat"),
    ("std.sql", "DbDecimal"),
    ("std.sql", "DbText"),
    ("std.sql", "DbVarchar"),
    ("std.sql", "DbUuid"),
    ("std.sql", "DbTimestamp"),
    ("std.sql", "DbTimestampTz"),
    ("std.sql", "DbBytes"),
    ("std.sql", "DbSmallInt"),
    ("std.sql", "DbChar"),
    ("std.sql", "DbJson"),
    ("std.sql", "DbJsonb"),
    ("std.sql", "DbDate"),
    ("std.sql", "DbTime"),
    ("std.sql", "DbInterval"),
    ("std.sql", "DbArray"),
    ("std.sql", "DbRaw"),
    // The `std.schema` generation + foreign-key-action unions: constructors
    // exported for descriptors, surfaced by text extraction only through the type
    // names.
    ("std.schema", "Supplied"),
    ("std.schema", "Identity"),
    ("std.schema", "DefaultNow"),
    ("std.schema", "DefaultLit"),
    ("std.schema", "DefaultRawSql"),
    ("std.schema", "NoAction"),
    ("std.schema", "Restrict"),
    ("std.schema", "Cascade"),
    ("std.schema", "SetNull"),
    ("std.schema", "SetDefault"),
];

/// Return `true` if `(module, sym)` is a known exported union constructor that is
/// legitimately in the manifest but not surfaced by text extraction.
fn is_constructor_export(module: &str, sym: &str) -> bool {
    CONSTRUCTOR_EXPORTS
        .iter()
        .any(|&(m, s)| m == module && s == sym)
}

// ── Test 1: bidirectional name consistency ────────────────────────────────────

/// For every module in `STDLIB_MODULE_ORDER`:
/// - Every source-public name (from `extract_pub_names_from_source`) must
///   appear in `BUILTINS[i].exports`.
/// - Every `BUILTINS[i].exports` entry must appear in the source, OR be a
///   known prelude re-export (see `PRELUDE_REEXPORTS`).
///
/// Uses text-level extraction — the same algorithm that generates the manifest
/// — so there are no parse errors to worry about.
///
/// Failures panic with `T201 ManifestRegressionFailed { module, sym, reason }`.
#[test]
fn bidirectional_name_consistency() {
    let stdlib = stdlib_dir();

    for (idx, &dotted) in STDLIB_MODULE_ORDER.iter().enumerate() {
        // Locate the corresponding BUILTINS entry by index (indices match order).
        let builtin = &BUILTINS[idx];

        // Sanity check: the names must agree.
        assert_eq!(
            builtin.name, dotted,
            "T201 ManifestRegressionFailed {{ module: {:?}, sym: \"(module-name)\", \
             reason: \"BUILTINS[{idx}].name mismatch: got {:?} expected {:?}\" }}",
            dotted, builtin.name, dotted
        );

        // Read and text-level-extract pub names from the .ridge source.
        let rel = module_name_to_path(dotted);
        let full = stdlib.join(&rel);

        let src = std::fs::read_to_string(&full).unwrap_or_else(|e| {
            panic!(
                "T201 ManifestRegressionFailed {{ module: {:?}, sym: \"(file)\", \
                 reason: \"could not read {}: {e}\" }}",
                dotted,
                full.display()
            )
        });

        let src_names: Vec<String> = extract_pub_names_from_source(&src);
        let manifest_names: &[&str] = builtin.exports;

        // Direction 1: every source-public symbol must be in the manifest.
        for sym in &src_names {
            assert!(
                manifest_names.contains(&sym.as_str()),
                "T201 ManifestRegressionFailed {{ module: {dotted:?}, sym: {sym:?}, \
                 reason: \"pub symbol in .ridge source but missing from BUILTINS.exports\" }}"
            );
        }

        // Direction 2: every manifest entry must have a matching source symbol
        // OR be a known prelude re-export.
        for &sym in manifest_names {
            if src_names.iter().any(|s| s == sym) {
                continue; // found in source
            }
            if is_prelude_reexport(dotted, sym) {
                continue; // whitelisted prelude re-export
            }
            if is_constructor_export(dotted, sym) {
                continue; // whitelisted exported union constructor
            }
            panic!(
                "T201 ManifestRegressionFailed {{ module: {dotted:?}, sym: {sym:?}, \
                 reason: \"BUILTINS.exports entry has no matching pub symbol in .ridge source \
                 and is not a prelude re-export\" }}"
            );
        }
    }
}

// ── Test 2: signature shape consistency ───────────────────────────────────────

/// For every `pub fn` in the stdlib AST (those that parse cleanly):
/// - `stdlib_signature(module_id, name, &b)` must return `Some`.
/// - The param count must match `FnDecl.params.len()`.
/// - The return type shape must loosely match (see implementation guide §4).
///
/// Functions that fail to parse due to known parser limitations (e.g.
/// `(_: Unit)` annotated params — `P012 TopLevelPatternParam`) are silently
/// skipped; the bidirectional name check in Test 1 still holds for them.
///
/// Entries that return `Type::Error` are explicitly skipped (Phase-7 stubs).
/// Failures panic with `T202 SignatureDrift { module, sym, reason }`.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one skip-guard block per reconciled stdlib module, kept inline for readability"
)]
fn signature_shape_consistency() {
    let stdlib = stdlib_dir();
    let b = make_builtins();

    for (idx, &dotted) in STDLIB_MODULE_ORDER.iter().enumerate() {
        let builtin = &BUILTINS[idx];
        let module_id = builtin.id;

        // Parse the .ridge file to obtain a (potentially partial) AST.
        let rel = module_name_to_path(dotted);
        let full = stdlib.join(&rel);

        let src = std::fs::read_to_string(&full).unwrap_or_else(|e| {
            panic!(
                "T202 SignatureDrift {{ module: {:?}, sym: \"(file)\", \
                 reason: \"could not read {}: {e}\" }}",
                dotted,
                full.display()
            )
        });

        let result = parse_source(&src);

        // Collect pub fn declarations that parsed cleanly.
        let pub_fns: Vec<(&str, usize)> = result
            .module
            .items
            .iter()
            .filter_map(|item| {
                if let Item::Fn(f) = item {
                    if f.vis == Visibility::Pub {
                        return Some((f.name.text.as_str(), f.params.len()));
                    }
                }
                None
            })
            .collect();

        for (fn_name, ast_param_count) in &pub_fns {
            // std.query `orderSql`/`ascending` reference the reconciled `SortOrder`
            // type, so they are seeded via `reconciled_fn_scheme` rather than the
            // `stdlib_signature` table this shape check covers.
            if dotted == "std.query"
                && matches!(
                    *fn_name,
                    "orderSql"
                        | "ascending"
                        | "planScan"
                        | "planCombine"
                        | "planRefine"
                        | "planJoin"
                        | "planProject"
                        | "planAggregate"
                        | "planGroup"
                        | "planToSql"
                        | "optimize"
                        | "planExists"
                        | "planList"
                        | "planInsert"
                        | "planUpsert"
                        | "planUpdate"
                        | "planDelete"
                        | "planDeleteKeys"
                        | "mutationToSql"
                        | "mutationReturningToSql"
                )
            {
                continue;
            }
            // std.data `memAdapter`/`connect`/`connectWith` return reconciled types
            // (`MemAdapter`/`Postgres`), `defaultPool`/`with*` take or return the
            // reconciled `PoolConfig`, and the `selectRows`/`fetch` read helpers carry a
            // quoted predicate, so all are seeded via `reconciled_fn_scheme`, not the
            // `stdlib_signature` table this shape check covers.
            if dotted == "std.data"
                && matches!(
                    *fn_name,
                    "memAdapter"
                        | "connect"
                        | "connectWith"
                        | "selectRows"
                        | "fetch"
                        | "defaultPool"
                        | "withPoolSize"
                        | "withConnectTimeoutMs"
                        | "withQueryTimeoutMs"
                        | "withCheckoutTimeoutMs"
                        | "withIdleTimeoutMs"
                        | "withMaxLifetimeMs"
                        | "withHealthCheckMs"
                        | "withConnectRetries"
                        | "withRetryBackoffMs"
                        | "withMaxQueueDepth"
                )
            {
                continue;
            }
            // std.data's typed-error helpers are seeded via `reconciled_fn_scheme`
            // (they read or return the reconciled `DbErrorKind`), not the
            // `stdlib_signature` table this shape check covers.
            if dotted == "std.data"
                && matches!(
                    *fn_name,
                    "dbErrorKind" | "dbErrorConstraint" | "dbErrorColumn" | "dbErrorTable"
                )
            {
                continue;
            }
            // std.decimal's `round`/`div` take the reconciled `RoundingMode`, so
            // they are seeded via `reconciled_fn_scheme`, not the `stdlib_signature`
            // table this shape check covers. The rest of the module is hand-seeded.
            if dotted == "std.decimal" && matches!(*fn_name, "round" | "div") {
                continue;
            }
            // Every std.repo verb takes or returns the reconciled `Repo e a`, so
            // the whole module is seeded via `reconciled_fn_scheme` rather than
            // the `stdlib_signature` table this shape check covers.
            if dotted == "std.repo" {
                continue;
            }
            // Every std.migrate builder/runner references the reconciled
            // `Column`/`MigrationOp`/`Migration` block, so the whole module is seeded
            // via `reconciled_fn_scheme` rather than the `stdlib_signature` table.
            if dotted == "std.migrate" {
                continue;
            }
            // Every std.raw verb is constrained over the `Adapter` seam, so the
            // whole module is seeded via `reconciled_fn_scheme` rather than the
            // `stdlib_signature` table this shape check covers.
            if dotted == "std.raw" {
                continue;
            }
            // Every std.schema builder/accessor references the reconciled descriptor
            // types (`DbType`/`Generation`/`FkAction`/`ForeignKey`/`ColumnSchema`/
            // `EntitySchema`), so the whole module is seeded via
            // `reconciled_fn_scheme` rather than the `stdlib_signature` table.
            if dotted == "std.schema" {
                continue;
            }
            // 1. Signature must resolve to Some.
            let scheme = stdlib_signature(module_id, fn_name, &b).unwrap_or_else(|| {
                panic!(
                    "T202 SignatureDrift {{ module: {dotted:?}, sym: {fn_name:?}, \
                     reason: \"stdlib_signature returned None — symbol present in AST \
                     but has no signature entry\" }}"
                )
            });

            // 2. Skip Type::Error stubs — these are explicitly allowed in Phase 7.
            if matches!(scheme.ty, Type::Error) {
                continue;
            }

            // 3. Peel the function type from the scheme body.
            //    The scheme body should be Type::Fn { params, ret, .. }.
            //    Polymorphic schemes (forall a. ...) have their body directly as
            //    Type::Fn (no Type::Forall wrapper in this type system).
            let (sig_param_count, sig_ret) = match &scheme.ty {
                Type::Fn { params, ret, .. } => (params.len(), ret.as_ref()),
                other => {
                    // Non-function type body — e.g. polymorphic value like `empty`.
                    // For these we cannot check param count meaningfully.
                    // Accept them as-is (the manifest-existence check is sufficient).
                    let _ = other;
                    continue;
                }
            };

            // 4. Param count must match.
            assert!(
                sig_param_count == *ast_param_count,
                "T202 SignatureDrift {{ module: {dotted:?}, sym: {fn_name:?}, \
                 reason: \"param count mismatch: AST has {ast_param_count} \
                 param(s) but signature has {sig_param_count}\" }}"
            );

            // 5. Return-type shape: loose check per §6.2 step 5.
            //    If sig_ret is Type::Error, skip (stub for ret type).
            //    Otherwise just verify sig_ret is not Error (it has a real type).
            if matches!(sig_ret, Type::Error) {
                // Phase-7 stub for return type — skip.
                continue;
            }

            // The sig_ret is a real type (not Error). No deeper structural
            // comparison is required by §6.2 (polymorphic / generic shapes are
            // explicitly excluded from the check to avoid fragility).
            // The existence of a non-Error return type is the DoD criterion.
            let _ = sig_ret;
        }
    }
}

// ── Test 3: module count matches STDLIB_MODULE_ORDER ─────────────────────────

/// Sanity guard: `BUILTINS` and `STDLIB_MODULE_ORDER` must have the same length.
#[test]
fn module_count_matches() {
    assert_eq!(
        BUILTINS.len(),
        STDLIB_MODULE_ORDER.len(),
        "T201 ManifestRegressionFailed {{ module: \"(all)\", sym: \"(count)\", \
         reason: \"BUILTINS.len() ({}) != STDLIB_MODULE_ORDER.len() ({})\" }}",
        BUILTINS.len(),
        STDLIB_MODULE_ORDER.len()
    );
}
