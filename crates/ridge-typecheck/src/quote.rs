//! Quotation type-checking — the isolated checker for a quoted predicate.
//!
//! When a lambda is passed where a `Quote (Entity -> Bool)` is expected, the
//! body is not checked as an ordinary function. It is checked here, against the
//! entity's columns, by a small dedicated walk that never touches the core
//! operator typing (`infer_binary`) or relaxes any language rule. The quoted
//! sub-language is deliberately narrow: column references, literals, the six
//! comparisons, and `&&`/`||`.
//!
//! A quote can range over more than one entity. A single-parameter quote is the
//! common case (`fn (u: User) -> u.age >= 18`); a join condition or projection
//! takes two (`fn (u: User) (p: Post) -> u.id == p.authorId`), and every column
//! reference resolves against whichever parameter's record it names.
//!
//! On success the checker records a [`QuoteInfo`] keyed by the lambda's span so
//! the lowering pass knows to reify the body into a `QExpr` tree rather than
//! lower it to a closure.

use ridge_ast::{BinOp, Expr, LambdaParam, Literal, Pattern, Span};
use ridge_types::{BuiltinTyCons, TyConId, TyConKind, Type};

use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::instantiate::instantiate;

/// What the lowering pass needs to reify a quoted lambda body.
#[derive(Debug, Clone)]
pub struct QuoteInfo {
    /// The lambda's first parameter name (the row bound to the entity, or the
    /// group handle for a grouped-aggregate quote).
    pub param_name: String,
    /// The entity type the first parameter is checked against.
    pub entity: TyConId,
    /// True for a grouped-aggregate quote (`having`/`summarize`): its body is
    /// reified over the group vocabulary (`g.key`, `g.count`, `g.sum(col)`, …)
    /// rather than the row columns.
    pub group: bool,
}

/// One parameter of a quote: the bound name and the record it ranges over.
struct Param {
    /// The lambda parameter's name (the base of a `name.column` access).
    name: String,
    /// The entity type the parameter ranges over.
    entity: TyConId,
    /// The entity's display name, for diagnostics.
    entity_name: String,
    /// The entity's `(column, type)` fields.
    fields: Vec<(String, Type)>,
    /// True when the parameter is `Option e` — the nullable right side of a
    /// left-join projection. Its columns read as `Option` of their declared
    /// type, so an unmatched row's column decodes to `None`.
    nullable: bool,
}

/// The kind a quoted sub-expression evaluates to during checking.
#[derive(Clone)]
enum QKind {
    /// A column reference, carrying the column's value type.
    Col(Type),
    /// A literal or computed scalar, carrying its type.
    Scalar(Type),
    /// A boolean predicate (the result of a comparison or `&&`/`||`).
    Pred,
}

/// Returns `true` when `param_ty` (already resolved) is a `Quote _` type.
pub(crate) fn is_quote_param(ctx: &InferCtx, param_ty: &Type) -> bool {
    matches!(param_ty, Type::Con(id, args) if args.len() == 1 && is_quote_tycon(ctx, *id))
}

/// The number of parameters of the function inside a `Quote (a -> … -> r)`.
///
/// `1` for a predicate or ordering key, `2` for a join condition or projection.
/// `None` when the type is not a `Quote` wrapping a function type.
pub(crate) fn quote_arity(ctx: &mut InferCtx, param_ty: &Type) -> Option<usize> {
    let Type::Con(id, args) = param_ty else {
        return None;
    };
    if !is_quote_tycon(ctx, *id) {
        return None;
    }
    let inner = ctx.deep_resolve(args.first()?);
    let Type::Fn { params, .. } = inner else {
        return None;
    };
    Some(params.len())
}

/// Extracts the concrete entity `TyConId` from the `i`th parameter slot of a
/// `Quote (a -> … -> r)` type.
///
/// Returns `None` when the type is not a `Quote`, when its inner shape is not a
/// function with an `i`th parameter, or when that parameter is not a concrete
/// type constructor (e.g. still an inference variable).
pub(crate) fn quote_entity_at(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    param_ty: &Type,
    i: usize,
) -> Option<TyConId> {
    let Type::Con(id, args) = param_ty else {
        return None;
    };
    if !is_quote_tycon(ctx, *id) {
        return None;
    }
    let inner = ctx.deep_resolve(args.first()?);
    let Type::Fn { params, .. } = inner else {
        return None;
    };
    match ctx.deep_resolve(params.get(i)?) {
        // An `Option e` slot — the nullable right side of a left-join projection
        // (`fn (u: User) (p: Option Post) -> …`) — names the entity `e`, whose
        // columns are read as `Option` of their type. Unwrap to that entity.
        Type::Con(opt, opt_args) if opt == b.option => match ctx.deep_resolve(opt_args.first()?) {
            Type::Con(entity, _) => Some(entity),
            _ => None,
        },
        Type::Con(entity, _) => Some(entity),
        _ => None,
    }
}

/// Whether the `i`th parameter slot of a `Quote (a -> … -> r)` is an `Option e`.
///
/// The nullable right side of a left-join projection is written `p: Option Post`,
/// and each of its columns reads as `Option` of the declared type so an unmatched
/// row's column decodes to `None`. Returns `false` for an ordinary entity slot.
pub(crate) fn quote_slot_nullable(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    param_ty: &Type,
    i: usize,
) -> bool {
    let Type::Con(id, args) = param_ty else {
        return false;
    };
    if !is_quote_tycon(ctx, *id) {
        return false;
    }
    let Some(inner) = args.first().map(|a| ctx.deep_resolve(a)) else {
        return false;
    };
    let Type::Fn { params, .. } = inner else {
        return false;
    };
    let Some(slot) = params.get(i).map(|p| ctx.deep_resolve(p)) else {
        return false;
    };
    matches!(slot, Type::Con(opt, _) if opt == b.option)
}

/// Extracts the resolved result type `r` from a `Quote (a -> … -> r)` type.
///
/// Returns `None` when the type is not a `Quote` wrapping a function type. The
/// result type selects the accepted body shape in [`check_quote`]: `Bool` is a
/// predicate (a `where` body); a scalar column type is a single column (an
/// `orderBy` key); a record is a projection (a `select` list).
pub(crate) fn quote_result(ctx: &mut InferCtx, param_ty: &Type) -> Option<Type> {
    let Type::Con(id, args) = param_ty else {
        return None;
    };
    if !is_quote_tycon(ctx, *id) {
        return None;
    }
    let inner = ctx.deep_resolve(args.first()?);
    let Type::Fn { ret, .. } = inner else {
        return None;
    };
    Some(ctx.deep_resolve(&ret))
}

