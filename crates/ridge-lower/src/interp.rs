//! String-interpolation lowering — §4.6.
//!
//! # Rule summary
//!
//! `Expr::Interp { parts, span }` lowers to a left-fold of
//! `std.text.concat` calls over the lowered parts:
//!
//! ```text
//! lower_interp_full([p0, p1, p2, ...]) =
//!     ((lower(p0) ++ lower(p1)) ++ lower(p2)) ++ ...
//! ```
//!
//! where each part is either:
//! - `InterpPart::Text { raw }` → `IrLit::Text(raw)`, or
//! - `InterpPart::Expr { expr }` → `lower_expr(expr)` optionally wrapped in
//!   `Call(SymbolRef::Stdlib { module: "std.<x>", name: "toText" }, [inner])`
//!   for the closed `ToText` set.
//!
//! # `ToText` dispatch
//!
//! The inner expression's type is looked up from `ctx.node_types` by `NodeId`.
//! Because `node_types` is populated by Phase 4 which is deferred,
//! type information may be absent.  When absent, `L007 ToTextLowering` is
//! emitted defensively and the raw `inner` is returned unwrapped.
//!
//! The closed `ToText` set maps `TyConId` to stdlib module:
//!
//! | `TyConId` | Built-in type | Stdlib module         |
//! |---------|---------------|----------------------|
//! | 0       | `Int`         | `std.int.toText`     |
//! | 1       | `Float`       | `std.float.toText`   |
//! | 2       | `Bool`        | `std.bool.toText`    |
//! | 3       | `Text`        | identity (no wrap)   |
//! | 5       | `Timestamp`   | `std.time.toText`    |
//!
//! `Type::Error` is absorbing — no wrapper, no diagnostic.
//! Any other type → `L007`, inner returned unwrapped.
//!
//! # Spec §7.1 left-to-right evaluation
//!
//! The fold is explicitly **left-to-right** (`fold` over `[p1..]` with `p0` as
//! the accumulator, wrapping `(acc, next) → concat(acc, next)`).  This ensures
//! side-effecting hole expressions evaluate in source order per spec §7.1.
//!
//! # Edge cases
//!
//! - **Empty interp `$""`:** emits a single `Text("")` literal.  Handled by
//!   the single-`Text`-part fast path in `core::lower_interp`.
//! - **Single-part text `$"hello"`:** fast path in `core::lower_interp`.
//! - **Adjacent expr holes `$"${a}${b}"`:** parts are
//!   `[Text "", Expr a, Text "", Expr b, Text ""]`; the fold naturally
//!   produces `((("" ++ a) ++ "") ++ b) ++ ""`.  Empty strings are NOT
//!   elided — backends or Phase 7 may optimise.
//! - **`Type::Error` hole:** absorbing — inner returned as-is, no L007.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{expr::InterpPart, Expr, Span};
use ridge_ir::{IrExpr, IrLit, SymbolRef};
use ridge_resolve::NodeKind;
use ridge_types::{TyConId, Type, TOTEXT_CLASS};

use crate::core::lower_expr;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// ── TyCon id constants — must match BuiltinTyCons::allocate order ─────────────

