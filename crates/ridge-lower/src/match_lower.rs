//! `match`-expression and pattern lowering rules.
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
//! Full pattern lowering table:
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
//! **Shorthand field expansion.** `FieldPattern { name, pattern: None }` expands
//! to `(name, IrPat::Bind { name, inner: None })` — the IR carries no shorthand form.
//!
//! **`SymbolRef` for constructors.** The `BindingMap` maps the constructor name's
//! `NodeId` to `Binding::Constructor { owner_type: SymbolId, variant }`. The
//! `SymbolId` is translated to a `TyConId` via
//! `LowerCtx::lookup_constructor_tycon`, which reads the per-module `SymbolTable`
//! to find the owner type's source name and then resolves it through
//! `lookup_tycon_by_name`. With no symbol table or workspace the pattern
//! carries no owner.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]
// Generated IR constants use usize→i64 casts for small loop indices/sizes.
// These values are bounded by list pattern lengths so the cast is safe.
#![allow(clippy::cast_possible_wrap)]

use ridge_ast::{pattern::FieldPattern, Expr, Ident, Pattern, Span};
use ridge_ir::symbol::CtorKind;
use ridge_ir::{IrArm, IrExpr, IrLit, IrPat, StdlibKind, SymbolRef};
use ridge_resolve::{imports::Binding, NodeKind};
use ridge_types::TyConId;

use crate::core::lower_expr;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// Numeric literal parsing is shared with `core` (`parse_int_*` helpers), so
// pattern-site and expression-site literals behave identically.
use ridge_ast::Literal;

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower `match scrutinee { arms }` to `IrExpr::Match`.
///
/// Each arm's pattern is lowered via [`lower_pattern_full`]; the optional
/// `when` guard and the body are lowered via [`lower_expr`].  The arm
/// ordering is preserved (source order).
///
/// Slice patterns retain their structural prefix at every nesting level. Their
/// list positions receive fresh bindings; length checks precede extraction and
/// user guards, without evaluating the match subject again.
pub fn lower_match(
    ctx: &mut LowerCtx<'_>,
    scrutinee: &Expr,
    arms: &[ridge_ast::expr::MatchArm],
    span: Span,
) -> IrExpr {
    // Expand each or-pattern arm `p1 | p2 -> body` into one arm per alternative
    // sharing the guard and body. BEAM has no native or-pattern, so this clause
    // duplication is how alternatives lower; every downstream step then sees
    // ordinary single-pattern arms.
    let expanded = expand_or_arms(arms);
    let arms: &[ridge_ast::expr::MatchArm] = &expanded;

    lower_match_simple(ctx, scrutinee, arms, span)
}

/// Expand every or-pattern arm into one arm per alternative, cloning the guard
/// and body. Non-or arms pass through unchanged.
fn expand_or_arms(arms: &[ridge_ast::expr::MatchArm]) -> Vec<ridge_ast::expr::MatchArm> {
    let mut out = Vec::with_capacity(arms.len());
    for arm in arms {
        if let Pattern::Or { alts, .. } = &arm.pattern {
            for alt in alts {
                out.push(ridge_ast::expr::MatchArm {
                    pattern: alt.clone(),
                    guard: arm.guard.clone(),
                    body: arm.body.clone(),
                    span: arm.span,
                });
            }
        } else {
            out.push(arm.clone());
        }
    }
    out
}

