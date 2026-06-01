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
//! **Shorthand field expansion.** `FieldPattern { name, pattern: None }` expands
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
// Generated IR constants use usize→i64 casts for small loop indices/sizes.
// These values are bounded by list pattern lengths so the cast is safe.
#![allow(clippy::cast_possible_wrap)]

use ridge_ast::{pattern::FieldPattern, pattern::ListPatElem, Expr, Ident, Pattern, Span};
use ridge_ir::symbol::CtorKind;
use ridge_ir::{IrArm, IrExpr, IrLit, IrPat, SymbolRef};
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
///
/// When any arm has a variable-length list pattern (suffix or middle rest —
/// `[.., z]`, `[a, .., z]`, `[a, mid @ .., z]`), the scrutinee is bound to
/// a fresh local so that guards and body-extraction expressions can reference
/// it by name.
pub fn lower_match(
    ctx: &mut LowerCtx<'_>,
    scrutinee: &Expr,
    arms: &[ridge_ast::expr::MatchArm],
    span: Span,
) -> IrExpr {
    // Detect whether any arm has a variable-length list pattern.
    let has_varlen = arms.iter().any(|arm| arm.pattern.is_varlen_list());

    if has_varlen {
        lower_match_with_varlen(ctx, scrutinee, arms, span)
    } else {
        lower_match_simple(ctx, scrutinee, arms, span)
    }
}

/// Simple match lowering — no variable-length list patterns.
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

/// Match lowering when at least one arm has a variable-length list pattern.
///
/// Binds the scrutinee to a fresh local `_slice_scrut_N` so that guards and
/// body-extraction expressions can reference it without re-evaluating it.
fn lower_match_with_varlen(
    ctx: &mut LowerCtx<'_>,
    scrutinee: &Expr,
    arms: &[ridge_ast::expr::MatchArm],
    span: Span,
) -> IrExpr {
    // The scrutinee is evaluated once as the `Match` subject (like an Erlang
    // `case` subject). A var-len arm binds the whole subject in its own arm
    // pattern (`case Xs of S when length(S) >= n -> ...`) so it can reference
    // it from the length guard and the extraction chain. We deliberately do
    // NOT wrap the match in an outer `LetIn`: a match can sit mid-block (with
    // statements after it), and `LetIn` must be continuation-form — wrapping
    // here would place a `LetIn` as a non-final block statement and violate the
    // Phase 5 IR invariant.
    let scrutinee_ir = lower_expr(ctx, scrutinee);

    let match_id = ctx.fresh_id(None);
    let arms_ir: Vec<IrArm> = arms
        .iter()
        .map(|arm| {
            if arm.pattern.is_varlen_list() {
                lower_varlen_list_arm(ctx, arm, span)
            } else {
                let pat = lower_pattern_full(ctx, &arm.pattern);
                let when = arm.guard.as_ref().map(|g| lower_expr(ctx, g));
                let body = lower_expr(ctx, &arm.body);
                IrArm {
                    pat,
                    when,
                    body,
                    span: arm.span,
                }
            }
        })
        .collect();

    IrExpr::Match {
        id: match_id,
        scrutinee: Box::new(scrutinee_ir),
        arms: arms_ir,
        span,
    }
}

