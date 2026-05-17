//! `with`-update expression lowering — §4.5.
//!
//! # Rule summary
//!
//! `Expr::With { base, fields, span }` lowers to:
//!
//! ```text
//! IrExpr::LetIn {
//!     pat:   IrPat::Bind { name: "__with_base_N" },
//!     value: lower_expr(base),
//!     body:  IrExpr::Construct {
//!         ctor:   SymbolRef::Constructor { kind: Record,
//!                                         owner_type: rec_tycon,
//!                                         name: rec_name,
//!                                         variant: 0 },
//!         fields: <merged> — schema fields in declaration order,
//!     },
//! }
//! ```
//!
//! The merged field list iterates over the **record schema's fields in
//! declaration order** (not the source order of the `with` clause).  For each
//! field:
//! - If the field is in the update set and has an explicit value: `lower_expr(v)`.
//! - If the field is in the update set and is shorthand (D053):
//!   `IrExpr::Local { name: fd.name }` (pulls from local environment).
//! - Otherwise: `IrExpr::Field { base: Local("__with_base_N"), field: fd.name }`.
//!
//! # Workspace dependency
//!
//! Schema lookup requires `TypedWorkspace.tycons` (via `ctx.workspace`).  The
//! base's type (`Type::Con(rec_tycon, _)`) is read from `ctx.node_types` by
//! using `span.start` as a proxy `NodeId` key (matching the T17-deferred
//! convention used in `ridge-typecheck::lib.rs`).
//!
//! When workspace or type information is unavailable (T17 deferred), `L008
//! WithOnNonRecord` is emitted defensively and a `Unit` literal is returned.
//!
//! # Edge cases
//!
//! - **Chained `with`:** `u with { a=1 } with { b=2 }` is left-associative;
//!   the outer `lower_with` call processes the inner-result as `base`, which
//!   `lower_expr` has already lowered.  Works automatically via recursion.
//! - **Shorthand field (`u with { name }`):** the shorthand pulls `name` from
//!   the local environment via `IrExpr::Local { name }`, NOT from the base.
//! - **`Type::Alias` on base:** the alias is transparent; we match through
//!   `Type::Alias { name: id, .. }` to reach the underlying `TyConId`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use std::collections::HashSet;

use ridge_ast::{expr::FieldInit, Expr, Span};
use ridge_ir::{symbol::CtorKind, IrExpr, IrLit, IrPat, SymbolRef};
use ridge_resolve::NodeKind;
use ridge_types::{tycon::RecordField, TyConDecl, TyConId, TyConKind, Type};

