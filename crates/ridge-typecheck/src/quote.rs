//! Quotation type-checking — the isolated checker for a quoted predicate.
//!
//! When a lambda is passed where a `Quote (Entity -> Bool)` is expected, the
//! body is not checked as an ordinary function. It is checked here, against the
//! entity's columns, by a small dedicated walk that never touches the core
//! operator typing (`infer_binary`) or relaxes any language rule. The quoted
//! sub-language is deliberately narrow: column references, literals, the six
//! comparisons, and `&&`/`||`.
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
    /// The lambda's single parameter name (the row bound to the entity).
    pub param_name: String,
    /// The entity type the predicate is checked against.
    pub entity: TyConId,
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

/// Extracts the concrete entity `TyConId` from a `Quote (e -> r)` type.
///
/// Returns `None` when the type is not a `Quote`, when its inner shape is not a
/// one-parameter function, or when the entity `e` is not a concrete type
/// constructor (e.g. still an inference variable).
pub(crate) fn quote_entity(ctx: &mut InferCtx, param_ty: &Type) -> Option<TyConId> {
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
    match ctx.deep_resolve(params.first()?) {
        Type::Con(entity, _) => Some(entity),
        _ => None,
    }
}

/// Extracts the resolved result type `r` from a `Quote (e -> r)` type.
///
/// Returns `None` when the type is not a `Quote` wrapping a one-parameter
/// function. The result type selects the accepted body shape in
/// [`check_quote`]: `Bool` is a predicate (a `where` body); a scalar column
/// type is a single column (an `orderBy` key).
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