/// Checks a quoted lambda body against `entities` (one entity per lambda
/// parameter, in order). On success records a [`QuoteInfo`] for the lambda span
/// and returns `true`; on failure pushes a diagnostic and returns `false`.
#[expect(
    clippy::too_many_lines,
    reason = "one linear walk over the quote body shapes (named/anonymous projection, predicate, ordering key); splitting it would scatter the shared setup"
)]
pub(crate) fn check_quote(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    lambda: &Expr,
    entities: &[(TyConId, bool)],
    expected_ret: Option<&Type>,
) -> bool {
    let Expr::Lambda { params, body, span } = lambda else {
        return false;
    };

    if params.len() != entities.len() {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: format!(
                "a quoted function over {} parameter(s) was written with {}",
                entities.len(),
                params.len()
            ),
            span: *span,
        });
        return false;
    }

    // Build the parameter scope: pair each lambda parameter's name with the
    // entity it ranges over and that entity's columns. A column access resolves
    // against whichever parameter's record names it. An `Option e` slot marks the
    // parameter nullable so its columns read as `Option` of their type.
    let mut scope: Vec<Param> = Vec::with_capacity(params.len());
    for (lp, &(entity, nullable)) in params.iter().zip(entities) {
        let name = match lp {
            LambdaParam::Pattern(Pattern::Var { name, .. })
            | LambdaParam::Annotated {
                pat: Pattern::Var { name, .. },
                ..
            } => name.text.clone(),
            _ => {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: "a quoted parameter must be a plain name".to_string(),
                    span: *span,
                });
                return false;
            }
        };
        let entity_name = ctx
            .tycon_decls
            .get(entity.0 as usize)
            .map_or_else(|| "?".to_string(), |d| d.name.clone());
        let Some(fields) = entity_fields(ctx, entity) else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "`{entity_name}` is not a record type, so it has no columns to quote"
                ),
                span: *span,
            });
            return false;
        };
        scope.push(Param {
            name,
            entity,
            entity_name,
            fields,
            nullable,
        });
    }

    // The quote's result type selects the accepted body shape: a `Bool` result
    // is a predicate (a `where`/join body); a record result is a projection (a
    // `select` list); any other scalar result is a single column (an `orderBy`
    // key). With no result type known, fall back to predicate.
    let want = expected_ret.map(|r| ctx.deep_resolve(r));

    // Named-constructor projection: a body like `Summary { name = u.name, … }`
    // names the result record directly through Ridge's record-construction
    // syntax. That makes the projection target concrete even when the quote's
    // declared result type is still an inference variable — which is the usual
    // case at a generic `selectList`/`selectJoin` call, where the result `s` is
    // only pinned by the binding's type *after* the argument is checked. Resolve
    // the constructor to its record type, check the projection against that
    // record's fields, and pin the quote's result to it so `List s` becomes the
    // named record and its `Row s` decode resolves.
    let mut named: &Expr = body;
    while let Expr::Paren { inner, .. } = named {
        named = inner;
    }
    if let Expr::Record { constructor, .. } = named {
        let ctor_name = match constructor {
            ridge_ast::RecordCtor::Bare(id) => id.text.clone(),
            ridge_ast::RecordCtor::Qualified(qn) => qn
                .segments
                .last()
                .map_or_else(String::new, |s| s.text.clone()),
        };
        let target = ctx.user_tycon_names.get(ctor_name.as_str()).copied();
        let Some(target_id) = target else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "a quoted projection names `{ctor_name}`, which is not a record type"
                ),
                span: named.span(),
            });
            return false;
        };
        let Some(target_fields) = entity_fields(ctx, target_id) else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!("a quoted projection names `{ctor_name}`, which is not a record"),
                span: named.span(),
            });
            return false;
        };
        if !check_projection(ctx, b, named, &scope, &target_fields, *span) {
            return false;
        }
        // Pin the quote's result to the named record, filling the record's
        // type parameters (if any) with fresh variables.
        if let Some(want_ty) = want.as_ref() {
            let arity = ctx
                .tycon_decls
                .get(target_id.0 as usize)
                .map_or(0, |d| d.arity);
            let args = (0..arity).map(|_| Type::Var(ctx.fresh_tyvid())).collect();
            let _ = crate::unify::unify(ctx, want_ty, &Type::Con(target_id, args));
        }
        return true;
    }

    // Projection: a record result is a select-list of columns.
    if let Some(proj) = want.as_ref().and_then(|t| record_fields_of(ctx, t)) {
        return check_projection(ctx, b, body, &scope, &proj, *span);
    }

    // An anonymous record body whose result type is not a known record (the usual
    // case at a generic `selectList`, where `s` is still unbound) cannot pin a
    // decode target. Point the caller at the named form rather than failing later
    // with an opaque "unsupported expression".
    if matches!(named, Expr::RecordLit { .. }) {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a quoted projection must name its result record, e.g. \
                     `Summary { name = row.name }`"
                .to_string(),
            span: named.span(),
        });
        return false;
    }

    let Some(qk) = check_node(ctx, b, body, &scope) else {
        return false;
    };

    // `want` selects the accepted body shape. A concrete `Bool` (or no result
    // type at all) is a predicate; a concrete non-`Bool` scalar is an ordering
    // key. An *unbound* result variable — the shape a class-method quote arrives
    // with, since `q -> p` only fixes it in the solver after the argument is
    // checked — is decided from the body itself: a boolean body is a predicate,
    // anything else an ordering key. The result variable is then pinned to the
    // inferred type so the fundep resolves the matching instance (`fn e -> Bool`
    // for `Refinable`, the key/column type otherwise).
    let body_is_predicate = as_predicate(b, &qk);
    let want_is_unbound = matches!(want.as_ref(), Some(Type::Var(_)));
    let is_bool_result = match want.as_ref() {
        None => true,
        Some(Type::Con(id, _)) if *id == b.bool => true,
        Some(Type::Var(_)) => body_is_predicate,
        Some(_) => false,
    };

    if is_bool_result {
        if body_is_predicate {
            if want_is_unbound {
                if let Some(w) = want.as_ref() {
                    let _ = crate::unify::unify(ctx, w, &Type::Con(b.bool, vec![]));
                }
            }
            ctx.quoted_lambdas_accum.insert(*span, quote_info(&scope));
            return true;
        }
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a quoted predicate must be a boolean expression".to_string(),
            span: body.span(),
        });
        return false;
    }

    // A non-boolean scalar result is an ordering key or an aggregate accessor. A
    // one-parameter quote is a query's `orderBy`/`sumOf`; a two-parameter quote a
    // join's, naming a value from either side (`fn u p -> p.title`). The value's
    // type must match the quote's declared result type, checked next.

    // The key may be a single column or a computed expression over the columns
    // (arithmetic, a CASE), each checked against the entity's schema — the value
    // and its type flow into the generated `ORDER BY` / `SUM(...)`, a literal in it
    // binding as a placeholder rather than interpolated, never as raw SQL. The type
    // must match the quote's declared result type — except when that result type is
    // an unbound variable (a polymorphic key, whose return is phantom), in which
    // case any value is accepted and the variable is bound to it.
    let want_ty = want.unwrap_or_else(|| Type::Con(b.bool, vec![]));
    let col_ty = match &qk {
        QKind::Col(t) | QKind::Scalar(t) => Some(ctx.deep_resolve(t)),
        QKind::Pred => None,
    };
    let accepts = match &col_ty {
        Some(vt) if matches!(want_ty, Type::Var(_)) => {
            let _ = crate::unify::unify(ctx, &want_ty, vt);
            true
        }
        Some(vt) => same_value_type(vt, &want_ty),
        None => false,
    };
    if accepts {
        ctx.quoted_lambdas_accum.insert(*span, quote_info(&scope));
        return true;
    }
    let want_rendered = crate::render::render_type_with(&want_ty, &ctx.tycon_decls);
    ctx.errors.push(TypeError::QuoteUnsupportedExpr {
        detail: format!(
            "a quoted ordering key must be a column or computed value of type {want_rendered}"
        ),
        span: body.span(),
    });
    false
}