/// Lower a single variable-length list arm.
///
/// `[p0, .., s0]` where rest is at position `rest_pos`:
/// - Prefix elements: `p0 .. p_{rest_pos-1}` (before the rest).
/// - Suffix elements: `s0 .. s_{suffix_count-1}` (after the rest).
/// - Middle bind: optional name from `Rest { bind: Some(name) }`.
///
/// Emits:
/// - `IrPat::Wild` as the arm pattern (length check is in the guard).
/// - Guard: `erlang:length(scrut) >= prefix_count + suffix_count`.
/// - Body: a let-chain extracting prefix/middle/suffix bindings, then the
///   original body.
///
/// Refutable sub-patterns in suffix/middle positions are rejected with a
/// `LowerError` (P026); the emitted body contains a wildcard in their place.
fn lower_varlen_list_arm(
    ctx: &mut LowerCtx<'_>,
    arm: &ridge_ast::expr::MatchArm,
    default_span: Span,
) -> IrArm {
    let Pattern::List {
        ref elements,
        span: pat_span,
    } = arm.pattern
    else {
        // Should not happen — caller verified is_varlen_list.
        return IrArm {
            pat: IrPat::Wild { span: default_span },
            when: None,
            body: lower_expr(ctx, &arm.body),
            span: arm.span,
        };
    };

    // Find the rest element position.
    let Some(rest_pos) = elements
        .iter()
        .position(|e| matches!(e, ListPatElem::Rest { .. }))
    else {
        // No rest element — shouldn't happen, but fall back to simple lowering.
        let pat = lower_pattern_full(ctx, &arm.pattern);
        let when = arm.guard.as_ref().map(|g| lower_expr(ctx, g));
        let body = lower_expr(ctx, &arm.body);
        return IrArm {
            pat,
            when,
            body,
            span: arm.span,
        };
    };

    let prefix_count = rest_pos;
    let suffix_count = elements.len() - rest_pos - 1;
    let min_len = prefix_count + suffix_count;

    // Extract the middle bind name (from `mid @ ..`).
    let mid_bind: Option<String> = match &elements[rest_pos] {
        ListPatElem::Rest {
            bind: Some(ident), ..
        } => Some(ident.text.clone()),
        _ => None,
    };

    // Bind the whole scrutinee in this arm's pattern so the length guard and
    // the extraction chain can reference it: `case ... of S when length(S) >= n`.
    let scrut_bind_id = ctx.fresh_id(None);
    let scrut_name = format!("_slice_scrut_{}", scrut_bind_id.0);
    let scrut_ref = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: scrut_name.clone(),
        span: pat_span,
    };
    let scrut_ref = &scrut_ref;

    // Build guard: erlang:length(scrut) >= min_len
    let guard_expr = build_length_ge_guard(ctx, scrut_ref, min_len, pat_span);

    // Merge with an existing user guard if present.
    let combined_guard = if let Some(user_guard) = arm.guard.as_ref() {
        let user_guard_ir = lower_expr(ctx, user_guard);
        // Combine: length_check AND user_guard via erlang:'and'/2 (guard BIF).
        let and_id = ctx.fresh_id(None);
        Some(IrExpr::Call {
            id: and_id,
            callee: Box::new(IrExpr::Symbol {
                id: ctx.fresh_id(None),
                sym: SymbolRef::Stdlib {
                    module: "__slice__".into(),
                    name: "and".into(),
                },
                span: pat_span,
            }),
            args: vec![guard_expr, user_guard_ir],
            span: pat_span,
        })
    } else {
        Some(guard_expr)
    };

    // Build body: let-chain of extractions, then the original body.
    let original_body = lower_expr(ctx, &arm.body);
    let body = build_extraction_chain(
        ctx,
        scrut_ref,
        elements,
        prefix_count,
        suffix_count,
        mid_bind,
        min_len,
        original_body,
        pat_span,
    );

    IrArm {
        pat: IrPat::Bind {
            name: scrut_name,
            inner: None,
            span: pat_span,
        },
        when: combined_guard,
        body,
        span: arm.span,
    }
}

/// Emit `erlang:length(scrut) >= min_len` as an `IrExpr`.
fn build_length_ge_guard(
    ctx: &mut LowerCtx<'_>,
    scrut_ref: &IrExpr,
    min_len: usize,
    span: Span,
) -> IrExpr {
    let len_call_id = ctx.fresh_id(None);
    let len_sym_id = ctx.fresh_id(None);
    let ge_call_id = ctx.fresh_id(None);
    let ge_sym_id = ctx.fresh_id(None);
    let lit_id = ctx.fresh_id(None);

    let length_call = IrExpr::Call {
        id: len_call_id,
        callee: Box::new(IrExpr::Symbol {
            id: len_sym_id,
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "length".into(),
            },
            span,
        }),
        args: vec![scrut_ref.clone()],
        span,
    };

    let min_len_lit = IrExpr::Lit {
        id: lit_id,
        value: IrLit::Int(min_len as i64),
        span,
    };

    IrExpr::Call {
        id: ge_call_id,
        callee: Box::new(IrExpr::Symbol {
            id: ge_sym_id,
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "ge".into(),
            },
            span,
        }),
        args: vec![length_call, min_len_lit],
        span,
    }
}