/// Lower structural patterns, deferring slice bindings until checks pass.
fn lower_match_simple(
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
            // Drain any by-value guards the pattern produced before lowering the
            // body/user-guard, so a nested `match` in either cannot mingle its
            // own pattern guards with this arm's.
            let pattern_guards = std::mem::take(&mut ctx.pending_pattern_guards);
            let extractions = std::mem::take(&mut ctx.pending_slice_extractions);
            let has_slice = std::mem::take(&mut ctx.pending_slice_pattern);
            let user_guard = arm.guard.as_ref().map(|g| lower_expr(ctx, g));
            let when = if has_slice {
                let pattern_guard = combine_pattern_guards(ctx, pattern_guards, None, arm.span);
                crate::slice_pattern::guarded(
                    ctx,
                    pattern_guard,
                    user_guard,
                    arm.guard.as_ref(),
                    &extractions,
                    arm.span,
                )
            } else {
                combine_pattern_guards(ctx, pattern_guards, user_guard, arm.span)
            };
            let body = lower_expr(ctx, &arm.body);
            let body = crate::slice_pattern::bind(ctx, extractions, body);
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

/// Build the by-value guard `Decimal.compare(<bind>, <literal>) == 0` for a
/// decimal-literal pattern.
///
/// `bind_name` is the fresh local the arm pattern binds the scrutinee to; `raw`
/// is the literal lexeme, still carrying its `m`/`M` suffix and any `_`
/// separators. The literal is rebuilt from its digits exactly as
/// [`crate::core::lower_literal`] does for a decimal *expression* — strip the
/// suffix and separators, then `std.decimal.parseStrict`. Comparison is by value,
/// so a `1.5m` pattern matches a `1.50` scrutinee.
///
/// `Decimal.compare` is not a BEAM guard BIF, so codegen lifts the whole guard
/// out of clause-guard position into a `case … of 'true' -> body` in the arm
/// body (see `ridge-codegen-erl::expr::lift_guarded_match`).
fn build_decimal_eq_guard(
    ctx: &mut LowerCtx<'_>,
    bind_name: &str,
    raw: &str,
    span: Span,
) -> IrExpr {
    // The scrutinee, referenced through the local the arm pattern binds it to.
    let scrut_ref = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: bind_name.to_owned(),
        span,
    };

    // The literal value: `std.decimal.parseStrict("<digits>")`.
    let text = raw.trim_end_matches(['m', 'M']).replace('_', "");
    let lit_expr = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.decimal".into(),
                name: "parseStrict".into(),
                kind: StdlibKind::Function,
            },
            span,
        }),
        args: vec![IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Text(text),
            span,
        }],
        span,
    };

    // Decimal.compare(scrut, lit) — a three-way compare returning -1 | 0 | 1.
    let compare_call = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.decimal".into(),
                name: "compare".into(),
                kind: StdlibKind::Function,
            },
            span,
        }),
        args: vec![scrut_ref, lit_expr],
        span,
    };

    // … == 0  →  std.op.eq(compare_result, 0).
    let zero = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Int(0),
        span,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.op".into(),
                name: "eq".into(),
                kind: StdlibKind::Function,
            },
            span,
        }),
        args: vec![compare_call, zero],
        span,
    }
}

/// Conjoin the by-value pattern guards drained from
/// [`LowerCtx::pending_pattern_guards`] with an optional user `when` guard into
/// a single guard expression.
///
/// Returns `None` when there is neither — preserving the exact prior lowering
/// for every arm without a decimal pattern. A single conjunct is returned as-is
/// (so a plain user guard stays byte-identical); multiple conjuncts fold left
/// with the internal boolean `and`, the same helper the list-slice guard uses.
fn combine_pattern_guards(
    ctx: &mut LowerCtx<'_>,
    pattern_guards: Vec<IrExpr>,
    user_guard: Option<IrExpr>,
    span: Span,
) -> Option<IrExpr> {
    let mut conjuncts = pattern_guards;
    if let Some(g) = user_guard {
        conjuncts.push(g);
    }
    let mut iter = conjuncts.into_iter();
    let first = iter.next()?;
    Some(iter.fold(first, |acc, next| and_guard(ctx, acc, next, span)))
}