// ── Internals ───────────────────────────────────────────────────────────────

fn is_quote_tycon(ctx: &InferCtx, id: TyConId) -> bool {
    ctx.tycon_decls
        .get(id.0 as usize)
        .is_some_and(|d| d.name == "Quote")
}

/// The `QuoteInfo` recorded for a checked quote — keyed off the first parameter.
/// The lowering pass reifies columns by parameter position from the lambda AST,
/// so only the span's presence in the accumulator is load-bearing here.
fn quote_info(scope: &[Param]) -> QuoteInfo {
    QuoteInfo {
        param_name: scope[0].name.clone(),
        entity: scope[0].entity,
        group: false,
    }
}

/// A comma-joined list of the scope's parameter names, for diagnostics.
fn scope_names(scope: &[Param]) -> String {
    scope
        .iter()
        .map(|p| format!("`{}`", p.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `(name, type)` columns of `entity`, if it is a record type.
fn entity_fields(ctx: &InferCtx, entity: TyConId) -> Option<Vec<(String, Type)>> {
    let decl = ctx.tycon_decls.get(entity.0 as usize)?;
    let TyConKind::Record(schema) = &decl.kind else {
        return None;
    };
    Some(
        schema
            .record_fields()
            .iter()
            .map(|f| (f.name.clone(), f.ty.clone()))
            .collect(),
    )
}

/// The `(name, type)` fields of `ty` when it is a record type, else `None`.
///
/// Selects the projection body shape in [`check_quote`]: a record result is a
/// select-list, so its declared fields drive the column-by-column check. An
/// inline annotation like `{ id: Int, name: Text }` is a structural
/// [`Type::Record`]; a named record type is a `Type::Con` over a record decl.
fn record_fields_of(ctx: &InferCtx, ty: &Type) -> Option<Vec<(String, Type)>> {
    match ty {
        Type::Record { fields, .. } => Some(
            fields
                .iter()
                .map(|(name, t)| (name.clone(), t.clone()))
                .collect(),
        ),
        Type::Con(id, _) => entity_fields(ctx, *id),
        _ => None,
    }
}

/// Checks a quoted projection body — a record literal whose every field is a
/// column of one of the scope's entities — against the declared projection
/// record `expected`.
///
/// Each field name is the output alias; its value must be a column of one of the
/// quote parameters. The body's field set and column types must match
/// `expected`, so the captured projection cannot disagree with the type the
/// caller declared for it.
fn check_projection(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    body: &Expr,
    scope: &[Param],
    expected: &[(String, Type)],
    span: Span,
) -> bool {
    let mut body = body;
    while let Expr::Paren { inner, .. } = body {
        body = inner;
    }
    // Both the anonymous `{ field = row.col }` and the named `Shape { field =
    // row.col }` forms project a record of columns; the constructor only names
    // the decode target, so the field check is identical for both.
    let (Expr::RecordLit { fields: inits, .. } | Expr::Record { fields: inits, .. }) = body else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a quoted projection must be a record of columns, like `{ id = row.id }`"
                .to_string(),
            span: body.span(),
        });
        return false;
    };
    if inits.is_empty() {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a quoted projection must select at least one column".to_string(),
            span: body.span(),
        });
        return false;
    }

    for fi in inits {
        let Some(value) = &fi.value else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{0}` must be written `{0} = row.column`",
                    fi.name.text
                ),
                span: fi.span,
            });
            return false;
        };
        let Some(qk) = check_node(ctx, b, value, scope) else {
            return false;
        };
        // A projection field is a column or a computed value (arithmetic, a
        // CASE, a literal) — anything carrying a value type. A bare predicate
        // (a raw comparison) is not a projectable value.
        let (QKind::Col(col_ty) | QKind::Scalar(col_ty)) = qk else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{}` must be a column or a computed value over one of {}",
                    fi.name.text,
                    scope_names(scope)
                ),
                span: fi.span,
            });
            return false;
        };
        let Some((_, exp_ty)) = expected.iter().find(|(n, _)| n == &fi.name.text) else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{}` is not declared in the result record",
                    fi.name.text
                ),
                span: fi.span,
            });
            return false;
        };
        let col_ty = ctx.deep_resolve(&col_ty);
        let exp_ty = ctx.deep_resolve(exp_ty);
        if !same_value_type(&col_ty, &exp_ty) {
            let left = crate::render::render_type_with(&col_ty, &ctx.tycon_decls);
            let right = crate::render::render_type_with(&exp_ty, &ctx.tycon_decls);
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{}` is of type {left}, but the result \
                     record declares {right}",
                    fi.name.text
                ),
                span: fi.span,
            });
            return false;
        }
    }

    // Every declared field must be projected — the captured select-list cannot
    // be narrower than the type the caller declared for it.
    for (n, _) in expected {
        if !inits.iter().any(|fi| &fi.name.text == n) {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!("a quoted projection is missing the declared column `{n}`"),
                span,
            });
            return false;
        }
    }

    ctx.quoted_lambdas_accum.insert(span, quote_info(scope));
    true
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear dispatch over the quoted-expression forms; splitting it would scatter the QKind mapping"
)]
fn check_node(ctx: &mut InferCtx, b: &BuiltinTyCons, e: &Expr, scope: &[Param]) -> Option<QKind> {
    match e {
        Expr::Paren { inner, .. } => check_node(ctx, b, inner, scope),

        Expr::Literal(lit) => Some(QKind::Scalar(literal_type(b, lit))),

        Expr::FieldAccess { base, field, span } => {
            let Expr::Ident(base_id) = base.as_ref() else {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: format!(
                        "only columns of the quote parameters ({}) can be accessed",
                        scope_names(scope)
                    ),
                    span: *span,
                });
                return None;
            };
            let Some(param) = scope.iter().find(|p| p.name == base_id.text) else {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: format!(
                        "only columns of the quote parameters ({}) can be accessed",
                        scope_names(scope)
                    ),
                    span: *span,
                });
                return None;
            };
            if let Some((_, ty)) = param.fields.iter().find(|(n, _)| n == &field.text) {
                // A column of a nullable (`Option e`) parameter reads as `Option`
                // of its declared type — an unmatched left-join row has no value.
                let col_ty = if param.nullable {
                    Type::Con(b.option, vec![ty.clone()])
                } else {
                    ty.clone()
                };
                Some(QKind::Col(col_ty))
            } else {
                let suggestions = ridge_resolve::suggest::suggest(
                    &field.text,
                    param.fields.iter().map(|(n, _)| n.clone()),
                );
                ctx.errors.push(TypeError::QuoteUnknownColumn {
                    entity: param.entity_name.clone(),
                    column: field.text.clone(),
                    suggestions,
                    span: *span,
                });
                None
            }
        }

        Expr::Binary { op, lhs, rhs, span } => check_binary(ctx, b, *op, lhs, rhs, *span, scope),

        // A conditional — `if cond then a else b`. Checked in `check_if`: the
        // condition must be boolean, an `else` is required, and the branches must
        // agree as two values of one type (a value CASE) or two predicates (a
        // boolean CASE).
        Expr::If {
            cond,
            then_branch,
            else_branch,
            span,
        } => check_if(
            ctx,
            b,
            cond,
            then_branch,
            else_branch.as_deref(),
            *span,
            scope,
        ),

        // A predicate helper: `Text.like`/`contains`/`startsWith`/`endsWith` for a
        // text match, `List.contains` for an `IN` test. One operand names a column
        // of the quote, the other is a literal (or a literal list for `IN`).
        Expr::Call { callee, args, span } => {
            let arg_refs: Vec<&Expr> = args.iter().collect();
            check_predicate_call(ctx, b, callee, &arg_refs, *span, scope)
        }
        // `value |> f rest` checks the same as `f rest value` — the piped value is
        // the call's last argument.
        Expr::Pipe { lhs, rhs, span } => {
            let rhs_inner = peel_paren(rhs);
            match rhs_inner {
                Expr::Call { callee, args, .. } => {
                    let mut arg_refs: Vec<&Expr> = args.iter().collect();
                    arg_refs.push(lhs.as_ref());
                    check_predicate_call(ctx, b, callee, &arg_refs, *span, scope)
                }
                Expr::Ident(_) | Expr::Qualified(_) => {
                    check_predicate_call(ctx, b, rhs_inner, &[lhs.as_ref()], *span, scope)
                }
                _ => {
                    ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                        detail: "this pipe is not supported in a quoted predicate".to_string(),
                        span: *span,
                    });
                    None
                }
            }
        }

        Expr::Ident(id) => {
            // The row itself can only be read through a column access.
            if scope.iter().any(|p| p.name == id.text) {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: format!(
                        "the row `{}` can only be used through a column access like `{}.field`",
                        id.text, id.text
                    ),
                    span: e.span(),
                });
                return None;
            }
            // A variable captured from the enclosing scope. A base scalar (Int,
            // Text, Bool, Float) lowers to a `$N` bind, exactly as an inline
            // literal would, so `filter (fn u -> u.age >= minAge)` reads a
            // runtime `minAge` as a query parameter rather than forcing the value
            // to be written inline.
            if let Some(scheme) = ctx.env.lookup(&id.text).cloned() {
                let inst = instantiate(ctx, &scheme);
                let ty = ctx.deep_resolve(&inst);
                if is_quote_scalar(b, &ty) {
                    // The quote checker walks the body itself and never runs
                    // `infer_expr`, so record the captured value's type here: the
                    // lowering reifier reads it back to pick the matching `QLit*`
                    // constructor for the runtime bind.
                    ctx.write_node_type(e.span(), ridge_resolve::NodeKind::Expr, &ty);
                    return Some(QKind::Scalar(ty));
                }
                let detail = if matches!(ty, Type::Var(_)) {
                    format!(
                        "the type of the captured variable `{}` is ambiguous here; annotate it \
                         so it can be sent as a query parameter",
                        id.text
                    )
                } else {
                    let rendered = crate::render::render_type_with(&ty, &ctx.tycon_decls);
                    format!(
                        "`{}` has type `{rendered}`; a quote can capture only Int, Text, Bool, or \
                         Float values from the enclosing scope",
                        id.text
                    )
                };
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail,
                    span: e.span(),
                });
                return None;
            }
            // Neither a column of the quote nor a value in scope.
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "`{}` is not a column of the quote parameters ({}) or a value in scope",
                    id.text,
                    scope_names(scope)
                ),
                span: e.span(),
            });
            None
        }

        other => {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: "this expression form is not supported in a quoted predicate".to_string(),
                span: other.span(),
            });
            None
        }
    }
}

