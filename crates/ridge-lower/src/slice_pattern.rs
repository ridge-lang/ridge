//! Structural slice patterns and deferred bindings for match arms.
#![allow(clippy::cast_possible_wrap)]
use crate::{ctx::LowerCtx, error::LowerError, match_lower::lower_pattern_full};
use ridge_ast::{pattern::ListPatElem, visit::Visit, Expr, Ident, Pattern, Span};
use ridge_ir::{IrArm, IrExpr, IrLit, IrPat, StdlibKind, SymbolRef};

/// A binding computed only after every structural and length check succeeds.
#[derive(Clone)]
pub struct Extraction {
    pattern: Pattern,
    value: IrExpr,
    span: Span,
}

/// Lower a suffix/middle-rest list without losing its structural prefix.
pub fn lower(ctx: &mut LowerCtx<'_>, elements: &[ListPatElem], span: Span) -> IrPat {
    let Some(rest) = elements
        .iter()
        .position(|e| matches!(e, ListPatElem::Rest { .. }))
    else {
        ctx.errors.push(LowerError::InternalLoweringError {
            message: "slice pattern without rest".into(),
            span,
        });
        return IrPat::Wild { span };
    };
    let suffix = elements.len() - rest - 1;
    let name = format!("_slice_scrut_{}", ctx.fresh_id(None).0);
    let subject = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: name.clone(),
        span,
    };
    let mut prefix = IrPat::Wild { span };
    for element in elements[..rest].iter().rev() {
        if let ListPatElem::Elem(pattern) = element {
            prefix = IrPat::Cons {
                head: Box::new(lower_pattern_full(ctx, pattern)),
                tail: Box::new(prefix),
                span,
            };
        }
    }
    let length_guard = build_length_ge_guard(ctx, &subject, rest + suffix, span);
    ctx.pending_pattern_guards.push(length_guard);

    // Retain the suffix tail once, shared by all its bindings.
    let tail_name = format!("_slice_tail_{}", ctx.fresh_id(None).0);
    let tail = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: tail_name.clone(),
        span,
    };
    let mut suffix_bindings = Vec::new();
    for (index, element) in elements[rest + 1..].iter().enumerate() {
        if let ListPatElem::Elem(pattern) = element {
            if is_refutable(pattern) {
                ctx.errors.push(LowerError::RefutableSliceElement {
                    span: pattern.span(),
                });
            } else if !matches!(pattern, Pattern::Wildcard { .. }) {
                suffix_bindings.push(Extraction {
                    pattern: pattern.clone(),
                    value: build_nth_call(ctx, index + 1, &tail, span),
                    span,
                });
            }
        }
    }
    if !suffix_bindings.is_empty() {
        let value = build_nthtail_call(ctx, &subject, suffix, span);
        ctx.pending_slice_extractions.push(Extraction {
            pattern: variable(tail_name, span),
            value,
            span,
        });
        ctx.pending_slice_extractions.extend(suffix_bindings);
    }
    if let ListPatElem::Rest {
        bind: Some(bind),
        span: bind_span,
    } = &elements[rest]
    {
        let value = build_sublist_call(ctx, &subject, rest, suffix, span);
        ctx.pending_slice_extractions.push(Extraction {
            pattern: Pattern::Var {
                name: bind.clone(),
                span: *bind_span,
            },
            value,
            span,
        });
    }
    IrPat::Bind {
        name,
        inner: Some(Box::new(prefix)),
        span,
    }
}

const fn variable(text: String, span: Span) -> Pattern {
    Pattern::Var {
        name: Ident { text, span },
        span,
    }
}

/// Wrap the deferred bindings around an arm body or its user guard.
pub fn bind(ctx: &mut LowerCtx<'_>, bindings: Vec<Extraction>, body: IrExpr) -> IrExpr {
    bindings.into_iter().rev().fold(body, |body, extraction| {
        wrap_irrefutable_binding(
            ctx,
            &extraction.pattern,
            extraction.value,
            body,
            extraction.span,
        )
    })
}

/// Unsafe list extraction and the user guard run only after pattern checks.
pub fn guarded(
    ctx: &mut LowerCtx<'_>,
    pattern: Option<IrExpr>,
    user: Option<IrExpr>,
    user_source: Option<&Expr>,
    bindings: &[Extraction],
    span: Span,
) -> Option<IrExpr> {
    let needs_bindings = user_source.is_some_and(|source| uses_extracted_binding(source, bindings));
    let user = user.map(|guard| {
        if needs_bindings {
            bind(ctx, bindings.to_vec(), guard)
        } else {
            guard
        }
    });
    match (pattern, user) {
        (Some(pattern), Some(user)) => Some(IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(pattern),
            arms: vec![
                IrArm {
                    pat: IrPat::Lit {
                        value: IrLit::Bool(true),
                        span,
                    },
                    when: None,
                    body: user,
                    span,
                },
                IrArm {
                    pat: IrPat::Wild { span },
                    when: None,
                    body: IrExpr::Lit {
                        id: ctx.fresh_id(None),
                        value: IrLit::Bool(false),
                        span,
                    },
                    span,
                },
            ],
            span,
        }),
        (pattern, user) => pattern.or(user),
    }
}

// A conservative name scan avoids list extraction for guards that cannot use
// its result. Shadowed names may retain extra bindings, but cannot remove a
// required one; actual binding and shadowing still use ordinary lexical scopes.
fn uses_extracted_binding(source: &Expr, bindings: &[Extraction]) -> bool {
    struct Uses<'a> {
        names: &'a std::collections::BTreeSet<String>,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Uses<'_> {
        fn visit_ident(&mut self, ident: &'ast Ident) {
            self.found |= self.names.contains(&ident.text);
        }
    }
    let names = bindings
        .iter()
        .flat_map(|binding| binding.pattern.bound_var_names())
        .collect::<std::collections::BTreeSet<_>>();
    let mut usage = Uses {
        names: &names,
        found: false,
    };
    usage.visit_expr(source);
    usage.found
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
                kind: StdlibKind::Function,
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
                kind: StdlibKind::Function,
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
                kind: StdlibKind::Function,
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
                kind: StdlibKind::Function,
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
                kind: StdlibKind::Function,
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
                kind: StdlibKind::Function,
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
                kind: StdlibKind::Function,
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
                kind: StdlibKind::Function,
            },
            span,
        }),
        args: vec![scrut_ref.clone()],
        span,
    }
}