/// Determine whether a pattern is irrefutable (only binds values, never fails).
///
/// Irrefutable: `Wildcard`, `Var`, `As { inner: irrefutable }`, `Paren { inner: irrefutable }`.
/// Refutable: everything else (literals, constructors, cons, tuples, nested lists).
fn is_irrefutable(pat: &Pattern) -> bool {
    match pat {
        Pattern::Wildcard { .. } | Pattern::Var { .. } => true,
        Pattern::As { inner, .. } | Pattern::Paren { inner, .. } => is_irrefutable(inner),
        _ => false,
    }
}

/// Determine whether a pattern is refutable (may not match all values).
fn is_refutable(pat: &Pattern) -> bool {
    !is_irrefutable(pat)
}

/// Build the let-chain that extracts prefix, middle, and suffix bindings.
///
/// Returns the innermost `body` wrapped in the extraction let-chain.
#[allow(clippy::too_many_arguments)]
fn build_extraction_chain(
    ctx: &mut LowerCtx<'_>,
    scrut_ref: &IrExpr,
    elements: &[ListPatElem],
    prefix_count: usize,
    suffix_count: usize,
    mid_bind: Option<String>,
    _min_len: usize,
    body: IrExpr,
    span: Span,
) -> IrExpr {
    // We build the chain inside-out (suffix then middle then prefix) since we
    // fold right-to-left from the body outward.
    let mut result = body;

    // ── Suffix elements (right-to-left — innermost first, outermost last) ─────
    //
    // `[.., s0, s1]` with suffix_count=2:
    //   suffix_tail = lists:nthtail(length(scrut) - 2, scrut)
    //   s0 = lists:nth(1, suffix_tail)
    //   s1 = lists:nth(2, suffix_tail)
    //
    // The suffix_tail binding is computed once and shared.

    if suffix_count > 0 {
        let suffix_tail_name = format!("_stail_{}", ctx.fresh_id(None).0);

        // Wrap suffix element extractions inside the suffix_tail let-binding.
        // Process suffix elements right-to-left so the outermost let appears first
        // in the chain (innermost body is already `result`).
        //
        // elements[prefix_count + 1 ..] are the suffix elements (after the Rest).
        let suffix_ref = {
            let local_id = ctx.fresh_id(None);
            IrExpr::Local {
                id: local_id,
                name: suffix_tail_name.clone(),
                span,
            }
        };

        // Wrap each suffix element extraction.
        for i in (0..suffix_count).rev() {
            let elem_pat = &elements[prefix_count + 1 + i];
            if let ListPatElem::Elem(pat) = elem_pat {
                if is_refutable(pat) {
                    // Emit error but continue — bind to wildcard.
                    ctx.errors
                        .push(LowerError::RefutableSliceElement { span: pat.span() });
                    // Bind the extracted value to `_` (wildcard) — don't introduce
                    // a variable but still consume the extraction.
                    let nth_call = build_nth_call(ctx, i + 1, &suffix_ref, span);
                    let discard_id = ctx.fresh_id(None);
                    let discard_name = format!("_discard_{}", discard_id.0);
                    let bind_id = ctx.fresh_id(None);
                    result = IrExpr::LetIn {
                        id: bind_id,
                        pat: IrPat::Bind {
                            name: discard_name,
                            inner: None,
                            span,
                        },
                        value: Box::new(nth_call),
                        body: Box::new(result),
                        span,
                    };
                } else {
                    // Irrefutable: extract and bind.
                    let extracted = build_nth_call(ctx, i + 1, &suffix_ref, span);
                    result = wrap_irrefutable_binding(ctx, pat, extracted, result, span);
                }
            }
        }

        // Compute suffix_tail = lists:nthtail(length(scrut) - suffix_count, scrut).
        let suffix_tail_val = build_nthtail_call(ctx, scrut_ref, suffix_count, span);
        let tail_bind_id = ctx.fresh_id(None);
        result = IrExpr::LetIn {
            id: tail_bind_id,
            pat: IrPat::Bind {
                name: suffix_tail_name,
                inner: None,
                span,
            },
            value: Box::new(suffix_tail_val),
            body: Box::new(result),
            span,
        };
    }

    // ── Middle bind (if present) ──────────────────────────────────────────────
    // mid = lists:sublist(scrut, prefix_count + 1, length(scrut) - min_len)
    if let Some(name) = mid_bind {
        let mid_val = build_sublist_call(ctx, scrut_ref, prefix_count, suffix_count, span);
        let mid_bind_id = ctx.fresh_id(None);
        result = IrExpr::LetIn {
            id: mid_bind_id,
            pat: IrPat::Bind {
                name,
                inner: None,
                span,
            },
            value: Box::new(mid_val),
            body: Box::new(result),
            span,
        };
    }

    // ── Prefix elements (right-to-left — last prefix elem innermost) ──────────
    // For prefix elements we use hd/tl chains. We go right-to-left so prefix[0]
    // is the outermost let.
    for i in (0..prefix_count).rev() {
        let elem_pat = &elements[i];
        if let ListPatElem::Elem(pat) = elem_pat {
            // Extract: hd(tl^i(scrut))
            let extracted = build_hd_tl_chain(ctx, scrut_ref, i, span);
            result = wrap_irrefutable_binding(ctx, pat, extracted, result, span);
        }
    }

    result
}