/// Checks a quoted lambda body against `entity`. On success records a
/// [`QuoteInfo`] for the lambda span and returns `true`; on failure pushes a
/// diagnostic and returns `false`.
pub(crate) fn check_quote(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    lambda: &Expr,
    entity: TyConId,
    expected_ret: Option<&Type>,
) -> bool {
    let Expr::Lambda { params, body, span } = lambda else {
        return false;
    };

    if params.len() != 1 {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a quoted predicate must take exactly one parameter".to_string(),
            span: *span,
        });
        return false;
    }
    let param_name = match &params[0] {
        LambdaParam::Pattern(Pattern::Var { name, .. })
        | LambdaParam::Annotated {
            pat: Pattern::Var { name, .. },
            ..
        } => name.text.clone(),
        _ => {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: "the predicate parameter must be a plain name".to_string(),
                span: *span,
            });
            return false;
        }
    };

    // Snapshot the entity's columns (owned) so the recursive walk can borrow
    // `ctx` mutably without aliasing the arena snapshot.
    let entity_name = ctx
        .tycon_decls
        .get(entity.0 as usize)
        .map_or_else(|| "?".to_string(), |d| d.name.clone());
    let Some(fields) = entity_fields(ctx, entity) else {
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: format!("`{entity_name}` is not a record type, so it has no columns to quote"),
            span: *span,
        });
        return false;
    };

    // The quote's result type selects the accepted body shape: a `Bool` result
    // is a predicate (a `where` body); a record result is a projection (a
    // `select` list); any other scalar result is a single column (an `orderBy`
    // key). With no result type known, fall back to predicate.
    let want = expected_ret.map(|r| ctx.deep_resolve(r));

    // Projection: a record result is a select-list of columns.
    if let Some(proj) = want.as_ref().and_then(|t| record_fields_of(ctx, t)) {
        return check_projection(
            ctx,
            b,
            body,
            &param_name,
            &fields,
            &entity_name,
            &proj,
            *span,
            entity,
        );
    }

    let Some(qk) = check_node(ctx, b, body, &param_name, &fields, &entity_name) else {
        return false;
    };

    let is_bool_result = want
        .as_ref()
        .is_none_or(|r| matches!(r, Type::Con(id, _) if *id == b.bool));

    if is_bool_result {
        if as_predicate(b, &qk) {
            ctx.quoted_lambdas_accum
                .insert(*span, QuoteInfo { param_name, entity });
            return true;
        }
        ctx.errors.push(TypeError::QuoteUnsupportedExpr {
            detail: "a quoted predicate must be a boolean expression".to_string(),
            span: body.span(),
        });
        return false;
    }

    // Non-boolean result: an ordering key, which must be a single column (or a
    // literal). Its type must match the quote's declared result type — except
    // when that result type is an unbound variable (a polymorphic `orderBy` key,
    // whose return is phantom), in which case any column is accepted and the
    // variable is bound to the column's type.
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
        ctx.quoted_lambdas_accum
            .insert(*span, QuoteInfo { param_name, entity });
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
/// column of `entity` — against the declared projection record `expected`.
///
/// Each field name is the output alias; its value must be a column of the
/// predicate parameter. The body's field set and column types must match
/// `expected`, so the captured projection cannot disagree with the type the
/// caller declared for it.
#[allow(clippy::too_many_arguments)]
fn check_projection(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    body: &Expr,
    param: &str,
    fields: &[(String, Type)],
    entity_name: &str,
    expected: &[(String, Type)],
    span: Span,
    entity: TyConId,
) -> bool {
    let mut body = body;
    while let Expr::Paren { inner, .. } = body {
        body = inner;
    }
    let Expr::RecordLit { fields: inits, .. } = body else {
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
                    "projection field `{0}` must be written `{0} = {param}.column`",
                    fi.name.text
                ),
                span: fi.span,
            });
            return false;
        };
        let Some(qk) = check_node(ctx, b, value, param, fields, entity_name) else {
            return false;
        };
        let QKind::Col(col_ty) = qk else {
            ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                detail: format!(
                    "projection field `{}` must be a column of `{param}`",
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

    ctx.quoted_lambdas_accum.insert(
        span,
        QuoteInfo {
            param_name: param.to_string(),
            entity,
        },
    );
    true
}

fn check_node(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    e: &Expr,
    param: &str,
    fields: &[(String, Type)],
    entity_name: &str,
) -> Option<QKind> {
    match e {
        Expr::Paren { inner, .. } => check_node(ctx, b, inner, param, fields, entity_name),

        Expr::Literal(lit) => Some(QKind::Scalar(literal_type(b, lit))),

        Expr::FieldAccess { base, field, span } => {
            if !matches!(base.as_ref(), Expr::Ident(id) if id.text == param) {
                ctx.errors.push(TypeError::QuoteUnsupportedExpr {
                    detail: format!(
                        "only columns of the predicate parameter `{param}` can be accessed"
                    ),
                    span: *span,
                });
                return None;
            }
            if let Some((_, ty)) = fields.iter().find(|(n, _)| n == &field.text) {
                Some(QKind::Col(ty.clone()))
            } else {
                let suggestions = ridge_resolve::suggest::suggest(
                    &field.text,
                    fields.iter().map(|(n, _)| n.clone()),
                );
                ctx.errors.push(TypeError::QuoteUnknownColumn {
                    entity: entity_name.to_string(),
                    column: field.text.clone(),
                    suggestions,
                    span: *span,
                });
                None
            }
        }

        Expr::Binary { op, lhs, rhs, span } => {
            check_binary(ctx, b, *op, lhs, rhs, *span, param, fields, entity_name)
        }

        Expr::Ident(id) => {
            let detail = if id.text == param {
                format!("the row `{param}` can only be used through a column access like `{param}.field`")
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

#[allow(clippy::too_many_arguments)]
fn check_binary(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
    param: &str,
    fields: &[(String, Type)],
    entity_name: &str,
) -> Option<QKind> {
    match op {
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            let l = check_node(ctx, b, lhs, param, fields, entity_name)?;
            let r = check_node(ctx, b, rhs, param, fields, entity_name)?;
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
            let l = check_node(ctx, b, lhs, param, fields, entity_name)?;
            let r = check_node(ctx, b, rhs, param, fields, entity_name)?;
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