/// `Int` — `TyConId(0)`.
const INT_TYCON: TyConId = TyConId(0);
/// `Float` — `TyConId(1)`.
const FLOAT_TYCON: TyConId = TyConId(1);
/// `Bool` — `TyConId(2)`.
const BOOL_TYCON: TyConId = TyConId(2);
/// `Text` — `TyConId(3)`.
const TEXT_TYCON: TyConId = TyConId(3);
/// `Timestamp` — `TyConId(5)`.
const TIMESTAMP_TYCON: TyConId = TyConId(5);

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower a multi-part or hole-containing `Interp` expression to a left-fold
/// of `std.text.concat` calls.
///
/// Called from `crate::core` for any interpolation that is not
/// the single-text-part fast path.  Each [`InterpPart::Text`] lowers to a
/// plain `IrLit::Text`; each [`InterpPart::Expr`] lowers to `lower_expr` then
/// optionally wrapped in a `toText` call for the closed `ToText` set.
///
/// The fold is strictly left-to-right (spec §7.1): `((p0 ++ p1) ++ p2) ++ …`.
///
/// # Empty parts
///
/// If `parts` is empty (which the parser should not produce for `$""`), returns
/// `IrLit::Text("")` as a safe default.
pub fn lower_interp_full(ctx: &mut LowerCtx<'_>, parts: &[InterpPart], span: Span) -> IrExpr {
    if parts.is_empty() {
        // Defensive: empty parts produce an empty string literal.
        let id = ctx.fresh_id(None);
        return IrExpr::Lit {
            id,
            value: IrLit::Text(String::new()),
            span,
        };
    }

    // Lower every part to an IrExpr, applying ToText wrapping where needed.
    let pieces: Vec<IrExpr> = parts
        .iter()
        .map(|part| lower_part(ctx, part, span))
        .collect();

    // Left-fold: ((p0 ++ p1) ++ p2) ++ ...
    // Split first so that the accumulator starts as p0 without a concat.
    let mut iter = pieces.into_iter();
    // SAFETY: `parts` is non-empty so `pieces` has ≥ 1 element.
    let first = iter.next().unwrap_or_else(|| {
        let id = ctx.fresh_id(None);
        IrExpr::Lit {
            id,
            value: IrLit::Text(String::new()),
            span,
        }
    });

    // OQ-IR002: binary std.text.concat fold-left — each pair is concat(acc, next).
    iter.fold(first, |acc, next| make_concat_call(ctx, acc, next, span))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Lower a single [`InterpPart`] to an [`IrExpr`], applying `toText` wrapping
/// for `InterpPart::Expr` holes (applying `toText` for the closed set of convertible types).
fn lower_part(ctx: &mut LowerCtx<'_>, part: &InterpPart, _span: Span) -> IrExpr {
    match part {
        InterpPart::Text {
            raw,
            span: text_span,
        } => {
            // Decode validated escape sequences (\n, \t, \", \\, \r, \0, \u{HHHH}, \$)
            // inside interpolated text segments.  Without this the runtime saw the
            // raw source bytes with literal backslashes.
            let id = ctx.fresh_id(None);
            IrExpr::Lit {
                id,
                value: IrLit::Text(crate::core::decode_text_escapes(raw)),
                span: *text_span,
            }
        }

        InterpPart::Expr {
            expr,
            span: expr_span,
        } => {
            let inner = lower_expr(ctx, expr);
            // Look up the type of this expression from the node_types side-table.
            // The lookup is by NodeId, which is stored in the node_types Vec
            // indexed by NodeId.0.  When node_types is empty (T17 deferred),
            // the lookup returns None and we emit L007 defensively.
            // Clone the type before the mutable borrow by `wrap_to_text`.
            let ty = lookup_expr_type(ctx, expr);
            wrap_to_text(ctx, inner, ty, *expr_span)
        }
    }
}

/// Attempt to look up the type of an expression from the `node_types` side-table.
///
/// Uses `node_id_map` to resolve the expression's `Span` to a compact sequential
/// `NodeId` (the correct index into `ctx.node_types`).  Falls back gracefully to
/// `None` when the map is absent (T17 deferred) or the span is not found.
///
/// **Do not** use `NodeId(span.start)` as a proxy — `node_types` is indexed by
/// compact sequential `NodeIds` from AST traversal, not by byte offsets.
fn lookup_expr_type(ctx: &LowerCtx<'_>, expr: &Expr) -> Option<Type> {
    ctx.node_id_map
        .as_ref()
        .and_then(|m| m.get(expr.span(), NodeKind::Expr))
        .and_then(|nid| ctx.node_type(nid).cloned())
}

/// Wrap `inner` in a `Call(stdlib::toText, [inner])` for the appropriate
/// built-in type, or return `inner` unchanged for `Text` / `Error` / unknown.
///
/// Emits `L007 ToTextLowering` when the type is non-`Error` and not in the
/// closed `ToText` set.
///
/// `ty` is cloned from `ctx.node_types` before this call (required to release
/// the immutable borrow on `ctx` so this function can mutably borrow it for
/// error reporting).
#[allow(clippy::needless_pass_by_value)]
fn wrap_to_text(ctx: &mut LowerCtx<'_>, inner: IrExpr, ty: Option<Type>, span: Span) -> IrExpr {
    match ty {
        // ── Type::Text — identity; no wrapper ────────────────────────────────
        Some(Type::Con(id, _)) if id == TEXT_TYCON => inner,

        // ── Type::Int — std.int.toText ────────────────────────────────────────
        Some(Type::Con(id, _)) if id == INT_TYCON => make_to_text_call(ctx, inner, "std.int", span),

        // ── Type::Float — std.float.toText ────────────────────────────────────
        Some(Type::Con(id, _)) if id == FLOAT_TYCON => {
            make_to_text_call(ctx, inner, "std.float", span)
        }

        // ── Type::Bool — std.bool.toText ─────────────────────────────────────
        Some(Type::Con(id, _)) if id == BOOL_TYCON => {
            make_to_text_call(ctx, inner, "std.bool", span)
        }

        // ── Type::Timestamp — std.time.toText ────────────────────────────────
        Some(Type::Con(id, _)) if id == TIMESTAMP_TYCON => {
            make_to_text_call(ctx, inner, "std.time", span)
        }

        // ── Type::Error — absorbing; pass through without wrapping ────────────
        Some(Type::Error) => inner,

        // ── Type::Var — polymorphic hole in a constrained fn ─────────────────
        // When the hole type is a free variable AND the enclosing fn is
        // constrained for `ToText a`, dispatch through the in-scope dict param:
        // `apply maps:get('toText', $dict_ToText_{a}) (inner)`.
        //
        // This is the dictionary-passing path for string-interpolation holes
        // whose type is polymorphic (e.g. `$"it is ${x}"` in `fn describe
        // (x: a) -> Text where ToText a`). For monomorphic holes the
        // `Type::Con` arms above handle dispatch.
        Some(Type::Var(_tyvid)) => {
            if let Some(dict_call) = try_dict_to_text(ctx, &inner, span) {
                dict_call
            } else {
                ctx.errors.push(LowerError::ToTextLowering { span });
                inner
            }
        }

        // ── User-defined Type::Con — ToText instance registry lookup ─────────
        // When the inner type is a user-defined TyCon, dispatch through the
        // workspace instance registry. The registry is populated during the
        // collect pass for explicit `instance ToText T` declarations and for
        // auto-promoted `pub fn toText (x: T) -> Text` declarations. This
        // O(1) lookup replaces the old AST scan and fixes the three limitations
        // of that approach: it works for types without a `def_module_raw` (e.g.
        // stdlib/builtin types), in unit-test contexts where the workspace is
        // absent (falls through to L007 gracefully), and at O(1) cost per site.
        Some(Type::Con(tycon_id, _)) => {
            if let Some(call) = try_instance_to_text(ctx, &inner, tycon_id, span) {
                call
            } else {
                ctx.errors.push(LowerError::ToTextLowering { span });
                inner
            }
        }

        // ── Type not available or not in closed set ───────────────────────────
        // When None: node_types is empty; emit L007 defensively and pass
        // inner through.  The type-checker guarantees this cannot fire on valid input.
        None | Some(_) => {
            ctx.errors.push(LowerError::ToTextLowering { span });
            inner
        }
    }
}

/// Try to dispatch `toText` for `tycon_id` through the workspace instance
/// registry.
///
/// Returns `Some(Call)` when the instance registry contains a `ToText` entry
/// for `tycon_id` and the owning module's `toText` function can be referenced.
/// Returns `None` when the registry is unavailable (unit tests without the
/// full pipeline) or when no `ToText` instance is registered for the type.
/// The caller is responsible for emitting `L007` on the `None` branch.
///
/// This is an O(1) lookup — it replaces the previous approach of scanning
/// the owning module's AST for a `pub fn toText` declaration on every
/// interpolation site.
fn try_instance_to_text(
    ctx: &mut LowerCtx<'_>,
    inner: &IrExpr,
    tycon_id: TyConId,
    span: Span,
) -> Option<IrExpr> {
    // The instance registry is available when the full pipeline is wired.
    let env = ctx.instance_env?;

    // O(1) instance lookup by (ToText, TyConId).
    let inst = env.get((TOTEXT_CLASS, tycon_id))?;

    // Determine the owning module: the instance's def_module carries the
    // module that originally declared the `pub fn toText` (or the explicit
    // `instance ToText T` declaration). For prelude instances `def_module`
    // is `None`; those are handled by the closed-set arms above.
    let module_raw = inst.def_module?;
    let owning_module = ridge_resolve::ModuleId(module_raw);

    let callee_id = ctx.fresh_id(None);
    let call_id = ctx.fresh_id(None);
    let callee = Box::new(IrExpr::Symbol {
        id: callee_id,
        sym: SymbolRef::External {
            module: owning_module,
            name: "toText".into(),
        },
        span,
    });
    Some(IrExpr::Call {
        id: call_id,
        callee,
        args: vec![inner.clone()],
        span,
    })
}

/// Try to dispatch `toText` through the in-scope dictionary parameter when the
/// hole expression has a polymorphic type (`Type::Var`).
///
/// Used inside constrained functions where the type variable `a` is bound by
/// a `where ToText a` constraint and the caller's own dict param
/// `$dict_ToText_{idx}` holds the dictionary.
///
/// Returns `Some(Call(Field($dict, 'toText'), [inner]))` when the enclosing
/// function has a `ToText` constraint; `None` otherwise.
fn try_dict_to_text(ctx: &mut LowerCtx<'_>, inner: &IrExpr, span: Span) -> Option<IrExpr> {
    // Find a ToText constraint on the current fn.
    let totext_c = ctx
        .current_fn_constraints
        .iter()
        .find(|c| c.class == TOTEXT_CLASS)
        .cloned()?;

    let class_name = ctx.class_name(TOTEXT_CLASS).unwrap_or("ToText").to_owned();
    let dict_param_name = format!("$dict_{class_name}_{}", totext_c.sole_ty().0);

    // Dict expression: the in-scope dict param (a local variable).
    let dict_id = ctx.fresh_id(None);
    let dict_expr = IrExpr::Local {
        id: dict_id,
        name: dict_param_name,
        span,
    };

    // Method projection: `IrExpr::Field { base: dict, field: "toText" }`.
    // Codegen lowers this to `maps:get('toText', Dict)` (or folds it if the
    // dict is a literal MapLit via the static peephole in lower_field).
    let field_id = ctx.fresh_id(None);
    let method_ref = IrExpr::Field {
        id: field_id,
        base: Box::new(dict_expr),
        field: "toText".into(),
        span,
    };

    // Wrap in a call: `apply (maps:get('toText', Dict)) (inner)`.
    let call_id = ctx.fresh_id(None);
    Some(IrExpr::Call {
        id: call_id,
        callee: Box::new(method_ref),
        args: vec![inner.clone()],
        span,
    })
}

// OQ-L007: ToText is inserted at lowering time (Phase 5), not at codegen time,
// so that the IR is target-neutral (no implicit coercion knowledge needed downstream).

/// Wrap `arg` in the correct `toText` call for `tycon_id`.
///
/// Used by the derived-instance lowering pass to dispatch `toText` on each
/// record field or union payload. The mapping follows the same closed set as
/// [`wrap_to_text`]:
///
/// | `TyConId` | Dispatch target          |
/// |-----------|--------------------------|
/// | 0 (Int)   | `std.int.toText`         |
/// | 1 (Float) | `std.float.toText`       |
/// | 2 (Bool)  | `std.bool.toText`        |
/// | 3 (Text)  | identity — returned as-is |
/// | 5 (Timestamp) | `std.time.toText`    |
/// | other     | identity — no known stdlib dispatch; field rendered as-is |
///
/// For user-defined types the derived instance lowering emits an identity
/// (no wrapper) because those types render via their own derived or explicit
/// `ToText` instance, resolved separately through the dict machinery.
pub(crate) fn wrap_to_text_by_tycon(
    ctx: &mut LowerCtx<'_>,
    arg: IrExpr,
    tycon_id: TyConId,
    span: Span,
) -> IrExpr {
    if tycon_id == INT_TYCON {
        make_to_text_call(ctx, arg, "std.int", span)
    } else if tycon_id == FLOAT_TYCON {
        make_to_text_call(ctx, arg, "std.float", span)
    } else if tycon_id == BOOL_TYCON {
        make_to_text_call(ctx, arg, "std.bool", span)
    } else if tycon_id == TIMESTAMP_TYCON {
        make_to_text_call(ctx, arg, "std.time", span)
    } else {
        // Text (TyConId 3) and all user-defined types: identity.
        arg
    }
}

/// Build `Call(Stdlib { module, name: "toText" }, [arg])`.
///
/// Shared with the derived-instance lowering pass, which constructs the
/// same `std.<x>.toText` dispatch for record and union payloads.
pub(crate) fn make_to_text_call(
    ctx: &mut LowerCtx<'_>,
    arg: IrExpr,
    module: &str,
    span: Span,
) -> IrExpr {
    let callee_id = ctx.fresh_id(None);
    let call_id = ctx.fresh_id(None);
    let callee = Box::new(IrExpr::Symbol {
        id: callee_id,
        sym: SymbolRef::Stdlib {
            module: module.into(),
            name: "toText".into(),
        },
        span,
    });
    IrExpr::Call {
        id: call_id,
        callee,
        args: vec![arg],
        span,
    }
}

/// Build `Call(Stdlib { module: "std.text", name: "concat" }, [lhs, rhs])`.
///
/// Shared with the derived-instance lowering pass, which uses the same
/// `std.text.concat` primitive to join rendered field and literal chunks.
pub(crate) fn make_concat_call(
    ctx: &mut LowerCtx<'_>,
    lhs: IrExpr,
    rhs: IrExpr,
    span: Span,
) -> IrExpr {
    let callee_id = ctx.fresh_id(None);
    let call_id = ctx.fresh_id(None);
    let callee = Box::new(IrExpr::Symbol {
        id: callee_id,
        sym: SymbolRef::Stdlib {
            module: "std.text".into(),
            name: "concat".into(),
        },
        span,
    });
    IrExpr::Call {
        id: call_id,
        callee,
        args: vec![lhs, rhs],
        span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{expr::InterpPart, Literal, Span};
    use ridge_ir::{IrExpr, IrLit, SymbolRef};
    use ridge_resolve::{ModuleId, NodeIdMap, NodeKind};
    use ridge_types::{TyConId, Type};

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    /// Build a `LowerCtx` whose `node_types` table has a single entry at index
    /// `node_id` with the given `Type`, and whose `node_id_map` maps
    /// `(Span::point(0), NodeKind::Expr)` → `NodeId(node_id)`.
    ///
    /// The span `Span::point(0)` matches `sp()` (used by test expressions), so
    /// type lookups for expressions at span 0 will succeed.
    fn ctx_with_type_at(node_id: u32, ty: Type) -> LowerCtx<'static> {
        // node_types is indexed by NodeId.0; allocate slots 0..=node_id.
        let mut node_types = vec![None; (node_id + 1) as usize];
        node_types[node_id as usize] = Some(ty);
        // Box::leak so that the slice has 'static lifetime for the test.
        let leaked: &'static [Option<Type>] = Box::leak(node_types.into_boxed_slice());
        let mut ctx = LowerCtx::new(ModuleId(0), leaked);
        // Wire node_id_map: (Span::point(0), NodeKind::Expr) → NodeId(node_id).
        // This matches the test expressions which all use sp() = Span::point(0).
        let mut nid_map = NodeIdMap::default();
        nid_map
            .assign(Span::point(0), NodeKind::Expr)
            .expect("NodeIdMap assign failed in test setup");
        // If node_id > 0, stamp dummy entries to advance the counter.
        // (For these tests node_id is always 0, so the assigned id = NodeId(0).)
        ctx.node_id_map = Some(nid_map);
        ctx
    }

    fn text_part(raw: &str) -> InterpPart {
        InterpPart::Text {
            raw: raw.into(),
            span: sp(),
        }
    }

    fn expr_part(expr: ridge_ast::Expr) -> InterpPart {
        InterpPart::Expr {
            expr: Box::new(expr),
            span: sp(),
        }
    }

    fn int_expr() -> ridge_ast::Expr {
        ridge_ast::Expr::Literal(Literal::IntDec {
            raw: "1".into(),
            span: sp(),
        })
    }

    fn bool_expr() -> ridge_ast::Expr {
        ridge_ast::Expr::Literal(Literal::Bool {
            value: true,
            span: sp(),
        })
    }

    fn float_expr() -> ridge_ast::Expr {
        ridge_ast::Expr::Literal(Literal::Float {
            raw: "1.0".into(),
            span: sp(),
        })
    }

    fn text_expr(raw: &str) -> ridge_ast::Expr {
        ridge_ast::Expr::Literal(Literal::Text {
            raw: format!("\"{raw}\""),
            span: sp(),
        })
    }

    fn timestamp_expr() -> ridge_ast::Expr {
        // Timestamps have no AST literal — use a Unit as a placeholder and
        // force the type by constructing a ctx with the Timestamp TyConId.
        ridge_ast::Expr::Unit(sp())
    }

    // ── T9-i-1: single Text part emits a text literal (fast-path via core) ────
    //
    // The fast path is in `core::lower_interp`; this test covers the module's
    // behaviour when `parts` has exactly one text part.
    #[test]
    fn single_text_part_emits_text_lit() {
        let mut ctx = fresh_ctx();
        let parts = vec![text_part("hello")];
        let ir = lower_interp_full(&mut ctx, &parts, sp());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Lit {
                value: IrLit::Text(ref s),
                ..
            } => assert_eq!(s, "hello"),
            other => panic!("expected IrLit::Text, got {other:?}"),
        }
    }

    // ── T9-i-2: two Text parts produce a single concat call ──────────────────
    #[test]
    fn two_text_parts_produce_concat() {
        let mut ctx = fresh_ctx();
        let parts = vec![text_part("a"), text_part("b")];
        let ir = lower_interp_full(&mut ctx, &parts, sp());

        // Errors: we get L007 for the Text part but wait — Text parts are
        // *literals*, not expr holes, so no ToText dispatch is needed.
        // Both parts are Text — no errors expected.
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Call { callee, args, .. } => {
                match *callee {
                    IrExpr::Symbol {
                        sym:
                            SymbolRef::Stdlib {
                                ref module,
                                ref name,
                            },
                        ..
                    } => {
                        assert_eq!(module, "std.text");
                        assert_eq!(name, "concat");
                    }
                    ref other => panic!("expected Stdlib concat callee, got {other:?}"),
                }
                assert_eq!(args.len(), 2, "concat takes 2 args");
                match &args[0] {
                    IrExpr::Lit {
                        value: IrLit::Text(s),
                        ..
                    } => assert_eq!(s, "a"),
                    other => panic!("arg[0] expected Text(a), got {other:?}"),
                }
                match &args[1] {
                    IrExpr::Lit {
                        value: IrLit::Text(s),
                        ..
                    } => assert_eq!(s, "b"),
                    other => panic!("arg[1] expected Text(b), got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }
    }

    // ── T9-i-3: Int hole wraps in std.int.toText ──────────────────────────────
    //
    // Requires node_types to be populated so the type lookup succeeds.
    #[test]
    fn int_hole_wraps_to_text() {
        // span().start == 0, so proxy_nid = 0.
        let mut ctx = ctx_with_type_at(0, Type::Con(INT_TYCON, vec![]));
        let parts = vec![expr_part(int_expr())];
        let ir = lower_interp_full(&mut ctx, &parts, sp());

        // No errors expected — Int is in the closed set.
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        // Result is Call(std.int.toText, [inner]).
        match ir {
            IrExpr::Call { callee, args, .. } => {
                match *callee {
                    IrExpr::Symbol {
                        sym:
                            SymbolRef::Stdlib {
                                ref module,
                                ref name,
                            },
                        ..
                    } => {
                        assert_eq!(module, "std.int");
                        assert_eq!(name, "toText");
                    }
                    ref other => panic!("expected std.int.toText, got {other:?}"),
                }
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected Call(toText), got {other:?}"),
        }
    }

    // ── T9-i-4: Float hole wraps in std.float.toText ─────────────────────────
    #[test]
    fn float_hole_wraps_to_text() {
        let mut ctx = ctx_with_type_at(0, Type::Con(FLOAT_TYCON, vec![]));
        let parts = vec![expr_part(float_expr())];
        let ir = lower_interp_full(&mut ctx, &parts, sp());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Call { callee, .. } => match *callee {
                IrExpr::Symbol {
                    sym:
                        SymbolRef::Stdlib {
                            ref module,
                            ref name,
                        },
                    ..
                } => {
                    assert_eq!(module, "std.float");
                    assert_eq!(name, "toText");
                }
                ref other => panic!("expected std.float.toText, got {other:?}"),
            },
            other => panic!("expected Call(toText), got {other:?}"),
        }
    }

    // ── T9-i-5: Bool hole wraps in std.bool.toText ───────────────────────────
    #[test]
    fn bool_hole_wraps_to_text() {
        let mut ctx = ctx_with_type_at(0, Type::Con(BOOL_TYCON, vec![]));
        let parts = vec![expr_part(bool_expr())];
        let ir = lower_interp_full(&mut ctx, &parts, sp());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Call { callee, .. } => match *callee {
                IrExpr::Symbol {
                    sym:
                        SymbolRef::Stdlib {
                            ref module,
                            ref name,
                        },
                    ..
                } => {
                    assert_eq!(module, "std.bool");
                    assert_eq!(name, "toText");
                }
                ref other => panic!("expected std.bool.toText, got {other:?}"),
            },
            other => panic!("expected Call(toText), got {other:?}"),
        }
    }

    // ── T9-i-6: Text hole is identity (no wrapper) ───────────────────────────
    #[test]
    fn text_hole_is_identity() {
        let mut ctx = ctx_with_type_at(0, Type::Con(TEXT_TYCON, vec![]));
        let parts = vec![expr_part(text_expr("hi"))];
        let ir = lower_interp_full(&mut ctx, &parts, sp());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        // No toText call — the hole is returned directly.
        match ir {
            IrExpr::Lit {
                value: IrLit::Text(_),
                ..
            } => {}
            other => panic!("expected bare IrLit::Text for Text hole, got {other:?}"),
        }
    }

    // ── T9-i-7: Timestamp hole wraps in std.time.toText ──────────────────────
    #[test]
    fn timestamp_hole_wraps_to_text() {
        let mut ctx = ctx_with_type_at(0, Type::Con(TIMESTAMP_TYCON, vec![]));
        let parts = vec![expr_part(timestamp_expr())];
        let ir = lower_interp_full(&mut ctx, &parts, sp());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Call { callee, .. } => match *callee {
                IrExpr::Symbol {
                    sym:
                        SymbolRef::Stdlib {
                            ref module,
                            ref name,
                        },
                    ..
                } => {
                    assert_eq!(module, "std.time");
                    assert_eq!(name, "toText");
                }
                ref other => panic!("expected std.time.toText, got {other:?}"),
            },
            other => panic!("expected Call(toText), got {other:?}"),
        }
    }

    // ── T9-i-8: Type::Error hole is absorbing — no wrapper, no L007 ──────────
    #[test]
    fn error_type_hole_is_absorbing() {
        let mut ctx = ctx_with_type_at(0, Type::Error);
        let parts = vec![expr_part(int_expr())];
        let ir = lower_interp_full(&mut ctx, &parts, sp());

        // No L007 for Error type — absorbing semantics.
        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Type::Error; got: {:?}",
            ctx.errors
        );
        // Result is the raw inner (not wrapped in toText).
        match ir {
            IrExpr::Lit { .. } => {} // inner was an int literal → Lit
            other => panic!("expected bare inner for Error type, got {other:?}"),
        }
    }

    // ── T9-i-9: Unknown type emits L007 ──────────────────────────────────────
    //
    // Uses a TyConId not in the closed set (e.g. List = TyConId(6)).
    #[test]
    fn unknown_type_emits_l007() {
        let list_tycon = TyConId(6);
        let mut ctx = ctx_with_type_at(0, Type::Con(list_tycon, vec![]));
        let parts = vec![expr_part(int_expr())];
        let _ir = lower_interp_full(&mut ctx, &parts, sp());

        assert_eq!(
            ctx.errors.len(),
            1,
            "expected 1 L007 error; got: {:?}",
            ctx.errors
        );
        assert_eq!(ctx.errors[0].code(), "L007");
    }

    // ── T9-i-10: R3 — left-fold side-effect order (§7.1) ─────────────────────
    //
    // `$"${a}${b}"` → parts: [Text "", Expr a, Text "", Expr b, Text ""]
    // Expected IR (left-fold):
    //   concat(concat(concat(concat("", a), ""), b), "")
    //
    // This verifies that `a` evaluates before `b` because it appears as the
    // left (first) argument to the outermost concat — strictly left-to-right.
    /// Count how many left-nested `std.text.concat` calls wrap a value.
    ///
    /// Used by [`r3_left_fold_order`] to verify the fold direction.
    fn count_concat_depth(expr: &IrExpr) -> usize {
        match expr {
            IrExpr::Call { callee, args, .. } => {
                if let IrExpr::Symbol {
                    sym: SymbolRef::Stdlib { name, .. },
                    ..
                } = callee.as_ref()
                {
                    if name == "concat" && args.len() == 2 {
                        return 1 + count_concat_depth(&args[0]);
                    }
                }
                0
            }
            _ => 0,
        }
    }

    #[test]
    fn r3_left_fold_order() {
        let span_a = sp_at(2, 3); // "a" at offset 2
        let span_b = sp_at(6, 7); // "b" at offset 6

        let a_expr = ridge_ast::Expr::Literal(Literal::IntDec {
            raw: "1".into(),
            span: span_a,
        });
        let b_expr = ridge_ast::Expr::Literal(Literal::IntDec {
            raw: "2".into(),
            span: span_b,
        });

        let parts = vec![
            text_part(""),
            InterpPart::Expr {
                expr: Box::new(a_expr),
                span: span_a,
            },
            text_part(""),
            InterpPart::Expr {
                expr: Box::new(b_expr),
                span: span_b,
            },
            text_part(""),
        ];

        let mut ctx = fresh_ctx(); // no node_types — L007 will fire for expr holes
        let ir = lower_interp_full(&mut ctx, &parts, sp());

        // There will be L007 errors for both expr holes (node_types is empty).
        // What we care about is the STRUCTURE: left-fold nesting.
        //
        // The outermost node must be concat(something, "").
        // The left arg of that must be concat(something, b_lit).
        // The left arg of THAT must be concat(something, "").
        // And so on — 4 concat calls for 5 parts.
        let depth = count_concat_depth(&ir);
        assert_eq!(
            depth, 4,
            "5 parts → 4 left-nested concats (R3); got depth {depth}"
        );

        // Additionally verify the outermost right arg is "" (last text part).
        match &ir {
            IrExpr::Call { args, .. } => match &args[1] {
                IrExpr::Lit {
                    value: IrLit::Text(s),
                    ..
                } => assert_eq!(s, "", "last concat rhs must be empty string"),
                other => panic!("expected Text(\"\") as outermost rhs, got {other:?}"),
            },
            other => panic!("expected outer concat Call, got {other:?}"),
        }
    }
}