/// Wrap an irrefutable pattern binding around `body`.
///
/// For `Wildcard` — no binding; return `body` directly.
/// For `Var { name }` — emit `LetIn { pat: Bind name, value, body }`.
/// For `As { name, inner }` — emit `LetIn { pat: Bind name, value, body }` then recurse.
fn wrap_irrefutable_binding(
    ctx: &mut LowerCtx<'_>,
    pat: &Pattern,
    value: IrExpr,
    body: IrExpr,
    span: Span,
) -> IrExpr {
    match pat {
        Pattern::Wildcard { .. } => {
            // Discard — no binding needed; evaluate value for side-effects only.
            // Since list extractions are pure, we can skip the let entirely.
            body
        }
        Pattern::Var {
            name,
            span: var_span,
        } => {
            let bind_id = ctx.fresh_id(None);
            IrExpr::LetIn {
                id: bind_id,
                pat: IrPat::Bind {
                    name: name.text.clone(),
                    inner: None,
                    span: *var_span,
                },
                value: Box::new(value),
                body: Box::new(body),
                span,
            }
        }
        Pattern::As { name, inner, .. } => {
            // Bind the whole value to `name`, then process `inner` with the same value.
            // We need to re-use the value — bind it twice. Use a temp local.
            let temp_id = ctx.fresh_id(None);
            let temp_name = format!("_astmp_{}", temp_id.0);
            let temp_ref = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: temp_name.clone(),
                span,
            };
            // Inner binding with the temp ref.
            let inner_bound = wrap_irrefutable_binding(ctx, inner, temp_ref.clone(), body, span);
            // Outer binding: name = temp.
            let name_bind_id = ctx.fresh_id(None);
            let name_bound = IrExpr::LetIn {
                id: name_bind_id,
                pat: IrPat::Bind {
                    name: name.text.clone(),
                    inner: None,
                    span,
                },
                value: Box::new(temp_ref),
                body: Box::new(inner_bound),
                span,
            };
            // Outermost: temp = value.
            let temp_bind_id = ctx.fresh_id(None);
            IrExpr::LetIn {
                id: temp_bind_id,
                pat: IrPat::Bind {
                    name: temp_name,
                    inner: None,
                    span,
                },
                value: Box::new(value),
                body: Box::new(name_bound),
                span,
            }
        }
        Pattern::Paren { inner, .. } => wrap_irrefutable_binding(ctx, inner, value, body, span),
        // Refutable patterns: already guarded by the caller; should not reach here.
        _ => body,
    }
}

