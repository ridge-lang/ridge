//! `match`-expression and pattern lowering rules — §4.8 / §4.8.1.
//!
//! # `lower_match`
//!
//! Lowers `Expr::Match { scrutinee, arms }` to `IrExpr::Match` by:
//! - lowering the scrutinee via `lower_expr`,
//! - mapping each arm: pattern via `lower_pattern_full`, guard via `lower_expr`
//!   (preserved verbatim), body via `lower_expr`.
//!
//! # `lower_pattern_full`
//!
//! Full pattern lowering table (§4.8.1):
//!
//! | AST pattern | IR pattern |
//! |---|---|
//! | `Wildcard` | `IrPat::Wild` |
//! | `Literal` | `IrPat::Lit` |
//! | `Var { name }` | `IrPat::Bind { name, inner: None }` |
//! | `Constructor { fields: Some(fps) }` (record) | `IrPat::Ctor { fields, args: [] }` |
//! | `Constructor { fields: None, args }` (positional) | `IrPat::Ctor { fields: [], args }` |
//! | `Tuple { elems }` | `IrPat::Tuple { elems }` |
//! | `Cons { head, tail }` | `IrPat::Cons { head, tail }` |
//! | `As { name, inner }` | `IrPat::Bind { name, inner: Some(lower_pattern_full(inner)) }` |
//! | `Paren { inner }` | `lower_pattern_full(inner)` (paren erasure) |
//!
//! **Shorthand field expansion (D053).** `FieldPattern { name, pattern: None }` expands
//! to `(name, IrPat::Bind { name, inner: None })` — the IR carries no shorthand form.
//!
//! **`SymbolRef` for constructors.** The `BindingMap` maps the constructor name's
//! `NodeId` to `Binding::Constructor { owner_type: SymbolId, variant }`. The
//! `SymbolId` is translated to a `TyConId` via
//! `LowerCtx::lookup_constructor_tycon`, which reads the per-module `SymbolTable`
//! to find the owner type's source name and then resolves it through
//! `lookup_tycon_by_name`. Falls back to `TyConId(0)` when the symbol table or
//! workspace is absent. // OQ-PHASE45-007

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{pattern::FieldPattern, Expr, Ident, Pattern, Span};
use ridge_ir::symbol::CtorKind;
use ridge_ir::{IrArm, IrExpr, IrPat, SymbolRef};
use ridge_resolve::{imports::Binding, NodeKind};
use ridge_types::TyConId;

use crate::core::lower_expr;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// Re-export the literal helper from core by duplicating it here to avoid
// a circular dependency between core ↔ match_lower.
use ridge_ast::Literal;

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower `match scrutinee { arms }` to `IrExpr::Match`.
///
/// Each arm's pattern is lowered via [`lower_pattern_full`]; the optional
/// `when` guard and the body are lowered via [`lower_expr`].  The arm
/// ordering is preserved (source order).
pub fn lower_match(
    ctx: &mut LowerCtx<'_>,
    scrutinee: &Expr,
    arms: &[ridge_ast::expr::MatchArm],
    span: Span,
) -> IrExpr {
    let id = ctx.fresh_id(None);
    let scrutinee_ir = Box::new(lower_expr(ctx, scrutinee));

    let arms_ir: Vec<IrArm> = arms
        .iter()
        .map(|arm| {
            let pat = lower_pattern_full(ctx, &arm.pattern);
            let when = arm.guard.as_ref().map(|g| lower_expr(ctx, g));
            let body = lower_expr(ctx, &arm.body);
            IrArm {
                pat,
                when,
                body,
                span: arm.span,
            }
        })
        .collect();

    IrExpr::Match {
        id,
        scrutinee: scrutinee_ir,
        arms: arms_ir,
        span,
    }
}