fn check_binary(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
    scope: &[Param],
) -> Option<QKind> {
    match op {
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            let l = check_node(ctx, b, lhs, scope)?;
            let r = check_node(ctx, b, rhs, scope)?;
            let (Some(lt), Some(rt)) = (value_type(&l), value_type(&r)) else {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: "a comparison operand must be a column or a literal".to_string(),
                    span,
                });
                return None;
            };
            let lt = ctx.deep_resolve(lt);
            let rt = ctx.deep_resolve(rt);
            if !same_value_type(&lt, &rt) {
                let left = crate::render::render_type_with(&lt, &ctx.tycon_decls);
                let right = crate::render::render_type_with(&rt, &ctx.tycon_decls);
                ctx.errors
                    .push(TypeError::QuoteComparisonMismatch { left, right, span });
                return None;
            }
            Some(QKind::Pred)
        }
        BinOp::And | BinOp::Or => {
            let l = check_node(ctx, b, lhs, scope)?;
            let r = check_node(ctx, b, rhs, scope)?;
            if as_predicate(b, &l) && as_predicate(b, &r) {
                Some(QKind::Pred)
            } else {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: "the operands of `&&` and `||` must be boolean".to_string(),
                    span,
                });
                None
            }
        }
        // Arithmetic: `a + b`, `a - b`, `a * b`, `a / b`, `a % b`. Both operands
        // must share one numeric type (Int or Float, no implicit coercion), and
        // the result is that type — a *value*, so it lands as a comparison operand
        // (`u.price * u.qty > 100`), not as a predicate of its own.
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
            let l = check_node(ctx, b, lhs, scope)?;
            let r = check_node(ctx, b, rhs, scope)?;
            let (Some(lt), Some(rt)) = (value_type(&l), value_type(&r)) else {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: "an arithmetic operand must be a column or a literal".to_string(),
                    span,
                });
                return None;
            };
            let lt = ctx.deep_resolve(lt);
            let rt = ctx.deep_resolve(rt);
            if !is_numeric(b, &lt) || !is_numeric(b, &rt) || !same_value_type(&lt, &rt) {
                let left = crate::render::render_type_with(&lt, &ctx.tycon_decls);
                let right = crate::render::render_type_with(&rt, &ctx.tycon_decls);
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: format!(
                        "arithmetic (`+ - * / %`) takes two operands of the same numeric type \
                         (Int or Float), but found {left} and {right}"
                    ),
                    span,
                });
                return None;
            }
            // `%` (modulo) is Int-only — Postgres does not define it on Float, so
            // the in-memory backend would have no matching operation either.
            if matches!(op, BinOp::Mod) && !is_int(b, &lt) {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: "modulo (`%`) applies to Int operands".to_string(),
                    span,
                });
                return None;
            }
            // A literal-zero divisor is a guaranteed error — reject it here, rather
            // than at run time where Postgres aborts the query and the in-memory
            // backend drops the row.
            if matches!(op, BinOp::Div | BinOp::Mod) && is_literal_zero(peel_paren(rhs)) {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: "division by zero".to_string(),
                    span,
                });
                return None;
            }
            Some(QKind::Scalar(lt))
        }
        _ => {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: "this operator is not supported in a quoted predicate".to_string(),
                span,
            });
            None
        }
    }
}