/// Build `hd(tl^n(scrut))` to extract the element at depth `n` (0-indexed).
fn build_hd_tl_chain(
    ctx: &mut LowerCtx<'_>,
    scrut_ref: &IrExpr,
    depth: usize,
    span: Span,
) -> IrExpr {
    // Build tl^depth(scrut) first.
    let mut current = scrut_ref.clone();
    for _ in 0..depth {
        let tl_id = ctx.fresh_id(None);
        let tl_sym_id = ctx.fresh_id(None);
        current = IrExpr::Call {
            id: tl_id,
            callee: Box::new(IrExpr::Symbol {
                id: tl_sym_id,
                sym: SymbolRef::Stdlib {
                    module: "__slice__".into(),
                    name: "tl".into(),
                },
                span,
            }),
            args: vec![current],
            span,
        };
    }
    // Then hd(current).
    let hd_id = ctx.fresh_id(None);
    let hd_sym_id = ctx.fresh_id(None);
    IrExpr::Call {
        id: hd_id,
        callee: Box::new(IrExpr::Symbol {
            id: hd_sym_id,
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "hd".into(),
            },
            span,
        }),
        args: vec![current],
        span,
    }
}

/// Build `lists:nth(n, suffix_ref)` where `n` is 1-indexed.
fn build_nth_call(ctx: &mut LowerCtx<'_>, n: usize, suffix_ref: &IrExpr, span: Span) -> IrExpr {
    let nth_id = ctx.fresh_id(None);
    let nth_sym_id = ctx.fresh_id(None);
    let n_lit_id = ctx.fresh_id(None);
    IrExpr::Call {
        id: nth_id,
        callee: Box::new(IrExpr::Symbol {
            id: nth_sym_id,
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "nth".into(),
            },
            span,
        }),
        args: vec![
            IrExpr::Lit {
                id: n_lit_id,
                value: IrLit::Int(n as i64),
                span,
            },
            suffix_ref.clone(),
        ],
        span,
    }
}

/// Build `lists:nthtail(length(scrut) - suffix_count, scrut)`.
fn build_nthtail_call(
    ctx: &mut LowerCtx<'_>,
    scrut_ref: &IrExpr,
    suffix_count: usize,
    span: Span,
) -> IrExpr {
    // Compute length(scrut) - suffix_count.
    let len_call = build_length_call(ctx, scrut_ref, span);
    let sub_id = ctx.fresh_id(None);
    let sub_sym_id = ctx.fresh_id(None);
    let sc_lit_id = ctx.fresh_id(None);
    let offset = IrExpr::Call {
        id: sub_id,
        callee: Box::new(IrExpr::Symbol {
            id: sub_sym_id,
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "minus".into(),
            },
            span,
        }),
        args: vec![
            len_call,
            IrExpr::Lit {
                id: sc_lit_id,
                value: IrLit::Int(suffix_count as i64),
                span,
            },
        ],
        span,
    };

    let nthtail_id = ctx.fresh_id(None);
    let nthtail_sym_id = ctx.fresh_id(None);
    IrExpr::Call {
        id: nthtail_id,
        callee: Box::new(IrExpr::Symbol {
            id: nthtail_sym_id,
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "nthtail".into(),
            },
            span,
        }),
        args: vec![offset, scrut_ref.clone()],
        span,
    }
}