/// Lower an AST [`Pattern`] to its [`IrPat`] equivalent (full lowering).
///
/// Unlike the stub in `core::lower_pattern`, this function handles all
/// pattern variants including `Constructor`, `Tuple`, `Cons`, `As`, and
/// `Paren`.
pub fn lower_pattern_full(ctx: &mut LowerCtx<'_>, pat: &Pattern) -> IrPat {
    match pat {
        // ── Atom patterns ──────────────────────────────────────────────────────
        Pattern::Wildcard { span } => IrPat::Wild { span: *span },

        Pattern::Literal { lit, span } => IrPat::Lit {
            value: literal_to_ir_lit(lit),
            span: *span,
        },

        Pattern::Var { name, span } => IrPat::Bind {
            name: name.text.clone(),
            inner: None,
            span: *span,
        },

        // ── Paren erasure ──────────────────────────────────────────────────────
        Pattern::Paren { inner, .. } => lower_pattern_full(ctx, inner),

        // OQ-L009: As-pattern lowers to Bind { inner: Some(...) } — same variant as
        // plain variable bindings but with inner populated.
        // ── As-pattern ────────────────────────────────────────────────────────
        // `name @ inner` → Bind { name, inner: Some(lower_pattern_full(inner)) }
        Pattern::As { name, inner, span } => IrPat::Bind {
            name: name.text.clone(),
            inner: Some(Box::new(lower_pattern_full(ctx, inner))),
            span: *span,
        },

        // ── Tuple pattern ─────────────────────────────────────────────────────
        Pattern::Tuple { elems, span } => {
            let lowered: Vec<IrPat> = elems.iter().map(|e| lower_pattern_full(ctx, e)).collect();
            IrPat::Tuple {
                elems: lowered,
                span: *span,
            }
        }

        // ── Cons pattern ──────────────────────────────────────────────────────
        // `head :: tail` → Cons { head, tail }
        Pattern::Cons { head, tail, span } => IrPat::Cons {
            head: Box::new(lower_pattern_full(ctx, head)),
            tail: Box::new(lower_pattern_full(ctx, tail)),
            span: *span,
        },

        // ── Empty-list pattern `[]` ───────────────────────────────────────────
        Pattern::ListNil { span } => IrPat::Nil { span: *span },

        // ── Constructor pattern ───────────────────────────────────────────────
        Pattern::Constructor {
            name,
            fields,
            args,
            span,
        } => lower_constructor_pattern(ctx, name, fields.as_deref(), args, *span),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Lower a constructor pattern to `IrPat::Ctor`.
///
/// Resolves the constructor `SymbolRef` via the `BindingMap` attached to
/// `ctx`.  If the binding is absent or the `NodeId` is missing, emits a
/// defensive `L999` and returns `IrPat::Wild { span }` so the surrounding
/// tree remains structurally valid.
///
/// OQ-PHASE45-007: `owner_type` is now resolved via
/// `LowerCtx::lookup_constructor_tycon(owner_sym_id)` which translates the
/// resolve-layer `SymbolId` to a `TyConId` via the symbol table. Falls back
/// to `TyConId(0)` when the symbol table or workspace is absent (defensive).
fn lower_constructor_pattern(
    ctx: &mut LowerCtx<'_>,
    name: &Ident,
    fields: Option<&[FieldPattern]>,
    args: &[Pattern],
    span: Span,
) -> IrPat {
    let node_id = ctx
        .node_id_map
        .as_ref()
        .and_then(|m| m.get(name.span, NodeKind::Ident));

    let binding = node_id.and_then(|nid| {
        ctx.binding_map
            .and_then(|bm| bm.get(nid.0 as usize).and_then(Option::as_ref))
    });

    let sym = match binding {
        Some(Binding::Constructor {
            owner_type: owner_sym_id,
            variant,
            ..
        }) => {
            // OQ-PHASE45-007: resolve SymbolId → TyConId via lookup_constructor_tycon.
            // Falls back to TyConId(0) when the symbol table or workspace is absent.
            let tycon_id = ctx
                .lookup_constructor_tycon(*owner_sym_id)
                .unwrap_or(TyConId(0));

            // Determine CtorKind from the pattern's fields presence: record form
            // (`Foo { x, y }`) → Record, positional form (`Foo a b` or bare
            // `Foo`) → UnionVariant.  Match-pattern lowering can use the syntax
            // form here because the field-vs-positional distinction is
            // syntactically clear in patterns; expression lowering cannot
            // (B-D013) and must rely on the resolver's `is_record` flag.
            let ctor_kind = if fields.is_some() {
                CtorKind::Record
            } else {
                CtorKind::UnionVariant
            };

            SymbolRef::Constructor {
                ctor_kind,
                owner_type: tycon_id,
                name: name.text.clone(),
                variant: *variant,
            }
        }

        // Prelude constructors (`Some`, `None`, `Ok`, `Err`) are bound by the
        // resolve phase as `Binding::StdlibSymbol` (OQ-R013 / prelude contract).
        // They are not `Binding::Constructor`; lower them directly to
        // `IrPat::Ctor { sym: SymbolRef::Prelude { name } }`.
        Some(Binding::StdlibSymbol { name: sym_name, .. }) => {
            let prelude_name = sym_name.clone();
            // Only the four known prelude constructors map to IrPat::Ctor.
            // For anything else fall through to the error arm below.
            match prelude_name.as_str() {
                "Some" | "None" | "Ok" | "Err" => {
                    let sym = SymbolRef::Prelude { name: prelude_name };
                    let ir_args: Vec<IrPat> =
                        args.iter().map(|a| lower_pattern_full(ctx, a)).collect();
                    return IrPat::Ctor {
                        sym,
                        fields: vec![],
                        args: ir_args,
                        span,
                    };
                }
                _ => {
                    ctx.errors.push(LowerError::InternalLoweringError {
                        span,
                        message: format!(
                            "StdlibSymbol `{sym_name}` is not a known prelude constructor pattern"
                        ),
                    });
                    return IrPat::Wild { span };
                }
            }
        }

        None => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!(
                    "no binding found for constructor `{}` at {span:?}; \
                     binding map absent or NodeId missing",
                    name.text
                ),
            });
            return IrPat::Wild { span };
        }

        Some(_) => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!(
                    "constructor `{}` has unexpected binding variant (not Binding::Constructor)",
                    name.text
                ),
            });
            return IrPat::Wild { span };
        }
    };

    if let Some(fps) = fields {
        // Record-body form: `Constructor { field1, field2 = pat }`
        let ir_fields: Vec<(String, IrPat)> = fps
            .iter()
            .map(|fp| field_pattern_to_pair(ctx, fp))
            .collect();
        IrPat::Ctor {
            sym,
            fields: ir_fields,
            args: vec![],
            span,
        }
    } else {
        // Positional form: `Constructor arg1 arg2`
        let ir_args: Vec<IrPat> = args.iter().map(|a| lower_pattern_full(ctx, a)).collect();
        IrPat::Ctor {
            sym,
            fields: vec![],
            args: ir_args,
            span,
        }
    }
}