use crate::core::lower_expr;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower `base with { fields }` to
/// `LetIn { Bind("__with_base_N"), lower(base), Construct(...) }`.
///
/// Looks up the record schema from `ctx.workspace.tycons` using the type of
/// `base` (which Phase 4 guarantees is `Type::Con(record_tycon, _)` after T8's
/// `T006` check).  Emits `L008 WithOnNonRecord` defensively when the schema
/// cannot be resolved and returns `IrLit::Unit`.
pub fn lower_with(ctx: &mut LowerCtx<'_>, base: &Expr, fields: &[FieldInit], span: Span) -> IrExpr {
    // ── 1. Look up the type of `base` ─────────────────────────────────────────
    let base_ty = lookup_base_type(ctx, base);

    // ── 2. Resolve the record TyConId ─────────────────────────────────────────
    let Some(rec_tycon) = resolve_record_tycon(ctx, base_ty, span) else {
        // L008 already emitted by resolve_record_tycon.
        let id = ctx.fresh_id(None);
        return IrExpr::Lit {
            id,
            value: IrLit::Unit,
            span,
        };
    };

    // ── 3. Look up the RecordSchema ───────────────────────────────────────────
    // Borrow the tycons slice from the workspace.
    let Some(ws) = ctx.workspace else {
        ctx.errors.push(LowerError::WithOnNonRecord { span });
        let id = ctx.fresh_id(None);
        return IrExpr::Lit {
            id,
            value: IrLit::Unit,
            span,
        };
    };
    let tycons: Vec<TyConDecl> = ws.tycons.clone();

    let Some((rec_name, record_fields)) = lookup_record_schema_from_slice(&tycons, rec_tycon)
    else {
        ctx.errors.push(LowerError::WithOnNonRecord { span });
        let id = ctx.fresh_id(None);
        return IrExpr::Lit {
            id,
            value: IrLit::Unit,
            span,
        };
    };

    // ── 4. Build the `__with_base_N` synthetic local name ────────────────────
    let base_local = ctx.fresh_local("__with_base");

    // ── 5. Collect the set of touched field names ─────────────────────────────
    let touched: HashSet<&str> = fields.iter().map(|f| f.name.text.as_str()).collect();

    // ── 6. Build the merged field list in schema declaration order ────────────
    let merged: Vec<(String, IrExpr)> = record_fields
        .iter()
        .map(|fd| {
            let field_name = fd.name.clone();
            if touched.contains(field_name.as_str()) {
                // This field is touched by the `with` update.
                let init = fields.iter().find(|f| f.name.text == field_name);
                let value_ir = match init {
                    Some(FieldInit { value: Some(v), .. }) => lower_expr(ctx, v),
                    Some(FieldInit {
                        value: None,
                        name: fname,
                        span: fspan,
                    }) => {
                        // Shorthand (D053) — pull from local environment, not from base.
                        let id = ctx.fresh_id(None);
                        IrExpr::Local {
                            id,
                            name: fname.text.clone(),
                            span: *fspan,
                        }
                    }
                    None => {
                        // Defensive: should never happen (touched set built from `fields`).
                        // Fall back to pulling from base.
                        make_field_projection(ctx, &base_local, &field_name, span)
                    }
                };
                (field_name, value_ir)
            } else {
                // Un-touched field — pull from the bound base local.
                let proj = make_field_projection(ctx, &base_local, &field_name, span);
                (field_name, proj)
            }
        })
        .collect();

    // ── 7. Build IrExpr::Construct ────────────────────────────────────────────
    let construct_id = ctx.fresh_id(None);
    let construct = IrExpr::Construct {
        id: construct_id,
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: rec_tycon,
            name: rec_name,
            variant: 0,
        },
        fields: merged,
        span,
    };

    // ── 8. Bind base to a local via LetIn ─────────────────────────────────────
    let let_id = ctx.fresh_id(None);
    IrExpr::LetIn {
        id: let_id,
        pat: IrPat::Bind {
            name: base_local,
            inner: None,
            span,
        },
        value: Box::new(lower_expr(ctx, base)),
        body: Box::new(construct),
        span,
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Look up the type of `base` from `ctx.node_types` via the `NodeIdMap`.
///
/// Uses `(base.span(), NodeKind::Expr)` to find the proper compact `NodeId`
/// assigned during the resolve phase, then indexes `node_types` with that id.
/// This is the authoritative lookup path — the old proxy `NodeId(span.start)`
/// incorrectly used the byte-offset as a `NodeId`, which is valid only when the
/// byte offset coincidentally equals the compact node counter (never guaranteed).
///
/// Falls back to `None` (which causes `L008` to be emitted defensively) when:
/// - `ctx.node_id_map` is absent (unit-test scaffolding), or
/// - the expression span has no `NodeKind::Expr` entry (synthetic expressions).
///
/// Clone the type so the immutable borrow on `ctx` is released before the
/// caller calls `resolve_record_tycon` (which takes a mutable borrow).
fn lookup_base_type(ctx: &LowerCtx<'_>, base: &Expr) -> Option<Type> {
    // Primary path: use the NodeIdMap to find the compact NodeId for this
    // expression, then look up the type in node_types[NodeId.0].
    ctx.node_id_map
        .as_ref()
        .and_then(|m| m.get(base.span(), NodeKind::Expr))
        .and_then(|nid| ctx.node_type(nid).cloned())
}

/// Resolve the `TyConId` of the record type from the base's `Type`.
///
/// Handles `Type::Con(c, _)` directly and `Type::Alias { name: c, .. }` by
/// looking through one level of aliasing.  Emits `L008` and returns `None`
/// when the type is absent or not a supported shape.
///
/// `base_ty` is cloned from `ctx.node_types` before this call so the
/// immutable borrow on `ctx` is released by the time this function takes its
/// mutable borrow.
#[allow(clippy::needless_pass_by_value)]
fn resolve_record_tycon(
    ctx: &mut LowerCtx<'_>,
    base_ty: Option<Type>,
    span: Span,
) -> Option<TyConId> {
    match base_ty {
        Some(Type::Con(id, _)) => Some(id),
        // Alias — one level of transparent resolution (OQ-T015).
        Some(Type::Alias { name, .. }) => Some(name),
        // Anything else: emit L008 defensively.
        None | Some(Type::Error | _) => {
            ctx.errors.push(LowerError::WithOnNonRecord { span });
            None
        }
    }
}

/// Look up `(record_name, field_vec)` from a `TyConDecl` slice.
///
/// Returns `None` when `rec_tycon` is out of range or the entry is not a
/// `TyConKind::Record`.  The caller is responsible for emitting `L008`.
fn lookup_record_schema_from_slice(
    tycons: &[TyConDecl],
    rec_tycon: TyConId,
) -> Option<(String, Vec<RecordField>)> {
    let decl = tycons.get(rec_tycon.0 as usize)?;
    if let TyConKind::Record(schema) = &decl.kind {
        let name = decl.name.clone();
        let fields = schema.record_fields().to_vec();
        Some((name, fields))
    } else {
        None
    }
}

/// Build `IrExpr::Field { base: Local(base_local), field: field_name }`.
fn make_field_projection(
    ctx: &mut LowerCtx<'_>,
    base_local: &str,
    field_name: &str,
    span: Span,
) -> IrExpr {
    let field_id = ctx.fresh_id(None);
    let base_id = ctx.fresh_id(None);
    IrExpr::Field {
        id: field_id,
        base: Box::new(IrExpr::Local {
            id: base_id,
            name: base_local.to_owned(),
            span,
        }),
        field: field_name.to_owned(),
        span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{expr::FieldInit, Ident, Literal, Span};
    use ridge_ir::{IrExpr, IrLit};
    use ridge_resolve::ModuleId;
    use ridge_types::{
        tycon::{RecordField, RecordSchema, TyConArena, TyConDecl, TyConId, TyConKind},
        BuiltinTyCons, Type,
    };

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    /// Build a `Vec<TyConDecl>` (builtins + one record at index 15).
    ///
    /// Returns the slice and the `TyConId` of the record (`TyConId(15)`).
    /// Index 15 = after the 15 `BuiltinTyCons` entries (12 original + 3 stdlib
    /// record types: Error=12, Duration=13, ProcOutput=14).
    fn make_tycons_with_record(fields: Vec<(&str, Type)>) -> (Vec<TyConDecl>, TyConId) {
        let mut arena = TyConArena::new();
        let _ = BuiltinTyCons::allocate(&mut arena);

        let record_schema = RecordSchema::new(
            vec![],
            fields
                .into_iter()
                .map(|(n, t)| RecordField {
                    name: n.to_string(),
                    ty: t,
                })
                .collect(),
        );
        let rec_id = arena.intern(TyConDecl {
            id: TyConId(0), // overwritten by intern
            name: "TestRecord".into(),
            arity: 0,
            kind: TyConKind::Record(record_schema),
            def_span: None,
        });
        assert_eq!(
            rec_id,
            TyConId(15),
            "record must be at index 15 (after 15 builtins)"
        );

        (arena.all().to_vec(), rec_id)
    }

    /// Build a `LowerCtx` with:
    /// - `node_types[base_span.start]` set to `Type::Con(rec_tycon, [])`, and
    /// - a minimal fake `TypedWorkspace` whose `tycons` slice contains the
    ///   record's `TyConDecl`.
    ///
    /// Because `TypedWorkspace` is `#[non_exhaustive]`, we cannot construct it
    /// directly in tests outside `ridge-typecheck`.  Instead we attach a real
    /// workspace built via `ridge_typecheck::typecheck_workspace` on a trivial
    /// empty source — this gives us the 12 built-in `TyConDecl`s in `tycons`.
    /// We then extend `ctx.node_types` and manually test `lookup_record_schema_from_slice`
    /// to cover the schema-lookup code paths.
    ///
    /// For `lower_with` integration tests that actually traverse the schema,
    /// we use a ctx whose workspace is `None` (testing the L008 fallback) OR
    /// we call `lower_with_using_tycons` (a test-only helper below).
    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn int_lit_expr(n: i64, span: Span) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.to_string(),
            span,
        })
    }

    fn make_field_init(name: &str, value: Option<Expr>) -> FieldInit {
        FieldInit {
            name: Ident {
                text: name.into(),
                span: sp(),
            },
            value,
            span: sp(),
        }
    }

    // ── T9-w-1: lookup_record_schema_from_slice — happy path ─────────────────
    //
    // Verifies that the schema lookup returns the correct (name, fields) pair.
    #[test]
    fn schema_lookup_happy_path() {
        let (tycons, rec_id) = make_tycons_with_record(vec![
            ("x", Type::Con(TyConId(0), vec![])),
            ("y", Type::Con(TyConId(0), vec![])),
        ]);

        let result = lookup_record_schema_from_slice(&tycons, rec_id);
        assert!(result.is_some(), "expected Some for valid record TyConId");
        let (name, fields) = result.unwrap();
        assert_eq!(name, "TestRecord");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "x");
        assert_eq!(fields[1].name, "y");
    }

    // ── T9-w-2: lookup_record_schema_from_slice — out of range ───────────────
    #[test]
    fn schema_lookup_out_of_range() {
        let (tycons, _) = make_tycons_with_record(vec![]);
        // TyConId(999) is out of range.
        let result = lookup_record_schema_from_slice(&tycons, TyConId(999));
        assert!(result.is_none(), "expected None for out-of-range TyConId");
    }

    // ── T9-w-3: lookup_record_schema_from_slice — non-record (Primitive) ─────
    #[test]
    fn schema_lookup_non_record_returns_none() {
        let (tycons, _) = make_tycons_with_record(vec![]);
        // TyConId(0) is Int (Primitive, not Record).
        let result = lookup_record_schema_from_slice(&tycons, TyConId(0));
        assert!(result.is_none(), "Int is not a record — expected None");
    }

    // ── T9-w-4: no-workspace emits L008 ──────────────────────────────────────
    //
    // When `ctx.workspace` is `None`, `lower_with` emits `L008` and returns
    // a `Unit` stub.
    #[test]
    fn no_workspace_emits_l008() {
        let mut ctx = fresh_ctx();
        let base = int_lit_expr(0, sp());
        let fields = vec![make_field_init("x", Some(int_lit_expr(1, sp())))];
        let ir = lower_with(&mut ctx, &base, &fields, sp());

        let l008_count = ctx.errors.iter().filter(|e| e.code() == "L008").count();
        assert_eq!(
            l008_count, 1,
            "expected 1 L008 error; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Lit {
                value: IrLit::Unit, ..
            } => {}
            other => panic!("expected Unit stub, got {other:?}"),
        }
    }

    // ── T9-w-5: make_field_projection builds Field over Local ────────────────
    //
    // Unit-tests the helper directly.
    #[test]
    fn field_projection_helper_builds_field_over_local() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 10);
        let proj = make_field_projection(&mut ctx, "__with_base_0", "age", span);

        match proj {
            IrExpr::Field {
                field,
                base,
                span: s,
                ..
            } => {
                assert_eq!(field, "age");
                assert_eq!(s, span);
                match *base {
                    IrExpr::Local { ref name, .. } => {
                        assert_eq!(name, "__with_base_0");
                    }
                    other => panic!("base must be Local, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Field, got {other:?}"),
        }
    }

    // ── T9-w-6: schema field ordering test ───────────────────────────────────
    //
    // Verifies that `lookup_record_schema_from_slice` returns fields in
    // declaration order, not alphabetical order.
    #[test]
    fn schema_fields_in_declaration_order() {
        // Declare in order: z, a, m — reversed alphabetically.
        let (tycons, rec_id) = make_tycons_with_record(vec![
            ("z", Type::Con(TyConId(0), vec![])),
            ("a", Type::Con(TyConId(0), vec![])),
            ("m", Type::Con(TyConId(0), vec![])),
        ]);
        let (_, fields) = lookup_record_schema_from_slice(&tycons, rec_id).unwrap();
        assert_eq!(
            fields[0].name, "z",
            "first field must be z (declaration order)"
        );
        assert_eq!(fields[1].name, "a", "second field must be a");
        assert_eq!(fields[2].name, "m", "third field must be m");
    }

    // ── T9-w-7: resolve_record_tycon handles Type::Con ───────────────────────
    #[test]
    fn resolve_record_tycon_con() {
        let mut ctx = fresh_ctx();
        // Use an arbitrary valid TyConId value (15 = first user-defined slot after
        // the 15 BuiltinTyCons entries).
        let ty = Some(Type::Con(TyConId(15), vec![]));
        let result = resolve_record_tycon(&mut ctx, ty, sp());
        assert_eq!(result, Some(TyConId(15)));
        assert!(ctx.errors.is_empty(), "no errors expected for Type::Con");
    }

    // ── T9-w-8: resolve_record_tycon emits L008 for None ─────────────────────
    #[test]
    fn resolve_record_tycon_none_emits_l008() {
        let mut ctx = fresh_ctx();
        let result = resolve_record_tycon(&mut ctx, None, sp());
        assert!(result.is_none());
        assert_eq!(ctx.errors.len(), 1);
        assert_eq!(ctx.errors[0].code(), "L008");
    }
}