/// Checks an `if`/`then`/`else` inside a quote. The condition must be boolean and
/// an `else` is required (a CASE with no else has no value for the rows the
/// condition does not match). The branches must agree, either as two values of one
/// type — a value CASE, whose type is that shared type — or as two predicates, a
/// boolean CASE usable wherever a predicate is. The value reading is preferred, so
/// two boolean columns make a `Bool` value, not a predicate.
fn check_if(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    cond: &Expr,
    then_branch: &Expr,
    else_branch: Option<&Expr>,
    span: Span,
    scope: &[Param],
) -> Option<QKind> {
    let c = check_node(ctx, b, cond, scope)?;
    if !as_predicate(b, &c) {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "the condition of an `if`/`then`/`else` in a quote must be boolean".to_string(),
            span: cond.span(),
        });
        return None;
    }
    let Some(else_branch) = else_branch else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "an `if`/`then`/`else` in a quote must have an `else` branch".to_string(),
            span,
        });
        return None;
    };
    let t = check_node(ctx, b, then_branch, scope)?;
    let e = check_node(ctx, b, else_branch, scope)?;
    match (value_type(&t), value_type(&e)) {
        // Both branches carry a value (column or scalar) → a value CASE. The
        // branches must share one type, which the CASE then yields.
        (Some(tt), Some(et)) => {
            let tt = ctx.deep_resolve(tt);
            let et = ctx.deep_resolve(et);
            if same_value_type(&tt, &et) {
                Some(QKind::Scalar(tt))
            } else {
                let left = crate::render::render_type_with(&tt, &ctx.tycon_decls);
                let right = crate::render::render_type_with(&et, &ctx.tycon_decls);
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: format!(
                        "the branches of an `if`/`then`/`else` in a quote must have the same \
                         type, but found {left} and {right}"
                    ),
                    span,
                });
                None
            }
        }
        // Otherwise both branches must be predicates → a boolean CASE.
        _ if as_predicate(b, &t) && as_predicate(b, &e) => Some(QKind::Pred),
        _ => {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: "the branches of an `if`/`then`/`else` in a quote must both be values \
                         of the same type, or both be boolean"
                    .to_string(),
                span,
            });
            None
        }
    }
}

/// Peel any number of parentheses from an expression.
fn peel_paren(e: &Expr) -> &Expr {
    match e {
        Expr::Paren { inner, .. } => peel_paren(inner),
        other => other,
    }
}

/// The last segment of a call's callee — the function name, whether bare
/// (`contains`) or qualified (`List.contains`).
fn callee_last_name(callee: &Expr) -> Option<&str> {
    match callee {
        Expr::Ident(id) => Some(id.text.as_str()),
        Expr::Qualified(qn) => qn.segments.last().map(|s| s.text.as_str()),
        _ => None,
    }
}

/// Whether `e` is a column access on one of the quote's parameters (`u.field`).
fn is_scope_column(e: &Expr, scope: &[Param]) -> bool {
    matches!(
        peel_paren(e),
        Expr::FieldAccess { base, .. }
            if matches!(base.as_ref(), Expr::Ident(id) if scope.iter().any(|p| p.name == id.text))
    )
}

/// Whether `ty` is the `Text` base type.
fn is_text(b: &BuiltinTyCons, ty: &Type) -> bool {
    matches!(ty, Type::Con(id, _) if *id == b.text)
}

/// Whether `ty` is a numeric base type (`Int` or `Float`).
fn is_numeric(b: &BuiltinTyCons, ty: &Type) -> bool {
    matches!(ty, Type::Con(id, _) if *id == b.int || *id == b.float)
}

/// Whether `ty` is the `Int` base type.
fn is_int(b: &BuiltinTyCons, ty: &Type) -> bool {
    matches!(ty, Type::Con(id, _) if *id == b.int)
}

/// Whether `ty` is a base scalar a quote can capture from the enclosing scope as
/// a runtime bind: Int, Text, Bool, or Float. These are exactly the types with a
/// `QLit*` node and a `SqlValue` wrapper in both the SQL and in-memory backends.
fn is_quote_scalar(b: &BuiltinTyCons, ty: &Type) -> bool {
    matches!(ty, Type::Con(id, _)
        if *id == b.int || *id == b.text || *id == b.bool || *id == b.float)
}

/// Whether `e` is a numeric literal whose value is zero, in any radix or as a
/// float — the one statically-detectable division-by-zero divisor.
fn is_literal_zero(e: &Expr) -> bool {
    let Expr::Literal(lit) = e else {
        return false;
    };
    match lit {
        Literal::IntDec { raw, .. }
        | Literal::IntBin { raw, .. }
        | Literal::IntOct { raw, .. }
        | Literal::IntHex { raw, .. } => {
            let s = raw.replace('_', "");
            let digits = s
                .strip_prefix("0b")
                .or_else(|| s.strip_prefix("0B"))
                .or_else(|| s.strip_prefix("0o"))
                .or_else(|| s.strip_prefix("0O"))
                .or_else(|| s.strip_prefix("0x"))
                .or_else(|| s.strip_prefix("0X"))
                .unwrap_or(s.as_str());
            !digits.is_empty() && digits.bytes().all(|c| c == b'0')
        }
        Literal::Float { raw, .. } => raw.replace('_', "").parse::<f64>().is_ok_and(|v| v == 0.0),
        _ => false,
    }
}

/// Checks a predicate-helper call inside a quote. The recognised helpers are the
/// text matches (`Text.like`/`contains`/`startsWith`/`endsWith`) and the `IN`
/// membership test (`List.contains col [literals]`). One operand must name a column
/// of the quote; the other must be a literal pattern (or a list of literals for
/// `IN`) whose type matches the column. Returns `QKind::Pred` on success.
fn check_predicate_call(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    callee: &Expr,
    args: &[&Expr],
    span: Span,
    scope: &[Param],
) -> Option<QKind> {
    let name = callee_last_name(callee);
    if !matches!(name, Some("contains" | "startsWith" | "endsWith" | "like")) {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "this call is not supported in a quoted predicate; use a comparison, \
                     `Text.like`/`contains`/`startsWith`/`endsWith`, or `List.contains` for `IN`"
                .to_string(),
            span,
        });
        return None;
    }
    if args.len() != 2 {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: format!(
                "`{}` takes a column and a literal in a quoted predicate",
                name.unwrap_or("")
            ),
            span,
        });
        return None;
    }
    let (a0, a1) = (peel_paren(args[0]), peel_paren(args[1]));
    let (col, other) = if is_scope_column(a0, scope) {
        (a0, a1)
    } else if is_scope_column(a1, scope) {
        (a1, a0)
    } else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: format!(
                "a text or `IN` predicate must name a column of {}",
                scope_names(scope)
            ),
            span,
        });
        return None;
    };
    let QKind::Col(col_ty) = check_node(ctx, b, col, scope)? else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "the matched operand must be a column".to_string(),
            span,
        });
        return None;
    };
    let col_ty = ctx.deep_resolve(&col_ty);

    // `List.contains col [literals]` — the `IN` test. Every element must be a
    // literal of the column's type.
    if matches!(name, Some("contains")) {
        if let Expr::List { elems, .. } = other {
            for el in elems {
                let el = peel_paren(el);
                let Expr::Literal(lit) = el else {
                    ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                        detail: "every element of an `IN` list must be a literal".to_string(),
                        span: el.span(),
                    });
                    return None;
                };
                let elt = literal_type(b, lit);
                if !same_value_type(&elt, &col_ty) {
                    let left = crate::render::render_type_with(&col_ty, &ctx.tycon_decls);
                    let right = crate::render::render_type_with(&elt, &ctx.tycon_decls);
                    ctx.errors.push(TypeError::QuoteComparisonMismatch {
                        left,
                        right,
                        span: el.span(),
                    });
                    return None;
                }
            }
            return Some(QKind::Pred);
        }
    }

    // The text-match family. The column must be `Text` and the pattern a text
    // literal.
    if !is_text(b, &col_ty) {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a text match (`like`/`contains`/`startsWith`/`endsWith`) applies to a \
                     Text column"
                .to_string(),
            span,
        });
        return None;
    }
    let Expr::Literal(lit) = other else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "the pattern of a text match must be a text literal".to_string(),
            span: other.span(),
        });
        return None;
    };
    if !is_text(b, &literal_type(b, lit)) {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "the pattern of a text match must be Text".to_string(),
            span: other.span(),
        });
        return None;
    }
    Some(QKind::Pred)
}