/// Expand a `FieldPattern` to a `(field_name, IrPat)` pair.
///
/// Shorthand fields (D053: `FieldPattern { pattern: None }`) expand to
/// `(name, IrPat::Bind { name, inner: None })` — the IR has no shorthand form.
fn field_pattern_to_pair(ctx: &mut LowerCtx<'_>, fp: &FieldPattern) -> (String, IrPat) {
    let name = fp.name.text.clone();
    // Shorthand fields (D053: `pattern: None`) expand to `(name, Bind { name, inner: None })`.
    let pat = fp.pattern.as_ref().map_or_else(
        || IrPat::Bind {
            name: name.clone(),
            inner: None,
            span: fp.span,
        },
        |p| lower_pattern_full(ctx, p),
    );
    (name, pat)
}

/// Convert an AST [`Literal`] to an [`IrLit`] in a pattern context.
///
/// Pattern contexts have no `LowerCtx` available for error reporting, so
/// parse failures fall back to neutral values.
fn literal_to_ir_lit(lit: &Literal) -> ridge_ir::IrLit {
    match lit {
        Literal::IntDec { raw, .. } => {
            let cleaned = raw.replace('_', "");
            cleaned
                .parse::<i64>()
                .map(ridge_ir::IrLit::Int)
                .unwrap_or(ridge_ir::IrLit::Int(0))
        }
        Literal::IntBin { raw, .. } => {
            let cleaned = raw.trim_start_matches("0b").replace('_', "");
            i64::from_str_radix(&cleaned, 2)
                .map(ridge_ir::IrLit::Int)
                .unwrap_or(ridge_ir::IrLit::Int(0))
        }
        Literal::IntOct { raw, .. } => {
            let cleaned = raw.trim_start_matches("0o").replace('_', "");
            i64::from_str_radix(&cleaned, 8)
                .map(ridge_ir::IrLit::Int)
                .unwrap_or(ridge_ir::IrLit::Int(0))
        }
        Literal::IntHex { raw, .. } => {
            let cleaned = raw.trim_start_matches("0x").replace('_', "");
            i64::from_str_radix(&cleaned, 16)
                .map(ridge_ir::IrLit::Int)
                .unwrap_or(ridge_ir::IrLit::Int(0))
        }
        Literal::Float { raw, .. } => {
            let cleaned = raw.replace('_', "");
            cleaned
                .parse::<f64>()
                .map(ridge_ir::IrLit::Float)
                .unwrap_or(ridge_ir::IrLit::Float(0.0))
        }
        Literal::Bool { value, .. } => ridge_ir::IrLit::Bool(*value),
        Literal::Text { raw, .. } => {
            let inner = raw.strip_prefix('"').unwrap_or(raw);
            let inner = inner.strip_suffix('"').unwrap_or(inner);
            ridge_ir::IrLit::Text(inner.to_owned())
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{expr::MatchArm, pattern::FieldPattern, Ident, Literal, Pattern, Span};
    use ridge_ir::{IrExpr, IrLit, IrPat};
    use ridge_resolve::{imports::Binding, BindingMap, ModuleId, NodeIdMap, NodeKind};

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn int_pat(n: &str) -> Pattern {
        Pattern::Literal {
            lit: Literal::IntDec {
                raw: n.into(),
                span: sp(),
            },
            span: sp(),
        }
    }

    fn wild_pat() -> Pattern {
        Pattern::Wildcard { span: sp() }
    }

    fn var_pat(name: &str) -> Pattern {
        Pattern::Var {
            name: Ident {
                text: name.into(),
                span: sp(),
            },
            span: sp(),
        }
    }

    fn int_arm(n: &str, body_n: &str) -> MatchArm {
        MatchArm {
            pattern: int_pat(n),
            guard: None,
            body: Expr::Literal(Literal::Text {
                raw: format!("\"{body_n}\""),
                span: sp(),
            }),
            span: sp(),
        }
    }

    fn wild_arm(body_n: &str) -> MatchArm {
        MatchArm {
            pattern: wild_pat(),
            guard: None,
            body: Expr::Literal(Literal::Text {
                raw: format!("\"{body_n}\""),
                span: sp(),
            }),
            span: sp(),
        }
    }

    /// Build a `BindingMap` wiring `ctor_span` to `Binding::Constructor { owner_type: SymbolId(0), variant }`.
    fn ctor_binding_ctx(ctor_span: Span, variant: u32) -> LowerCtx<'static> {
        use ridge_resolve::SymbolId;

        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(ctor_span, NodeKind::Ident).unwrap();

        let mut binding_map: BindingMap = vec![None; (node_id.0 + 1) as usize];
        binding_map[node_id.0 as usize] = Some(Binding::Constructor {
            owner_type: SymbolId(0),
            variant,
            is_record: false,
        });

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(binding_map)));
        ctx
    }

    // ── T5-match-1: literal pattern arms ─────────────────────────────────────
    //
    // `match n { 1 -> "a", 2 -> "b", _ -> "c" }` produces 3 arms with
    // IrPat::Lit{Int(1)}, IrPat::Lit{Int(2)}, IrPat::Wild.

    #[test]
    fn lower_match_literal_pattern() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 30);
        let scrutinee = Expr::Literal(Literal::IntDec {
            raw: "0".into(),
            span: sp(),
        });
        let arms = vec![int_arm("1", "a"), int_arm("2", "b"), wild_arm("c")];

        let ir = lower_match(&mut ctx, &scrutinee, &arms, span);

        match ir {
            IrExpr::Match { arms: ir_arms, .. } => {
                assert_eq!(ir_arms.len(), 3);
                match &ir_arms[0].pat {
                    IrPat::Lit {
                        value: IrLit::Int(1),
                        ..
                    } => {}
                    other => panic!("arm 0 expected Lit(Int(1)), got {other:?}"),
                }
                match &ir_arms[1].pat {
                    IrPat::Lit {
                        value: IrLit::Int(2),
                        ..
                    } => {}
                    other => panic!("arm 1 expected Lit(Int(2)), got {other:?}"),
                }
                assert!(
                    matches!(&ir_arms[2].pat, IrPat::Wild { .. }),
                    "arm 2 expected Wild"
                );
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T5-match-2: tuple pattern ─────────────────────────────────────────────
    //
    // arm `(x, _) -> x`: pattern is IrPat::Tuple { elems: [Bind x, Wild] }

    #[test]
    fn lower_match_tuple_pattern() {
        let mut ctx = fresh_ctx();
        let span = sp();

        let tuple_pat = Pattern::Tuple {
            elems: vec![var_pat("x"), wild_pat()],
            span,
        };

        let ir_pat = lower_pattern_full(&mut ctx, &tuple_pat);

        match ir_pat {
            IrPat::Tuple { elems, .. } => {
                assert_eq!(elems.len(), 2);
                match &elems[0] {
                    IrPat::Bind {
                        name, inner: None, ..
                    } => assert_eq!(name, "x"),
                    other => panic!("elem 0 expected Bind x, got {other:?}"),
                }
                assert!(
                    matches!(&elems[1], IrPat::Wild { .. }),
                    "elem 1 expected Wild"
                );
            }
            other => panic!("expected IrPat::Tuple, got {other:?}"),
        }
    }

    // ── T5-match-3: cons pattern ──────────────────────────────────────────────
    //
    // arm `head :: tail -> head`: pattern is IrPat::Cons { head: Bind head, tail: Bind tail }

    #[test]
    fn lower_match_cons_pattern() {
        let mut ctx = fresh_ctx();

        let cons_pat = Pattern::Cons {
            head: Box::new(var_pat("head")),
            tail: Box::new(var_pat("tail")),
            span: sp(),
        };

        let ir_pat = lower_pattern_full(&mut ctx, &cons_pat);

        match ir_pat {
            IrPat::Cons { head, tail, .. } => {
                match *head {
                    IrPat::Bind {
                        ref name,
                        inner: None,
                        ..
                    } => assert_eq!(name, "head"),
                    ref other => panic!("head expected Bind head, got {other:?}"),
                }
                match *tail {
                    IrPat::Bind {
                        ref name,
                        inner: None,
                        ..
                    } => assert_eq!(name, "tail"),
                    ref other => panic!("tail expected Bind tail, got {other:?}"),
                }
            }
            other => panic!("expected IrPat::Cons, got {other:?}"),
        }
    }

    // ── T5-match-4: as-pattern ────────────────────────────────────────────────
    //
    // arm `x @ _ -> x`: IrPat::Bind { name: "x", inner: Some(Wild) }

    #[test]
    fn lower_match_as_pattern() {
        let mut ctx = fresh_ctx();

        let as_pat = Pattern::As {
            name: Ident {
                text: "x".into(),
                span: sp(),
            },
            inner: Box::new(wild_pat()),
            span: sp(),
        };

        let ir_pat = lower_pattern_full(&mut ctx, &as_pat);

        match ir_pat {
            IrPat::Bind {
                name,
                inner: Some(inner_box),
                ..
            } => {
                assert_eq!(name, "x");
                assert!(
                    matches!(*inner_box, IrPat::Wild { .. }),
                    "inner must be Wild"
                );
            }
            other => panic!("expected Bind {{ name: x, inner: Some(Wild) }}, got {other:?}"),
        }
    }

    // ── T5-match-5: when-guard preserved ─────────────────────────────────────
    //
    // arm `x when true -> x`: arm.when is Some(IrExpr::Lit { Bool(true) })

    #[test]
    fn lower_match_when_guard_preserved() {
        let mut ctx = fresh_ctx();
        let span = sp();

        let scrutinee = Expr::Literal(Literal::IntDec {
            raw: "0".into(),
            span,
        });

        let arms = vec![MatchArm {
            pattern: var_pat("x"),
            guard: Some(Expr::Literal(Literal::Bool { value: true, span })),
            body: Expr::Literal(Literal::IntDec {
                raw: "0".into(),
                span,
            }),
            span,
        }];

        let ir = lower_match(&mut ctx, &scrutinee, &arms, span);

        match ir {
            IrExpr::Match { arms: ir_arms, .. } => {
                assert_eq!(ir_arms.len(), 1);
                match &ir_arms[0].when {
                    Some(IrExpr::Lit {
                        value: IrLit::Bool(true),
                        ..
                    }) => {}
                    other => panic!("expected Some(Bool(true)) guard, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T5-match-6: constructor positional pattern ────────────────────────────
    //
    // arm `Some x -> x`: IrPat::Ctor { sym, fields: [], args: [Bind x] }
    // Uses a BindingMap wiring the `Some` ident to Binding::Constructor.

    #[test]
    fn lower_match_constructor_positional() {
        let ctor_span = sp_at(5, 9); // span of "Some"
        let mut ctx = ctor_binding_ctx(ctor_span, 0);

        let ctor_pat = Pattern::Constructor {
            name: Ident {
                text: "Some".into(),
                span: ctor_span,
            },
            fields: None,
            args: vec![var_pat("x")],
            span: sp_at(5, 11),
        };

        let ir_pat = lower_pattern_full(&mut ctx, &ctor_pat);

        // OQ-PHASE45-007: no error emitted — lookup_constructor_tycon falls
        // back to TyConId(0) silently when the symbol table is absent.
        // Previously an L999 / OQ-L013 placeholder error was emitted; that
        // behaviour was removed when the SymbolId→TyConId mapping was wired.
        assert!(
            ctx.errors.is_empty(),
            "expected no errors after wiring; got: {:?}",
            ctx.errors
        );

        match ir_pat {
            IrPat::Ctor {
                sym, fields, args, ..
            } => {
                assert!(fields.is_empty(), "positional ctor: no named fields");
                assert_eq!(args.len(), 1, "one positional arg");
                match &args[0] {
                    IrPat::Bind {
                        name, inner: None, ..
                    } => assert_eq!(name, "x"),
                    other => panic!("arg 0 expected Bind x, got {other:?}"),
                }
                match sym {
                    SymbolRef::Constructor {
                        name,
                        variant,
                        owner_type,
                        ..
                    } => {
                        assert_eq!(name, "Some");
                        assert_eq!(variant, 0);
                        // Without a workspace the fallback TyConId(0) is used.
                        assert_eq!(owner_type.0, 0, "fallback TyConId(0) when no workspace");
                    }
                    other => panic!("expected Constructor sym, got {other:?}"),
                }
            }
            other => panic!("expected IrPat::Ctor, got {other:?}"),
        }
    }

    // ── T5-match-7: record shorthand field expansion (D053) ───────────────────
    //
    // arm `User { name } -> name`: D053 shorthand expands to
    // fields: [("name", IrPat::Bind { name: "name", inner: None })]

    #[test]
    fn lower_match_record_shorthand_field() {
        let ctor_span = sp_at(0, 4); // span of "User"
        let mut ctx = ctor_binding_ctx(ctor_span, 0);

        let field_span = sp_at(7, 11);
        let fp = FieldPattern {
            name: Ident {
                text: "name".into(),
                span: field_span,
            },
            pattern: None, // shorthand
            span: field_span,
        };

        let ctor_pat = Pattern::Constructor {
            name: Ident {
                text: "User".into(),
                span: ctor_span,
            },
            fields: Some(vec![fp]),
            args: vec![],
            span: sp_at(0, 15),
        };

        let ir_pat = lower_pattern_full(&mut ctx, &ctor_pat);

        match ir_pat {
            IrPat::Ctor { fields, args, .. } => {
                assert!(args.is_empty(), "record ctor: no positional args");
                assert_eq!(fields.len(), 1, "one field");
                let (ref field_name, ref field_pat) = fields[0];
                assert_eq!(field_name, "name");
                match field_pat {
                    IrPat::Bind {
                        name, inner: None, ..
                    } => assert_eq!(name, "name"),
                    other => panic!("expected Bind {{ name: name, inner: None }}, got {other:?}"),
                }
            }
            other => panic!("expected IrPat::Ctor, got {other:?}"),
        }
    }
}