/// Build `lists:sublist(scrut, prefix_count + 1, length(scrut) - min_len)`.
fn build_sublist_call(
    ctx: &mut LowerCtx<'_>,
    scrut_ref: &IrExpr,
    prefix_count: usize,
    suffix_count: usize,
    span: Span,
) -> IrExpr {
    let len_call = build_length_call(ctx, scrut_ref, span);
    let required_len = prefix_count + suffix_count;
    let sub_id = ctx.fresh_id(None);
    let sub_sym_id = ctx.fresh_id(None);
    let ml_lit_id = ctx.fresh_id(None);
    let mid_len = IrExpr::Call {
        id: sub_id,
        callee: Box::new(IrExpr::Symbol {
            id: sub_sym_id,
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "minus".into(),
            },
            span,
        }),
        args: vec![
            len_call,
            IrExpr::Lit {
                id: ml_lit_id,
                value: IrLit::Int(required_len as i64),
                span,
            },
        ],
        span,
    };

    let start_lit_id = ctx.fresh_id(None);
    let sublist_id = ctx.fresh_id(None);
    let sublist_sym_id = ctx.fresh_id(None);
    IrExpr::Call {
        id: sublist_id,
        callee: Box::new(IrExpr::Symbol {
            id: sublist_sym_id,
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "sublist".into(),
            },
            span,
        }),
        args: vec![
            scrut_ref.clone(),
            IrExpr::Lit {
                id: start_lit_id,
                value: IrLit::Int((prefix_count + 1) as i64),
                span,
            },
            mid_len,
        ],
        span,
    }
}

/// Build `erlang:length(scrut)`.
fn build_length_call(ctx: &mut LowerCtx<'_>, scrut_ref: &IrExpr, span: Span) -> IrExpr {
    let len_id = ctx.fresh_id(None);
    let len_sym_id = ctx.fresh_id(None);
    IrExpr::Call {
        id: len_id,
        callee: Box::new(IrExpr::Symbol {
            id: len_sym_id,
            sym: SymbolRef::Stdlib {
                module: "__slice__".into(),
                name: "length".into(),
            },
            span,
        }),
        args: vec![scrut_ref.clone()],
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
        // (unit-test scaffolding), we fall back to `TyConId(0)` — the IR is still
        // structurally correct even if the id is wrong.
        Pattern::Record { fields, span, .. } => {
            let anon_id: TyConId = ctx
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
                })
                .unwrap_or(TyConId(0));

            let anon_name = ctx
                .workspace
                .and_then(|ws| ws.tycons.get(anon_id.0 as usize))
                .map_or_else(
                    || format!("{{anon record #{}}}", anon_id.0),
                    |d| d.name.clone(),
                );

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

        // ── Bracketed list pattern `[a, b, ..]` ──────────────────────────────
        // Desugar to Cons/ListNil/Wildcard/Bind and lower the result.
        Pattern::List { .. } => lower_pattern_full(ctx, &pat.clone().desugar_list()),

        // ── Constructor pattern ───────────────────────────────────────────────
        Pattern::Constructor {
            name,
            fields,
            args,
            span,
            ..
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
            // Only the known prelude constructors map to IrPat::Ctor.
            // For anything else fall through to the error arm below.
            match prelude_name.as_str() {
                "Some" | "None" | "Ok" | "Err" | "JNull" | "JBool" | "JInt" | "JFloat"
                | "JText" | "JList" | "JObject" => {
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
        // Raw strings carry literal bytes; no escape decoding.
        Literal::RawText { raw, .. } => ridge_ir::IrLit::Text(raw.clone()),
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

    // A refutable element in a slice suffix/middle position must be rejected
    // with L009 (the 0.2.8 restriction): `[.., 0]` has a literal in the suffix.
    #[test]
    fn lower_match_refutable_slice_suffix_emits_l009() {
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
            "expected L009 RefutableSliceElement for a literal in suffix position, got: {:?}",
            ctx.errors
        );
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
            has_rest: false,
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

    // ── Record shorthand field expansion ──────────────────────────────────────
    //
    // arm `User { name } -> name`: shorthand expands to
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
}