/// `lhs and rhs` via the internal `__slice__.and` boolean helper (`erlang:and/2`).
///
/// When a conjunct is a decimal guard the whole expression is not a BEAM guard
/// BIF, so codegen lifts it into the arm body where `and/2` evaluates as an
/// ordinary call.
fn and_guard(ctx: &mut LowerCtx<'_>, lhs: IrExpr, rhs: IrExpr, span: Span) -> IrExpr {
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "and".into(),
                kind: StdlibKind::Function,
            },
            span,
        }),
        args: vec![lhs, rhs],
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

        // ── Inline record pattern ──────────────────────────────────────────────
        //
        // `{ f1, f2 = p, .. }` lowers to `IrPat::Ctor { Record, fields, args: [] }`,
        // exactly like the record-body form of `Pattern::Constructor`.
        //
        // The anonymous TyConId is taken from the scrutinee's inferred type recorded
        // in `node_types` by the typecheck pass.  When the node-type table is absent
        // (unit-test scaffolding) the pattern has no owner, and the IR is still
        // structurally correct without one.
        Pattern::Record { fields, span, .. } => {
            let anon_id: Option<TyConId> = ctx
                .node_id_map
                .as_ref()
                .and_then(|m| m.get(*span, NodeKind::Expr))
                .and_then(|nid| ctx.node_type(nid))
                .and_then(|ty| {
                    if let ridge_types::Type::Con(id, _) = ty {
                        Some(*id)
                    } else {
                        None
                    }
                });

            // Only the record's own decl may name it. With no id there is
            // nothing to look up, and naming it after whatever the arena
            // happens to hold at the front is how these patterns came out
            // labelled `Int`.
            let anon_name = anon_id
                .and_then(|id| ctx.workspace.and_then(|ws| ws.tycons.get(id.0 as usize)))
                .map_or_else(|| "{anon record}".to_string(), |d| d.name.clone());

            let ir_fields: Vec<(String, IrPat)> = fields
                .iter()
                .map(|fp| field_pattern_to_pair(ctx, fp))
                .collect();

            IrPat::Ctor {
                sym: SymbolRef::Constructor {
                    ctor_kind: CtorKind::Record,
                    owner_type: anon_id,
                    name: anon_name,
                    variant: 0,
                },
                fields: ir_fields,
                args: vec![],
                span: *span,
            }
        }

        Pattern::Literal { lit, span } => {
            // A decimal literal cannot match structurally: `1.5` and `1.50` are
            // equal by value but stored at different scales, so an `IrPat::Lit`
            // would match one written form and miss the other. Bind the
            // scrutinee position to a fresh local instead and defer the check to
            // a `Decimal.compare … == 0` guard, which the arm lowering conjoins
            // with any user `when`. Every other literal matches structurally.
            if let Literal::Decimal { raw, .. } = lit {
                let name = format!("_dec_pat_{}", ctx.fresh_id(None).0);
                let guard = build_decimal_eq_guard(ctx, &name, raw, *span);
                ctx.pending_pattern_guards.push(guard);
                return IrPat::Bind {
                    name,
                    inner: None,
                    span: *span,
                };
            }
            IrPat::Lit {
                value: literal_to_ir_lit(ctx, lit),
                span: *span,
            }
        }

        Pattern::Var { name, span } => IrPat::Bind {
            name: name.text.clone(),
            inner: None,
            span: *span,
        },

        // ── Paren erasure ──────────────────────────────────────────────────────
        Pattern::Paren { inner, .. } => lower_pattern_full(ctx, inner),

        // As-pattern lowers to Bind { inner: Some(...) } — same variant as
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

        // ── Bracketed list pattern `[a, b, ..]` ──────────────────────────────
        // Desugar to Cons/ListNil/Wildcard/Bind and lower the result.
        Pattern::List { elements, span } if pat.is_varlen_list() => {
            ctx.pending_slice_pattern = true;
            crate::slice_pattern::lower(ctx, elements, *span)
        }
        Pattern::List { .. } => lower_pattern_full(ctx, &pat.clone().desugar_list()),

        // ── Constructor pattern ───────────────────────────────────────────────
        Pattern::Constructor {
            name,
            fields,
            args,
            span,
            ..
        } => lower_constructor_pattern(ctx, name, fields.as_deref(), args, *span),

        // Or-patterns are expanded into one arm per alternative by
        // `expand_or_arms` before any pattern is lowered, so this is
        // unreachable; lower the first alternative defensively to stay total.
        Pattern::Or { alts, span } => alts
            .first()
            .map_or(IrPat::Wild { span: *span }, |p| lower_pattern_full(ctx, p)),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// The variant index of a `QExpr` constructor, or `None` if `name` is not one.