// ── Grouped-aggregate quotes (`groupBy` → `having` / `summarize`) ────────────
//
// A `having`/`summarize` lambda ranges over a `Group e k` handle, not a row. Its
// body is a small vocabulary over the group rather than the row columns:
// `g.key` (the group key), `g.count` (`COUNT(*)`), and `g.sum`/`avg`/`min`/`max`
// applied to a column accessor (`g.sum (fn u -> u.salary)`). A `summarize` body
// is a named record of these aggregates (a projection); a `having` body is a
// boolean expression over them. The checker validates the shapes and pins the
// result types; the lowering pass reifies them into the `QAgg*`/`QGroupKey`
// tree the seam interprets.

/// Detects a grouped-aggregate quote: a `Quote (Grouped q p -> r)` whose first
/// parameter is the `Grouped` handle. The source `q` carries the grouped entities
/// (a `Query e a` / `Join e f a` / `LeftJoin e f a`) and the key-accessor type `p`
/// carries the key. Returns the primary entity and the key type so the group checker
/// can resolve the inner aggregate columns and the `g.key` type. `None` for an
/// ordinary row quote.
///
/// The entity is `q`'s first type argument; when `q` is not yet resolved (the quote
/// is synthesised before the receiver pins it) it stays open and the inner aggregate
/// accessors' own annotations pin it, exactly as a row quote recovers its entity. The
/// key is `p`'s result type; an open `p` leaves it open until the projection pins it.
pub(crate) fn quote_group_slot(ctx: &mut InferCtx, param_ty: &Type) -> Option<(Type, Type)> {
    let Type::Con(id, args) = param_ty else {
        return None;
    };
    if !is_quote_tycon(ctx, *id) {
        return None;
    }
    let inner = ctx.deep_resolve(args.first()?);
    let Type::Fn { params, .. } = inner else {
        return None;
    };
    let slot = ctx.deep_resolve(params.first()?);
    let Type::Con(gid, gargs) = slot else {
        return None;
    };
    if !is_group_tycon(ctx, gid) {
        return None;
    }
    let q = ctx.deep_resolve(gargs.first()?);
    let p = ctx.deep_resolve(gargs.get(1)?);
    // The entity is `q`'s first type argument. When `q` is still an inference variable
    // (the quote is synthesised before the receiver pins it), use a *fresh* entity
    // variable rather than `q` itself: the inner accessors' annotations unify the
    // entity with their row type, and aliasing it to `q` would corrupt the source —
    // pinning `q` to the entity instead of to its `Query`/`Join`/`LeftJoin`.
    let e = match &q {
        Type::Con(_, qargs) => match qargs.first() {
            Some(a) => ctx.deep_resolve(a),
            None => Type::Var(ctx.fresh_tyvid()),
        },
        _ => Type::Var(ctx.fresh_tyvid()),
    };
    let k = match &p {
        Type::Fn { ret, .. } => ctx.deep_resolve(ret),
        _ => Type::Var(ctx.fresh_tyvid()),
    };
    Some((e, k))
}

fn is_group_tycon(ctx: &InferCtx, id: TyConId) -> bool {
    ctx.tycon_decls
        .get(id.0 as usize)
        .is_some_and(|d| d.name == "Grouped")
}

/// Checks a grouped-aggregate lambda body. On success records a [`QuoteInfo`]
/// (marked `group`) for the lambda span and returns `true`; on failure pushes a
/// diagnostic and returns `false`. `e_ty`/`k_ty` are the entity and key types of
/// the `Group e k` the lambda ranges over.
pub(crate) fn check_group_quote(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    lambda: &Expr,
    e_ty: &Type,
    k_ty: &Type,
    expected_ret: Option<&Type>,
) -> bool {
    let Expr::Lambda { params, body, span } = lambda else {
        return false;
    };
    if params.len() != 1 {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a grouped quote takes one group parameter, like `fn g -> …`".to_string(),
            span: *span,
        });
        return false;
    }
    let g_name = match &params[0] {
        LambdaParam::Pattern(Pattern::Var { name, .. })
        | LambdaParam::Annotated {
            pat: Pattern::Var { name, .. },
            ..
        } => name.text.clone(),
        _ => {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: "a quoted parameter must be a plain name".to_string(),
                span: *span,
            });
            return false;
        }
    };

    let want = expected_ret.map(|r| ctx.deep_resolve(r));

    // A named-constructor projection (`Stats { dept = g.key, … }`) is a
    // `summarize`: it names the result record directly, pinning `s` even when the
    // declared result is still an inference variable, exactly as the row
    // projection does for `selectList`.
    let mut named: &Expr = body;
    while let Expr::Paren { inner, .. } = named {
        named = inner;
    }
    if let Expr::Record { constructor, .. } = named {
        let ctor_name = match constructor {
            ridge_ast::RecordCtor::Bare(id) => id.text.clone(),
            ridge_ast::RecordCtor::Qualified(qn) => qn
                .segments
                .last()
                .map_or_else(String::new, |s| s.text.clone()),
        };
        let Some(target_id) = ctx.user_tycon_names.get(ctor_name.as_str()).copied() else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "a grouped projection names `{ctor_name}`, which is not a record type"
                ),
                span: named.span(),
            });
            return false;
        };
        let Some(target_fields) = entity_fields(ctx, target_id) else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!("a grouped projection names `{ctor_name}`, which is not a record"),
                span: named.span(),
            });
            return false;
        };
        if !check_group_projection(ctx, b, named, &g_name, e_ty, k_ty, &target_fields, *span) {
            return false;
        }
        if let Some(want_ty) = want.as_ref() {
            let arity = ctx
                .tycon_decls
                .get(target_id.0 as usize)
                .map_or(0, |d| d.arity);
            let args = (0..arity).map(|_| Type::Var(ctx.fresh_tyvid())).collect();
            let _ = crate::unify::unify(ctx, want_ty, &Type::Con(target_id, args));
        }
        record_group_quote(ctx, *span, &g_name, e_ty);
        return true;
    }

    // An anonymous projection whose result type is already a known record.
    if let Some(proj) = want.as_ref().and_then(|t| record_fields_of(ctx, t)) {
        if check_group_projection(ctx, b, body, &g_name, e_ty, k_ty, &proj, *span) {
            record_group_quote(ctx, *span, &g_name, e_ty);
            return true;
        }
        return false;
    }
    if matches!(named, Expr::RecordLit { .. }) {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a grouped projection must name its result record, e.g. \
                     `Stats { dept = g.key, n = g.count }`"
                .to_string(),
            span: named.span(),
        });
        return false;
    }

    // Otherwise a `having` predicate over the group.
    let Some(qk) = check_group_node(ctx, b, body, &g_name, e_ty, k_ty) else {
        return false;
    };
    if as_predicate(b, &qk) {
        record_group_quote(ctx, *span, &g_name, e_ty);
        return true;
    }
    ctx.errors.push(TypeError::QuoteUnsupportedExpr {
        detail: "a grouped `having` must be a boolean expression over the group".to_string(),
        span: body.span(),
    });
    false
}

