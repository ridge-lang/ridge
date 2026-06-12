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

    let is_bool_result = want
        .as_ref()
        .is_none_or(|r| matches!(r, Type::Con(id, _) if *id == b.bool));

    if is_bool_result {
        if as_predicate(b, &qk) {
            ctx.quoted_lambdas_accum.insert(*span, quote_info(&scope));
            return true;
        }
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a quoted predicate must be a boolean expression".to_string(),
            span: body.span(),
        });
        return false;
    }

    // A non-boolean scalar result is an ordering key, which is only meaningful
    // for a single-parameter quote (`orderBy`). A multi-parameter quote must be
    // a predicate or a named projection.
    if scope.len() != 1 {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a multi-parameter quote must be a predicate or a named projection".to_string(),
            span: body.span(),
        });
        return false;
    }

    // The ordering key must be a single column (or a literal). Its type must
    // match the quote's declared result type — except when that result type is
    // an unbound variable (a polymorphic `orderBy` key, whose return is phantom),
    // in which case any column is accepted and the variable is bound to it.
    let want_ty = want.unwrap_or_else(|| Type::Con(b.bool, vec![]));
    let col_ty = value_type(&qk).map(|vt| ctx.deep_resolve(vt));
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
        detail: format!("a quoted ordering key must be a single column of type {want_rendered}"),
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
        let QKind::Col(col_ty) = qk else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{}` must be a column of one of {}",
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
                    "projection field `{}` is a column of type {left}, but the result \
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

        Expr::Ident(id) => {
            let detail = if scope.iter().any(|p| p.name == id.text) {
                format!(
                    "the row `{}` can only be used through a column access like `{}.field`",
                    id.text, id.text
                )
            } else {
                format!(
                    "`{}` is a captured variable, which is not supported in a quoted predicate yet",
                    id.text
                )
            };
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail,
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
        _ => {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: "this operator is not supported in a quoted predicate".to_string(),
                span,
            });
            None
        }
    }
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

/// Detects a grouped-aggregate quote: a `Quote (Group e k -> r)` whose first
/// parameter is the `Group` handle. Returns the entity `e` and key `k` types so
/// the group checker can resolve the inner aggregate columns and the `g.key`
/// type. `None` for an ordinary row quote.
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
    let e = ctx.deep_resolve(gargs.first()?);
    let k = ctx.deep_resolve(gargs.get(1)?);
    Some((e, k))
}

fn is_group_tycon(ctx: &InferCtx, id: TyConId) -> bool {
    ctx.tycon_decls
        .get(id.0 as usize)
        .is_some_and(|d| d.name == "Group")
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

/// The column type a group aggregate folds: the inner lambda `fn (u: E) -> u.col`
/// names a single column of `E`, whose declared type is the aggregate's type. The
/// entity comes from the inner parameter's annotation (pinning `e_ty`), or from a
/// concrete `e_ty` when the annotation is omitted.
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
    if params.len() != 1 {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a group aggregate's column accessor takes one row parameter".to_string(),
            span: *span,
        });
        return None;
    }
    let pname = match &params[0] {
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
    // Resolve the entity from the parameter annotation (tying `e_ty` to it), or
    // fall back to a concrete `e_ty`.
    let entity = inner_lambda_entity(ctx, b, &params[0], e_ty, *span)?;

    let mut bd: &Expr = body;
    while let Expr::Paren { inner: i, .. } = bd {
        bd = i;
    }
    let Expr::FieldAccess {
        base,
        field,
        span: fspan,
    } = bd
    else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a group aggregate's column must be a single column access, like `u.col`"
                .to_string(),
            span: body.span(),
        });
        return None;
    };
    if !matches!(base.as_ref(), Expr::Ident(id) if id.text == pname) {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: format!("a group aggregate's column must be a column of `{pname}`"),
            span: *fspan,
        });
        return None;
    }
    let fields = entity_fields(ctx, entity)?;
    if let Some((_, ty)) = fields.iter().find(|(n, _)| n == &field.text) {
        Some(ty.clone())
    } else {
        let entity_name = ctx
            .tycon_decls
            .get(entity.0 as usize)
            .map_or_else(|| "?".to_string(), |d| d.name.clone());
        let suggestions =
            ridge_resolve::suggest::suggest(&field.text, fields.iter().map(|(n, _)| n.clone()));
        ctx.errors.push(TypeError::QuoteUnknownColumn {
            entity: entity_name,
            column: field.text.clone(),
            suggestions,
            span: *fspan,
        });
        None
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