///
/// Mirrors the variant order interned for `QExpr` in `ridge_types::builtins`.
fn qexpr_variant(name: &str) -> Option<u32> {
    Some(match name {
        "QCol" => 0,
        "QLitInt" => 1,
        "QLitText" => 2,
        "QLitBool" => 3,
        "QLitFloat" => 4,
        "QAnd" => 5,
        "QOr" => 6,
        "QNot" => 7,
        "QEq" => 8,
        "QNe" => 9,
        "QLt" => 10,
        "QGt" => 11,
        "QLe" => 12,
        "QGe" => 13,
        "QProj" => 14,
        "QColR" => 15,
        "QGroupKey" => 16,
        "QAggCount" => 17,
        "QAggSum" => 18,
        "QAggAvg" => 19,
        "QAggMin" => 20,
        "QAggMax" => 21,
        "QColAt" => 22,
        "QNotTrue" => 23,
        _ => return None,
    })
}

/// Lower a constructor pattern to `IrPat::Ctor`.
///
/// Resolves the constructor `SymbolRef` via the `BindingMap` attached to
/// `ctx`.  If the binding is absent or the `NodeId` is missing, emits a
/// defensive `L999` and returns `IrPat::Wild { span }` so the surrounding
/// tree remains structurally valid.
///
/// `owner_type` is resolved via
/// `LowerCtx::lookup_constructor_tycon(owner_sym_id)`, which translates the
/// resolve-layer `SymbolId` to a `TyConId` via the symbol table. With no
/// symbol table or workspace the pattern carries no owner.
#[allow(
    clippy::too_many_lines,
    reason = "flat constructor-binding dispatch; the arms read best in one place"
)]
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
            is_record,
            ..
        }) => {
            // Resolve the resolver's `SymbolId` to the arena's `TyConId`. With no
            // symbol table or workspace there is nothing to resolve against, and
            // the pattern carries no owner.
            let tycon_id = ctx
                .constructor_sites
                .and_then(|sites| sites.get(&name.span))
                .map(|(owner, _)| *owner)
                .or_else(|| ctx.lookup_constructor_tycon(*owner_sym_id));

            // Record-payload union variant pattern `Login { userId, .. }`:
            // runtime shape `{'Login', #{userId := U, ..}}`. Nest a Record (map)
            // pattern in the tagged tuple's single slot so the UnionVariant and
            // Record codegen arms compose. Mirrors the construction lowering; the
            // resolver's `is_record` flag distinguishes this from a record *type*
            // auto-constructor pattern (`Point { x, y }`), which stays a bare map.
            if !*is_record {
                if let Some(fps) = fields {
                    let ir_fields: Vec<(String, IrPat)> = fps
                        .iter()
                        .map(|fp| field_pattern_to_pair(ctx, fp))
                        .collect();
                    let inner = IrPat::Ctor {
                        sym: SymbolRef::Constructor {
                            ctor_kind: CtorKind::Record,
                            owner_type: tycon_id,
                            name: name.text.clone(),
                            variant: 0,
                        },
                        fields: ir_fields,
                        args: vec![],
                        span,
                    };
                    return IrPat::Ctor {
                        sym: SymbolRef::Constructor {
                            ctor_kind: CtorKind::UnionVariant,
                            owner_type: tycon_id,
                            name: name.text.clone(),
                            variant: *variant,
                        },
                        fields: vec![],
                        args: vec![inner],
                        span,
                    };
                }
            }

            // Determine CtorKind from the pattern's fields presence: record form
            // (`Foo { x, y }`) → Record, positional form (`Foo a b` or bare
            // `Foo`) → UnionVariant.  Match-pattern lowering can use the syntax
            // form here because the field-vs-positional distinction is
            // syntactically clear in patterns; expression lowering cannot
            // expression lowering cannot determine this syntactically and must rely on the resolver's `is_record` flag.
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

        // Prelude constructors (`Some`, `None`, `Ok`, `Err`, and the seven
        // JsonValue variants) are bound by the resolve phase as
        // `Binding::StdlibSymbol` (prelude contract). They are not
        // `Binding::Constructor`; lower them directly to
        // `IrPat::Ctor { sym: SymbolRef::Prelude { name } }`.
        Some(Binding::StdlibSymbol { name: sym_name, .. }) => {
            let prelude_name = sym_name.clone();
            // QExpr constructors (the quotation tree) are prelude builtins. They
            // carry payloads, so they must lower to a real `UnionVariant`
            // pattern over QExpr's `TyConId` — matching the way the reify pass
            // constructs them — so codegen binds the payload variables.
            if let Some(variant) = qexpr_variant(&prelude_name) {
                let ir_args: Vec<IrPat> = args.iter().map(|a| lower_pattern_full(ctx, a)).collect();
                return IrPat::Ctor {
                    sym: SymbolRef::Constructor {
                        ctor_kind: CtorKind::UnionVariant,
                        owner_type: Some(ctx.builtins.q_expr),
                        name: prelude_name,
                        variant,
                    },
                    fields: vec![],
                    args: ir_args,
                    span,
                };
            }
            // Only the known prelude constructors map to IrPat::Ctor.
            // For anything else fall through to the error arm below.
            match prelude_name.as_str() {
                "Some" | "None" | "Ok" | "Err" | "JNull" | "JBool" | "JInt" | "JFloat"
                | "JText" | "JList" | "JObject" | "JObjectFields" => {
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
                    // A constructor of a reconciled stdlib type. Its owner/variant
                    // live only in the arena decl (resolve bound a bare
                    // `StdlibSymbol`), so resolve the shape from the reserved block
                    // and lower to a real `UnionVariant` pattern so codegen binds
                    // the payload variables — the same shape the construct side uses.
                    if let Some((owner_type, variant, _is_record)) =
                        ctx.lookup_stdlib_ctor(&prelude_name)
                    {
                        let ir_args: Vec<IrPat> =
                            args.iter().map(|a| lower_pattern_full(ctx, a)).collect();
                        return IrPat::Ctor {
                            sym: SymbolRef::Constructor {
                                ctor_kind: CtorKind::UnionVariant,
                                owner_type: Some(owner_type),
                                name: prelude_name,
                                variant,
                            },
                            fields: vec![],
                            args: ir_args,
                            span,
                        };
                    }
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

        // Resolve could not find the name, emitted its own R### diagnostic and
        // stamped `Binding::Error`. Lower to a wildcard and say nothing: the arm
        // below asserts something about a program that resolved clean, and every
        // pass runs even when an earlier one failed, so without this a misspelled
        // constructor in a match arm answers with an internal message on top of
        // the real one. Same treatment the expression side already gives it.
        Some(Binding::Error) => return IrPat::Wild { span },

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
/// Shorthand fields (`FieldPattern { pattern: None }`) expand to
/// `(name, IrPat::Bind { name, inner: None })` — the IR has no shorthand form.
fn field_pattern_to_pair(ctx: &mut LowerCtx<'_>, fp: &FieldPattern) -> (String, IrPat) {
    let name = fp.name.text.clone();
    // Shorthand fields (`pattern: None`) expand to `(name, Bind { name, inner: None })`.
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
/// Numeric literals share the expression-site parsers from [`crate::core`], so
/// base prefixes are case-insensitive and out-of-range values are reported as
/// `L110` instead of silently collapsing to zero.
fn literal_to_ir_lit(ctx: &mut LowerCtx<'_>, lit: &Literal) -> ridge_ir::IrLit {
    let span = lit.span();
    match lit {
        Literal::IntDec { raw, .. } => crate::core::parse_int_dec(ctx, raw, span),
        Literal::IntBin { raw, .. } => crate::core::parse_int_bin(ctx, raw, span),
        Literal::IntOct { raw, .. } => crate::core::parse_int_oct(ctx, raw, span),
        Literal::IntHex { raw, .. } => crate::core::parse_int_hex(ctx, raw, span),
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
        // Raw strings carry literal bytes; no escape decoding.
        Literal::RawText { raw, .. } => ridge_ir::IrLit::Text(raw.clone()),
        // A decimal-literal pattern is handled by the caller before it reaches
        // here — it lowers to a fresh binding plus a value-comparison guard, not
        // a structural `IrPat::Lit` — so this neutral value is never used.
        Literal::Decimal { .. } => ridge_ir::IrLit::Int(0),
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
        LowerCtx::new(ModuleId(0), &[], crate::test_support::builtins())
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

    #[test]
    fn decimal_literal_pattern_binds_and_defers_to_a_value_guard() {
        // A decimal literal cannot match structurally (1.5 == 1.50 by value but
        // the stored scale differs), so it lowers to a fresh binding and pushes
        // a value-comparison guard the arm lowering later conjoins with `when`.
        let mut ctx = fresh_ctx();
        let pat = Pattern::Literal {
            lit: Literal::Decimal {
                raw: "1.5m".into(),
                span: sp(),
            },
            span: sp(),
        };
        let ir = lower_pattern_full(&mut ctx, &pat);
        assert!(
            matches!(ir, IrPat::Bind { inner: None, .. }),
            "a decimal pattern must bind the scrutinee, got {ir:?}"
        );
        assert!(
            ctx.errors.is_empty(),
            "a decimal pattern is no longer an error, got {:?}",
            ctx.errors
        );
        assert_eq!(
            ctx.pending_pattern_guards.len(),
            1,
            "a decimal pattern must push exactly one value-comparison guard"
        );
    }

    // ── Integer literal pattern edge cases (B3) ──────────────────────────────

    /// Upper-case base prefixes in a pattern must parse to the same value as
    /// their lower-case form — previously `0B101` silently collapsed to a
    /// pattern for `0`, a miscompile.
    #[test]
    fn literal_pattern_uppercase_base_prefixes() {
        for (lit, expected) in [
            (
                Literal::IntBin {
                    raw: "0B101".into(),
                    span: sp(),
                },
                5,
            ),
            (
                Literal::IntOct {
                    raw: "0O777".into(),
                    span: sp(),
                },
                511,
            ),
            (
                Literal::IntHex {
                    raw: "0XFF".into(),
                    span: sp(),
                },
                255,
            ),
        ] {
            let mut ctx = fresh_ctx();
            let pat = Pattern::Literal { lit, span: sp() };
            let ir = lower_pattern_full(&mut ctx, &pat);
            assert!(
                ctx.errors.is_empty(),
                "expected no errors; got: {:?}",
                ctx.errors
            );
            match ir {
                IrPat::Lit {
                    value: IrLit::Int(n),
                    ..
                } => assert_eq!(n, expected),
                other => panic!("expected IrPat::Lit Int({expected}), got {other:?}"),
            }
        }
    }

    /// An out-of-range integer literal in a pattern reports `L110` instead of
    /// silently matching zero.
    #[test]
    fn literal_pattern_int_out_of_range_is_l110() {
        let mut ctx = fresh_ctx();
        let pat = Pattern::Literal {
            lit: Literal::IntDec {
                raw: "99999999999999999999".into(),
                span: sp(),
            },
            span: sp(),
        };
        let ir = lower_pattern_full(&mut ctx, &pat);
        assert_eq!(ctx.errors.len(), 1, "expected exactly one error");
        assert!(
            matches!(ctx.errors[0], LowerError::IntLiteralOutOfRange { .. }),
            "expected IntLiteralOutOfRange, got {:?}",
            ctx.errors[0]
        );
        assert_eq!(ctx.errors[0].code(), "L110");
        assert!(
            matches!(
                ir,
                IrPat::Lit {
                    value: IrLit::Int(0),
                    ..
                }
            ),
            "fallback pattern must be a neutral zero literal, got {ir:?}"
        );
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
    fn ctor_binding_ctx(ctor_span: Span, variant: u32, is_record: bool) -> LowerCtx<'static> {
        use ridge_resolve::SymbolId;

        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(ctor_span, NodeKind::Ident).unwrap();

        let mut binding_map: BindingMap = vec![None; (node_id.0 + 1) as usize];
        binding_map[node_id.0 as usize] = Some(Binding::Constructor {
            owner_type: SymbolId(0),
            variant,
            is_record,
            owner_module: ModuleId(0),
        });

        let mut ctx = LowerCtx::new(ModuleId(0), &[], crate::test_support::builtins());
        ctx.attach_bindings(nid_map, Box::leak(Box::new(binding_map)));
        ctx
    }

    // ── literal pattern arms ─────────────────────────────────────
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

    // ── tuple pattern ─────────────────────────────────────────────
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

    // ── cons pattern ──────────────────────────────────────────────
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

    // ── as-pattern ────────────────────────────────────────────────
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

    // ── when-guard preserved ─────────────────────────────────────
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

    // A refutable element in a slice suffix/middle position must be rejected
    // with L109 (the 0.2.8 restriction): `[.., 0]` has a literal in the suffix.
    #[test]
    fn lower_match_refutable_slice_suffix_emits_l109() {
        use ridge_ast::ListPatElem;

        let mut ctx = fresh_ctx();
        let span = sp();

        let scrutinee = Expr::Literal(Literal::IntDec {
            raw: "0".into(),
            span,
        });

        // `[.., 0]` — the suffix element is a refutable literal pattern.
        let pat = Pattern::List {
            elements: vec![
                ListPatElem::Rest { bind: None, span },
                ListPatElem::Elem(Pattern::Literal {
                    lit: Literal::IntDec {
                        raw: "0".into(),
                        span,
                    },
                    span,
                }),
            ],
            span,
        };
        let arms = vec![MatchArm {
            pattern: pat,
            guard: None,
            body: Expr::Literal(Literal::IntDec {
                raw: "1".into(),
                span,
            }),
            span,
        }];

        let _ = lower_match(&mut ctx, &scrutinee, &arms, span);

        assert!(
            ctx.errors
                .iter()
                .any(|e| matches!(e, LowerError::RefutableSliceElement { .. })),
            "expected L109 RefutableSliceElement for a literal in suffix position, got: {:?}",
            ctx.errors
        );
    }

    // ── constructor positional pattern ────────────────────────────
    //
    // arm `Some x -> x`: IrPat::Ctor { sym, fields: [], args: [Bind x] }
    // Uses a BindingMap wiring the `Some` ident to Binding::Constructor.

    #[test]
    fn lower_match_constructor_positional() {
        let ctor_span = sp_at(5, 9); // span of "Some"
        let mut ctx = ctor_binding_ctx(ctor_span, 0, false);

        let ctor_pat = Pattern::Constructor {
            qualifier: None,
            name: Ident {
                text: "Some".into(),
                span: ctor_span,
            },
            fields: None,
            has_rest: false,
            args: vec![var_pat("x")],
            span: sp_at(5, 11),
        };

        let ir_pat = lower_pattern_full(&mut ctx, &ctor_pat);

        // No error emitted: without a symbol table there is no owner to
        // resolve, and a constructor without one is not a lowering failure.
        // An L999 placeholder error used to fire here; it went away when the
        // SymbolId-to-TyConId mapping was wired.
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
                        // Without a workspace there is no owner to resolve.
                        assert_eq!(owner_type, None, "no owner without a workspace");
                    }
                    other => panic!("expected Constructor sym, got {other:?}"),
                }
            }
            other => panic!("expected IrPat::Ctor, got {other:?}"),
        }
    }

    // ── Record shorthand field expansion ──────────────────────────────────────
    //
    // arm `User { name } -> name`: shorthand expands to
    // fields: [("name", IrPat::Bind { name: "name", inner: None })]

    #[test]
    fn lower_match_record_shorthand_field() {
        let ctor_span = sp_at(0, 4); // span of "User"
                                     // `User { name }` models a record *type* auto-constructor pattern, so the
                                     // binding carries is_record = true (a bare record-map pattern). A record
                                     // *variant* (is_record = false) would nest under a tagged tuple instead.
        let mut ctx = ctor_binding_ctx(ctor_span, 0, true);

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
            qualifier: None,
            name: Ident {
                text: "User".into(),
                span: ctor_span,
            },
            fields: Some(vec![fp]),
            has_rest: false,
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

    // ── Record-payload union variant pattern ──────────────────────────────────
    //
    // arm `Login { userId } -> ...`: a record-*variant* pattern (is_record = false)
    // nests a Record (map) pattern inside the tagged tuple's single positional
    // slot — shape `{'Login', #{userId := userId}}`. Distinct from a record *type*
    // pattern, which stays a bare map.

    #[test]
    fn lower_match_record_variant_nests_under_tag() {
        let ctor_span = sp_at(0, 5); // span of "Login"
        let mut ctx = ctor_binding_ctx(ctor_span, 1, false);

        let field_span = sp_at(8, 14);
        let fp = FieldPattern {
            name: Ident {
                text: "userId".into(),
                span: field_span,
            },
            pattern: None, // shorthand
            span: field_span,
        };

        let ctor_pat = Pattern::Constructor {
            qualifier: None,
            name: Ident {
                text: "Login".into(),
                span: ctor_span,
            },
            fields: Some(vec![fp]),
            has_rest: true,
            args: vec![],
            span: sp_at(0, 20),
        };

        let ir_pat = lower_pattern_full(&mut ctx, &ctor_pat);

        // Outer: UnionVariant tag with exactly one positional arg (the record map).
        match ir_pat {
            IrPat::Ctor {
                sym, fields, args, ..
            } => {
                match sym {
                    SymbolRef::Constructor {
                        ctor_kind: CtorKind::UnionVariant,
                        name,
                        variant,
                        ..
                    } => {
                        assert_eq!(name, "Login");
                        assert_eq!(variant, 1, "variant index preserved");
                    }
                    other => panic!("expected outer UnionVariant sym, got {other:?}"),
                }
                assert!(fields.is_empty(), "outer tag carries no named fields");
                assert_eq!(args.len(), 1, "outer tag carries the record map as one arg");

                // Inner: Record (map) pattern carrying the named field.
                match &args[0] {
                    IrPat::Ctor {
                        sym: inner_sym,
                        fields: inner_fields,
                        args: inner_args,
                        ..
                    } => {
                        assert!(matches!(
                            inner_sym,
                            SymbolRef::Constructor {
                                ctor_kind: CtorKind::Record,
                                ..
                            }
                        ));
                        assert!(inner_args.is_empty(), "inner record: no positional args");
                        assert_eq!(inner_fields.len(), 1, "inner record: one named field");
                        assert_eq!(inner_fields[0].0, "userId");
                    }
                    other => panic!("expected inner Record ctor pattern, got {other:?}"),
                }
            }
            other => panic!("expected IrPat::Ctor, got {other:?}"),
        }
    }
}