/// Records a grouped quote for the lowering pass. The entity is taken from `e_ty`
/// when concrete (pinned by an inner aggregate's row annotation); for a `having`
/// that never names a column it stays a placeholder, which the group lowering
/// does not read.
fn record_group_quote(ctx: &mut InferCtx, span: Span, g_name: &str, e_ty: &Type) {
    let entity = match ctx.deep_resolve(e_ty) {
        Type::Con(id, _) => id,
        _ => TyConId(0),
    };
    ctx.quoted_lambdas_accum.insert(
        span,
        QuoteInfo {
            param_name: g_name.to_string(),
            entity,
            group: true,
        },
    );
}

/// Checks a grouped projection body — a record whose every field is a group
/// aggregate — against the declared result record `expected`.
#[expect(
    clippy::too_many_arguments,
    reason = "the group projection check threads the scope (g name, entity, key) and the target the same way check_projection threads its scope"
)]
fn check_group_projection(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    body: &Expr,
    g_name: &str,
    e_ty: &Type,
    k_ty: &Type,
    expected: &[(String, Type)],
    span: Span,
) -> bool {
    let mut body = body;
    while let Expr::Paren { inner, .. } = body {
        body = inner;
    }
    let (Expr::RecordLit { fields: inits, .. } | Expr::Record { fields: inits, .. }) = body else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a grouped projection must be a record of group aggregates, like \
                     `Stats { dept = g.key, n = g.count }`"
                .to_string(),
            span: body.span(),
        });
        return false;
    };
    if inits.is_empty() {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a grouped projection must select at least one aggregate".to_string(),
            span: body.span(),
        });
        return false;
    }

    for fi in inits {
        let Some(value) = &fi.value else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{0}` must be written `{0} = <group aggregate>`",
                    fi.name.text
                ),
                span: fi.span,
            });
            return false;
        };
        let Some(qk) = check_group_node(ctx, b, value, g_name, e_ty, k_ty) else {
            return false;
        };
        let Some(val_ty) = value_type(&qk) else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{}` must be a group aggregate (g.key, g.count, \
                     g.sum/avg/min/max)",
                    fi.name.text
                ),
                span: fi.span,
            });
            return false;
        };
        let Some((_, exp_ty)) = expected.iter().find(|(n, _)| n == &fi.name.text) else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{}` is not declared in the result record",
                    fi.name.text
                ),
                span: fi.span,
            });
            return false;
        };
        let val_ty = ctx.deep_resolve(val_ty);
        let exp_ty = ctx.deep_resolve(exp_ty);
        // The key type is open until the projection pins it; unify rather than
        // compare so `g.key` takes the declared field type.
        if matches!(val_ty, Type::Var(_)) {
            let _ = crate::unify::unify(ctx, &val_ty, &exp_ty);
        } else if !same_value_type(&val_ty, &exp_ty) {
            let left = crate::render::render_type_with(&val_ty, &ctx.tycon_decls);
            let right = crate::render::render_type_with(&exp_ty, &ctx.tycon_decls);
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{}` is {left}, but the result record declares {right}",
                    fi.name.text
                ),
                span: fi.span,
            });
            return false;
        }
    }

    for (n, _) in expected {
        if !inits.iter().any(|fi| &fi.name.text == n) {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!("a grouped projection is missing the declared column `{n}`"),
                span,
            });
            return false;
        }
    }
    true
}

fn check_group_node(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    e: &Expr,
    g_name: &str,
    e_ty: &Type,
    k_ty: &Type,
) -> Option<QKind> {
    match e {
        Expr::Paren { inner, .. } => check_group_node(ctx, b, inner, g_name, e_ty, k_ty),
        Expr::Literal(lit) => Some(QKind::Scalar(literal_type(b, lit))),
        Expr::FieldAccess { base, field, span } => {
            if !is_group_base(base, g_name) {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: format!("only the group `{g_name}` can be accessed here"),
                    span: *span,
                });
                return None;
            }
            match field.text.as_str() {
                "key" => Some(QKind::Scalar(k_ty.clone())),
                "count" => Some(QKind::Scalar(Type::Con(b.int, vec![]))),
                other => {
                    ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                        detail: format!(
                            "`{g_name}.{other}` is not a group aggregate; use `{g_name}.key`, \
                             `{g_name}.count`, or `{g_name}.sum`/`avg`/`min`/`max`"
                        ),
                        span: *span,
                    });
                    None
                }
            }
        }
        Expr::Call { callee, args, span } => {
            check_group_call(ctx, b, callee, args, *span, g_name, e_ty)
        }
        Expr::Binary { op, lhs, rhs, span } => {
            check_group_binary(ctx, b, *op, lhs, rhs, *span, g_name, e_ty, k_ty)
        }
        Expr::Ident(id) => {
            let detail = if id.text == g_name {
                format!(
                    "the group `{g_name}` can only be used through `{g_name}.key`, \
                     `{g_name}.count`, or `{g_name}.sum`/`avg`/`min`/`max`"
                )
            } else {
                format!("`{}` is not available in a grouped quote", id.text)
            };
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail,
                span: e.span(),
            });
            None
        }
        other => {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: "this expression form is not supported in a grouped quote".to_string(),
                span: other.span(),
            });
            None
        }
    }
}

/// Whether `base` is the group parameter `g_name`.
fn is_group_base(base: &Expr, g_name: &str) -> bool {
    matches!(base, Expr::Ident(id) if id.text == g_name)
}

/// Checks `g.sum`/`avg`/`min`/`max (fn u -> u.col)`. The aggregate keyword is the
/// field on the group; the single argument is a column accessor whose column type
/// `n` becomes the aggregate's type (`avg` is always `Float`).
fn check_group_call(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    callee: &Expr,
    args: &[Expr],
    span: Span,
    g_name: &str,
    e_ty: &Type,
) -> Option<QKind> {
    let Expr::FieldAccess { base, field, .. } = callee else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "this call is not supported in a grouped quote".to_string(),
            span,
        });
        return None;
    };
    if !is_group_base(base, g_name) {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: format!("only the group `{g_name}` can be aggregated here"),
            span,
        });
        return None;
    }
    let func = field.text.as_str();
    if !matches!(func, "sum" | "avg" | "min" | "max") {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: format!(
                "`{g_name}.{func}` is not a group aggregate; use `{g_name}.sum`, `{g_name}.avg`, \
                 `{g_name}.min`, or `{g_name}.max`"
            ),
            span,
        });
        return None;
    }
    if args.len() != 1 {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: format!(
                "`{g_name}.{func}` takes one column accessor, like `{g_name}.{func} (fn u -> u.col)`"
            ),
            span,
        });
        return None;
    }
    let col_ty = group_agg_col_type(ctx, b, &args[0], e_ty)?;
    let ty = if func == "avg" {
        Type::Con(b.float, vec![])
    } else {
        col_ty
    };
    Some(QKind::Scalar(ty))
}

/// The value type a group aggregate folds: the inner lambda `fn (u: E) -> u.col`
/// (or a computed `fn (u: E) -> u.price * u.qty`) reads columns of `E`, and its
/// result type is the aggregate's type. The entity comes from the inner
/// parameter's annotation (pinning `e_ty`), or from a concrete `e_ty` when the
/// annotation is omitted; the body is checked as a `where`/`select` operand is.
fn group_agg_col_type(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    arg: &Expr,
    e_ty: &Type,
) -> Option<Type> {
    let mut inner = arg;
    while let Expr::Paren { inner: i, .. } = inner {
        inner = i;
    }
    let Expr::Lambda { params, body, span } = inner else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a group aggregate takes a column accessor, like `(fn u -> u.col)`".to_string(),
            span: arg.span(),
        });
        return None;
    };
    // One row parameter for a query group, two for a binary join, and one per leaf
    // for a deeper composite (naming a column from any leaf, `fn (u: User) (p: Post)
    // (c: Comment) -> c.col`). The first parameter ties `e_ty` (the single-entity
    // fallback); each further parameter — a join's right side or a composite's deeper
    // leaf — is pinned only from its own annotation. The lowering tags which leaf a
    // column belongs to by which parameter it reads (`QCol`/`QColR`/`QColAt`).
    if params.is_empty() {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a group aggregate's column accessor takes at least one row parameter"
                .to_string(),
            span: *span,
        });
        return None;
    }
    let mut scope: Vec<Param> = Vec::with_capacity(params.len());
    for (i, param) in params.iter().enumerate() {
        let pname = match param {
            LambdaParam::Pattern(Pattern::Var { name, .. })
            | LambdaParam::Annotated {
                pat: Pattern::Var { name, .. },
                ..
            } => name.text.clone(),
            _ => {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: "a group aggregate's row parameter must be a plain name".to_string(),
                    span: *span,
                });
                return None;
            }
        };
        let entity = if i == 0 {
            inner_lambda_entity(ctx, b, param, e_ty, *span)?
        } else {
            let slot = Type::Var(ctx.fresh_tyvid());
            inner_lambda_entity(ctx, b, param, &slot, *span)?
        };
        let entity_name = ctx
            .tycon_decls
            .get(entity.0 as usize)
            .map_or_else(|| "?".to_string(), |d| d.name.clone());
        let fields = entity_fields(ctx, entity)?;
        scope.push(Param {
            name: pname,
            entity,
            entity_name,
            fields,
            nullable: false,
        });
    }

    // The folded value may be a single column or a computed expression over the
    // group's columns (`g.sum (fn u -> u.price * u.qty)`), checked against the
    // accessor's entities exactly as a `where`/`select` operand is — a literal in
    // it binds as a placeholder, never reaching the generated SQL. A boolean
    // predicate is not a foldable value, so it is rejected.
    match check_node(ctx, b, body, &scope)? {
        QKind::Col(ty) | QKind::Scalar(ty) => Some(ty),
        QKind::Pred => {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: "a group aggregate folds a column or computed value, not a predicate"
                    .to_string(),
                span: body.span(),
            });
            None
        }
    }
}

/// The entity a group aggregate's inner lambda ranges over: from the parameter's
/// annotation (unified into `e_ty`), or a concrete `e_ty` fallback.
fn inner_lambda_entity(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    param: &LambdaParam,
    e_ty: &Type,
    span: Span,
) -> Option<TyConId> {
    if let LambdaParam::Annotated { ty, .. } = param {
        let ann = crate::infer::ast_type_to_type(ctx, b, ty);
        let _ = crate::unify::unify(ctx, e_ty, &ann);
        if let Type::Con(id, _) = ctx.deep_resolve(&ann) {
            return Some(id);
        }
    }
    if let Type::Con(id, _) = ctx.deep_resolve(e_ty) {
        return Some(id);
    }
    ctx.errors.push(TypeError::QuoteUnsupportedExpr {
        detail: "annotate the aggregate's row parameter, like `(fn (u: User) -> u.col)`"
            .to_string(),
        span,
    });
    None
}

#[expect(
    clippy::too_many_arguments,
    reason = "a group comparison threads the same scope (g name, entity, key) as check_group_node plus the two operands"
)]
fn check_group_binary(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
    g_name: &str,
    e_ty: &Type,
    k_ty: &Type,
) -> Option<QKind> {
    match op {
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            let l = check_group_node(ctx, b, lhs, g_name, e_ty, k_ty)?;
            let r = check_group_node(ctx, b, rhs, g_name, e_ty, k_ty)?;
            let (Some(lt), Some(rt)) = (value_type(&l), value_type(&r)) else {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: "a comparison operand must be a group aggregate or a literal"
                        .to_string(),
                    span,
                });
                return None;
            };
            let lt = ctx.deep_resolve(lt);
            let rt = ctx.deep_resolve(rt);
            // One side may be the still-open key type; unify it with the other.
            if matches!(lt, Type::Var(_)) || matches!(rt, Type::Var(_)) {
                let _ = crate::unify::unify(ctx, &lt, &rt);
            } else if !same_value_type(&lt, &rt) {
                let left = crate::render::render_type_with(&lt, &ctx.tycon_decls);
                let right = crate::render::render_type_with(&rt, &ctx.tycon_decls);
                ctx.errors
                    .push(TypeError::QuoteComparisonMismatch { left, right, span });
                return None;
            }
            Some(QKind::Pred)
        }
        BinOp::And | BinOp::Or => {
            let l = check_group_node(ctx, b, lhs, g_name, e_ty, k_ty)?;
            let r = check_group_node(ctx, b, rhs, g_name, e_ty, k_ty)?;
            if as_predicate(b, &l) && as_predicate(b, &r) {
                Some(QKind::Pred)
            } else {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: "the operands of `&&` and `||` must be boolean".to_string(),
                    span,
                });
                None
            }
        }
        _ => {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: "this operator is not supported in a grouped quote".to_string(),
                span,
            });
            None
        }
    }
}

const fn literal_type(b: &BuiltinTyCons, lit: &Literal) -> Type {
    let id = match lit {
        Literal::IntDec { .. }
        | Literal::IntBin { .. }
        | Literal::IntOct { .. }
        | Literal::IntHex { .. } => b.int,
        Literal::Float { .. } => b.float,
        Literal::Bool { .. } => b.bool,
        Literal::Text { .. } | Literal::RawText { .. } => b.text,
    };
    Type::Con(id, vec![])
}

const fn value_type(qk: &QKind) -> Option<&Type> {
    match qk {
        QKind::Col(t) | QKind::Scalar(t) => Some(t),
        QKind::Pred => None,
    }
}

/// A `QKind` usable in a boolean position: an explicit predicate, or a boolean
/// column/literal (so `fn u -> u.active` and `active && age >= 18` both work).
fn as_predicate(b: &BuiltinTyCons, qk: &QKind) -> bool {
    match qk {
        QKind::Pred => true,
        QKind::Col(t) | QKind::Scalar(t) => matches!(t, Type::Con(id, _) if *id == b.bool),
    }
}

fn same_value_type(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Con(ia, aa), Type::Con(ib, ab)) => {
            ia == ib
                && aa.len() == ab.len()
                && aa.iter().zip(ab).all(|(x, y)| same_value_type(x, y))
        }
        _ => false,
    }
}
