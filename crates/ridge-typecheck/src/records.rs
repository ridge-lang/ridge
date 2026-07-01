//! Record construction, field-access, and `with`-update inference (T8).
//!
//! # Entry points
//!
//! - [`infer_record_construction`] — `Constructor { field = val, … }` (§4.8).
//! - [`infer_field_access`]        — `expr.field` (§4.8).
//! - [`infer_record_with`]         — `expr with { field = val, … }` (§4.8).
//!
//! # Design note
//!
//! Each function takes the [`RecordSchema`] and [`TyConId`] as explicit
//! parameters rather than looking them up via a `BindingMap`.  The full
//! pipeline wiring (T17) wraps these calls after doing the `BindingMap` lookup;
//! the unit tests below construct the schema directly.
//!
//! # Did-you-mean
//!
//! `T005 UnknownField` uses [`ridge_resolve::suggest::suggest`] with the
//! schema's field names as candidates (§7, upstream contract).

use ridge_ast::{FieldInit, FieldPattern, Ident, Span};
use ridge_types::{BuiltinTyCons, RecordSchema, TyConId, TyVid, Type};

use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::render::emit_internal;
use crate::unify::unify;

// ── Substitution helper ───────────────────────────────────────────────────────

/// Apply a param→fresh-var substitution to a type.
///
/// `params[i]` is the schema `TyVid`; `args[i]` is the fresh `Type::Var(TyVid)`
/// allocated during instantiation.  Every `Type::Var(v)` where `v` equals one
/// of the params is replaced by the corresponding arg.
fn subst_type(ty: &Type, params: &[TyVid], args: &[Type]) -> Type {
    match ty {
        Type::Var(v) => params
            .iter()
            .position(|p| p == v)
            .map_or_else(|| ty.clone(), |pos| args[pos].clone()),
        Type::Con(id, sub_args) => {
            let new_sub = sub_args
                .iter()
                .map(|a| subst_type(a, params, args))
                .collect();
            Type::Con(*id, new_sub)
        }
        Type::Tuple(ts) => {
            let new_ts = ts.iter().map(|t| subst_type(t, params, args)).collect();
            Type::Tuple(new_ts)
        }
        Type::Fn {
            params: ps,
            ret,
            caps,
        } => Type::Fn {
            params: ps.iter().map(|p| subst_type(p, params, args)).collect(),
            ret: Box::new(subst_type(ret, params, args)),
            caps: caps.clone(),
        },
        // Alias: walk body but preserve wrapper for diagnostic names.
        Type::Alias { name, body } => Type::Alias {
            name: *name,
            body: Box::new(subst_type(body, params, args)),
        },
        Type::Error => Type::Error,
        // Non-exhaustive wildcard: future Type variants returned as-is.
        _ => ty.clone(),
    }
}

// ── Did-you-mean helpers ──────────────────────────────────────────────────────

/// Generate "did you mean?" suggestions for a field name miss.
///
/// Uses `ridge_resolve::suggest::suggest` with the schema's field names as
/// candidates.  Per §7 upstream contract, `suggest` is `pub` in
/// `ridge_resolve::suggest`.
fn field_suggestions(field: &str, schema: &RecordSchema) -> Vec<String> {
    let candidates = schema.record_fields().iter().map(|f| f.name.clone());
    ridge_resolve::suggest::suggest(field, candidates)
}

// ── infer_record_construction ─────────────────────────────────────────────────

/// Infer the type of a record-construction expression (§4.8 construction rule).
///
/// # Parameters
///
/// - `ctx`       — mutable inference context.
/// - `b`         — built-in type-constructor handles.
/// - `schema`    — the `RecordSchema` for `owner_tycon` (already looked up by
///   the caller via the `BindingMap` / `TyConDecl` table).
/// - `owner_tycon` — the `TyConId` of the record type (e.g. `User`).
/// - `record_name` — the human-readable record type name (for diagnostics).
/// - `fields`    — the field initialisers from the AST node.
/// - `span`      — span of the whole construction expression.
///
/// # Returns
///
/// `Type::Con(owner_tycon, instantiated_args)` on success;
/// `Type::Error` if any field is missing, unknown, or type-mismatched.
pub fn infer_record_construction(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    schema: &RecordSchema,
    owner_tycon: TyConId,
    record_name: &str,
    fields: &[FieldInit],
    span: Span,
) -> Type {
    // Step 3: instantiate the schema's params with fresh TyVids.
    let params = schema.params.clone();
    let fresh_args: Vec<Type> = params
        .iter()
        .map(|_| Type::Var(ctx.fresh_tyvid()))
        .collect();

    let mut had_error = false;

    // Step 5 first pass: flag unknown fields.
    for fi in fields {
        let found = schema
            .record_fields()
            .iter()
            .any(|f| f.name == fi.name.text);
        if !found {
            let suggestions = field_suggestions(&fi.name.text, schema);
            ctx.errors.push(TypeError::UnknownField {
                record: record_name.to_string(),
                field: fi.name.text.clone(),
                suggestions,
                span: fi.span,
            });
            had_error = true;
        }
    }

    // Step 4: for each declared field, find matching init or report missing.
    for decl_field in schema.record_fields() {
        let field_ty_subst = subst_type(&decl_field.ty, &params, &fresh_args);

        match fields.iter().find(|fi| fi.name.text == decl_field.name) {
            None => {
                ctx.errors.push(TypeError::MissingField {
                    record: record_name.to_string(),
                    field: decl_field.name.clone(),
                    span,
                });
                had_error = true;
            }
            Some(fi) => {
                let value_ty = match &fi.value {
                    Some(val_expr) => crate::infer::infer_expr(ctx, b, val_expr),
                    // Shorthand field `{ age }` → look up `age` as a local.
                    None => {
                        if let Some(s) = ctx.env.lookup(&fi.name.text) {
                            let s = s.clone();
                            crate::instantiate::instantiate(ctx, &s)
                        } else {
                            had_error = true;
                            emit_internal(
                                ctx,
                                format!("shorthand field '{}' not in scope", fi.name.text),
                                fi.span,
                            )
                        }
                    }
                };
                if let Err(e) = unify(ctx, &value_ty, &field_ty_subst) {
                    // Attach the field init span to the unification error.
                    ctx.errors.push(attach_span(e, fi.span));
                    had_error = true;
                }
            }
        }
    }

    if had_error {
        // Return Error if any structural issue, but only for missing/unknown fields.
        // Type-mismatch errors return the nominal record type so callers can
        // continue type-checking.
    }

    Type::Con(owner_tycon, fresh_args)
}

// ── opaque-type field boundary ──────────────────────────────────────────────────

/// Whether `decl` is an `opaque` type whose fields are being reached from a
/// module other than the one that declares it.
///
/// Field-level access (`.field`) and `with`-updates of an opaque type are
/// confined to its defining module. Built-ins and anonymous records are never
/// opaque, so this is a cheap no-op for them. When the current module is unknown
/// (unit-test scaffolding that bypasses the per-module driver) the gate also
/// no-ops, since there is nothing to compare against.
fn opaque_field_violation(ctx: &InferCtx, decl: &ridge_types::TyConDecl) -> bool {
    decl.opaque && ctx.current_module_raw.is_some() && ctx.current_module_raw != decl.def_module_raw
}

// ── infer_field_access ────────────────────────────────────────────────────────

/// Infer the type of a field-access expression `base.field` (§4.8).
///
/// # Parameters
///
/// - `ctx`         — mutable inference context.
/// - `b`           — built-in type-constructor handles.
/// - `base_ty`     — the (already-inferred) type of the base expression.
/// - `field_name`  — the field being accessed.
/// - `field_span`  — span of the field access for diagnostics.
/// - `tycons`      — the type-constructor declarations (for schema lookup).
///
/// # Returns
///
/// The substituted field type on success; `Type::Error` on any error.
pub fn infer_field_access(
    ctx: &mut InferCtx,
    _b: &BuiltinTyCons,
    base_ty: &Type,
    field_name: &Ident,
    field_span: Span,
    tycons: &[ridge_types::TyConDecl],
) -> Type {
    let base_resolved = ctx.deep_resolve(base_ty);

    // Absorb: if the base is an unresolved type variable (e.g. a HOF callback
    // param constrained after body inference, or a Phase-7 stub type) or is
    // already Error, return Error silently.  T006 fires only for concrete
    // non-record types.
    if matches!(&base_resolved, Type::Var(_) | Type::Error) {
        return Type::Error;
    }

    // Structural record (anonymous / inline): the field set lives in the type,
    // so look it up directly — no schema, and no opaque check (anonymous records
    // are never opaque).
    if let Type::Record { fields, .. } = &base_resolved {
        if let Some((_, fty)) = fields.iter().find(|(l, _)| *l == field_name.text) {
            return fty.clone();
        }
        let suggestions = ridge_resolve::suggest::suggest(
            &field_name.text,
            fields.iter().map(|(l, _)| l.clone()),
        );
        ctx.errors.push(TypeError::UnknownField {
            record: format!("{base_resolved}"),
            field: field_name.text.clone(),
            suggestions,
            span: field_span,
        });
        return Type::Error;
    }

    if let Type::Con(tycon_id, args) = &base_resolved {
        let decl = tycons.get(tycon_id.0 as usize);
        if let Some(decl) = decl {
            if let ridge_types::TyConKind::Record(schema) = &decl.kind {
                // An opaque type hides its fields outside its defining module.
                if opaque_field_violation(ctx, decl) {
                    ctx.errors.push(TypeError::OpaqueFieldAccess {
                        record: decl.name.clone(),
                        field: field_name.text.clone(),
                        span: field_span,
                    });
                    return Type::Error;
                }
                // Find the field.
                let params = schema.params.clone();
                let field_entry = schema
                    .record_fields()
                    .iter()
                    .find(|f| f.name == field_name.text);
                if let Some(f) = field_entry {
                    return subst_type(&f.ty, &params, args);
                }
                let suggestions = field_suggestions(&field_name.text, schema);
                ctx.errors.push(TypeError::UnknownField {
                    record: decl.name.clone(),
                    field: field_name.text.clone(),
                    suggestions,
                    span: field_span,
                });
                return Type::Error;
            }
        }
    }
    // Non-record type, unknown TyConId, or non-Con base.
    ctx.errors.push(TypeError::WithOnNonRecord {
        ty: format!("{base_resolved}"),
        span: field_span,
    });
    Type::Error
}

// ── infer_record_with ─────────────────────────────────────────────────────────

/// Infer the type of a `with`-update expression `base with { field = val, … }`
/// (§4.8 `with` rule).
///
/// # Parameters
///
/// - `ctx`         — mutable inference context.
/// - `b`           — built-in type-constructor handles.
/// - `base_ty`     — the (already-inferred) type of the base expression.
/// - `fields`      — the field updates from the AST node.
/// - `span`        — span of the whole `with` expression.
/// - `tycons`      — type-constructor declarations (for schema lookup).
///
/// # Returns
///
/// The same record type as `base_ty` on success; `Type::Error` on error.
pub fn infer_record_with(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    base_ty: &Type,
    fields: &[FieldInit],
    span: Span,
    tycons: &[ridge_types::TyConDecl],
) -> Type {
    let base_resolved = ctx.deep_resolve(base_ty);

    // Absorb: free type vars and Error bases propagate silently.
    if matches!(&base_resolved, Type::Var(_) | Type::Error) {
        return Type::Error;
    }

    // Structural record: handled in its own helper (no schema, no opaque check).
    let structural_fields = if let Type::Record { fields, .. } = &base_resolved {
        Some(fields.clone())
    } else {
        None
    };
    if let Some(row_fields) = structural_fields {
        return infer_structural_with(ctx, b, base_resolved, &row_fields, fields);
    }

    let (tycon_id, args) = if let Type::Con(id, args) = &base_resolved {
        (*id, args.clone())
    } else {
        ctx.errors.push(TypeError::WithOnNonRecord {
            ty: format!("{base_resolved}"),
            span,
        });
        return Type::Error;
    };

    let decl = tycons.get(tycon_id.0 as usize);
    let Some(ridge_types::TyConKind::Record(schema)) = decl.map(|d| &d.kind) else {
        ctx.errors.push(TypeError::WithOnNonRecord {
            ty: format!("{base_resolved}"),
            span,
        });
        return Type::Error;
    };

    let record_name = decl.map_or("?", |d| d.name.as_str());
    let params = schema.params.clone();

    // An opaque type may only be field-updated inside its defining module.
    if let Some(d) = decl {
        if opaque_field_violation(ctx, d) {
            let field = fields
                .first()
                .map_or(String::new(), |fi| fi.name.text.clone());
            ctx.errors.push(TypeError::OpaqueFieldAccess {
                record: record_name.to_string(),
                field,
                span,
            });
            return Type::Error;
        }
    }

    // Step: flag unknown fields (extra fields not in schema).
    for fi in fields {
        let found = schema
            .record_fields()
            .iter()
            .any(|f| f.name == fi.name.text);
        if !found {
            let suggestions = field_suggestions(&fi.name.text, schema);
            ctx.errors.push(TypeError::UnknownField {
                record: record_name.to_string(),
                field: fi.name.text.clone(),
                suggestions,
                span: fi.span,
            });
        }
    }

    // Step: for each provided field-init, unify with schema field type.
    for fi in fields {
        if let Some(decl_field) = schema
            .record_fields()
            .iter()
            .find(|f| f.name == fi.name.text)
        {
            let field_ty_subst = subst_type(&decl_field.ty, &params, &args);
            let value_ty = match &fi.value {
                Some(val_expr) => crate::infer::infer_expr(ctx, b, val_expr),
                None => {
                    if let Some(s) = ctx.env.lookup(&fi.name.text) {
                        let s = s.clone();
                        crate::instantiate::instantiate(ctx, &s)
                    } else {
                        emit_internal(
                            ctx,
                            format!(
                                "shorthand field '{}' not in scope in with-update",
                                fi.name.text
                            ),
                            fi.span,
                        )
                    }
                }
            };
            if let Err(e) = unify(ctx, &value_ty, &field_ty_subst) {
                ctx.errors.push(attach_span(e, fi.span));
            }
        }
        // Unknown fields already reported above; skip inference for them.
    }

    // Result: same type as base.
    base_resolved
}

/// `with`-update over a structural record row (anonymous / inline). Each update
/// field must already be in the row; its value type is unified with the field's
/// type. Unknown fields are reported. Returns the base row unchanged.
fn infer_structural_with(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    base_resolved: Type,
    row_fields: &[(String, Type)],
    fields: &[FieldInit],
) -> Type {
    for fi in fields {
        let Some((_, field_ty)) = row_fields.iter().find(|(l, _)| *l == fi.name.text) else {
            let suggestions = ridge_resolve::suggest::suggest(
                &fi.name.text,
                row_fields.iter().map(|(l, _)| l.clone()),
            );
            ctx.errors.push(TypeError::UnknownField {
                record: format!("{base_resolved}"),
                field: fi.name.text.clone(),
                suggestions,
                span: fi.span,
            });
            continue;
        };
        let field_ty = field_ty.clone();
        let value_ty = match &fi.value {
            Some(val_expr) => crate::infer::infer_expr(ctx, b, val_expr),
            None => {
                if let Some(s) = ctx.env.lookup(&fi.name.text) {
                    let s = s.clone();
                    crate::instantiate::instantiate(ctx, &s)
                } else {
                    emit_internal(
                        ctx,
                        format!(
                            "shorthand field '{}' not in scope in with-update",
                            fi.name.text
                        ),
                        fi.span,
                    )
                }
            }
        };
        if let Err(e) = unify(ctx, &value_ty, &field_ty) {
            ctx.errors.push(attach_span(e, fi.span));
        }
    }
    base_resolved
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Public re-export of [`attach_span`] for use by `infer.rs` inline-record helpers.
///
/// Callers outside `records.rs` that need to attach a span to a `TypeError`
/// produced by `unify` (which emits dummy spans) can call this instead of
/// duplicating the match logic.
#[must_use]
pub fn attach_span_pub(e: TypeError, span: Span) -> TypeError {
    attach_span(e, span)
}

/// Attach a `Span` to a `TypeError` that was produced without a proper span
/// (typically from [`unify`]).
///
/// Replaces the dummy span in `T001 TypeMismatch` and `T010 OccursCheck`
/// variants; all other variants are returned unchanged.
fn attach_span(e: TypeError, span: Span) -> TypeError {
    match e {
        TypeError::TypeMismatch {
            expected, found, ..
        } => TypeError::TypeMismatch {
            expected,
            found,
            span,
        },
        TypeError::OccursCheck { var, ty, .. } => TypeError::OccursCheck { var, ty, span },
        TypeError::InsertShapeFullEntity {
            entity,
            companion,
            omitted,
            ..
        } => TypeError::InsertShapeFullEntity {
            entity,
            companion,
            omitted,
            span,
        },
        other => other,
    }
}

// ── Record-body pattern inference ───────────────────────────────────────────────

/// Type-check a record-body constructor pattern `Rec { f1 = p1, f2, .. }`.
///
/// Unifies the scrutinee with the record type, types each named field's
/// sub-pattern against that field's declared type (a shorthand `{ age }` binds
/// a new local of the field's type), reports unknown field names, and — unless a
/// trailing `..` is present (`has_rest`) — reports any omitted field as missing.
#[allow(clippy::too_many_arguments)]
pub fn infer_record_pattern(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    schema: &RecordSchema,
    owner_tycon: TyConId,
    record_name: &str,
    fields: &[FieldPattern],
    has_rest: bool,
    expected: &Type,
    span: Span,
) {
    // Instantiate the schema's params with fresh vars and unify the scrutinee.
    let params = schema.params.clone();
    let fresh_args: Vec<Type> = params
        .iter()
        .map(|_| Type::Var(ctx.fresh_tyvid()))
        .collect();
    let record_ty = Type::Con(owner_tycon, fresh_args.clone());
    if let Err(e) = unify(ctx, expected, &record_ty) {
        ctx.errors.push(attach_span(e, span));
    }

    // Type or bind each named field; flag unknown field names.
    for fp in fields {
        let field_ty = if let Some(f) = schema
            .record_fields()
            .iter()
            .find(|f| f.name == fp.name.text)
        {
            subst_type(&f.ty, &params, &fresh_args)
        } else {
            ctx.errors.push(TypeError::UnknownField {
                record: record_name.to_string(),
                field: fp.name.text.clone(),
                suggestions: field_suggestions(&fp.name.text, schema),
                span: fp.span,
            });
            Type::Error
        };
        match &fp.pattern {
            Some(sub) => crate::infer::infer_pattern(ctx, b, sub, &field_ty),
            // Shorthand `{ age }` binds a new local of the field's type.
            None => ctx.env.bind(
                fp.name.text.clone(),
                crate::instantiate::monoscheme(field_ty),
            ),
        }
    }

    // Without `..`, every declared field must be named.
    if !has_rest {
        for decl_field in schema.record_fields() {
            if !fields.iter().any(|fp| fp.name.text == decl_field.name) {
                ctx.errors.push(TypeError::MissingField {
                    record: record_name.to_string(),
                    field: decl_field.name.clone(),
                    span,
                });
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Ident, Span};
    use ridge_types::{
        BuiltinTyCons, RecordField, RecordSchema, TyConArena, TyConDecl, TyConId, TyConKind, TyVid,
        Type,
    };

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn ds() -> Span {
        Span::point(0)
    }

    fn id(text: &str) -> Ident {
        Ident {
            text: text.to_string(),
            span: ds(),
        }
    }

    fn fi(name: &str, value: Option<ridge_ast::Expr>) -> FieldInit {
        FieldInit {
            name: id(name),
            value,
            span: ds(),
        }
    }

    fn int_lit(raw: &str) -> ridge_ast::Expr {
        ridge_ast::Expr::Literal(ridge_ast::Literal::IntDec {
            raw: raw.to_string(),
            span: ds(),
        })
    }

    fn text_lit(raw: &str) -> ridge_ast::Expr {
        ridge_ast::Expr::Literal(ridge_ast::Literal::Text {
            raw: format!("\"{raw}\""),
            span: ds(),
        })
    }

    /// Build a `RecordSchema` with named fields of given types.
    fn make_schema(fields: &[(&str, Type)]) -> RecordSchema {
        RecordSchema::new(
            vec![],
            fields
                .iter()
                .map(|(n, t)| RecordField {
                    name: (*n).to_string(),
                    ty: t.clone(),
                })
                .collect(),
        )
    }

    /// Build a `TyConArena` + `BuiltinTyCons`, then intern a record `TyCon`.
    /// Returns (arena, builtins, `user_tycon_id`).
    fn make_arena_with_record(
        name: &str,
        schema: RecordSchema,
    ) -> (TyConArena, BuiltinTyCons, TyConId) {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let tycon_id = arena.intern(TyConDecl {
            id: TyConId(0), // will be overwritten by intern
            name: name.to_string(),
            #[expect(
                clippy::cast_possible_truncation,
                reason = "schema params count fits u32"
            )]
            arity: schema.params.len() as u32,
            kind: TyConKind::Record(schema),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        (arena, b, tycon_id)
    }

    // ── Test 1: bare ctor happy path ──────────────────────────────────────────
    // `User { name = "x", age = 30 }` types as `User`

    #[test]
    fn t1_bare_ctor_happy_path_user() {
        let schema = make_schema(&[
            ("name", Type::Con(TyConId(3), vec![])), // Text placeholder
            ("age", Type::Con(TyConId(0), vec![])),  // Int placeholder
        ]);
        let (arena, b, user_id) = make_arena_with_record("User", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let fields = vec![
            fi("name", Some(text_lit("x"))),
            fi("age", Some(int_lit("30"))),
        ];

        let ty = infer_record_construction(&mut ctx, &b, &schema, user_id, "User", &fields, ds());
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(ty, Type::Con(id, _) if id == user_id),
            "expected Type::Con(User, ..), got {ty:?}"
        );
        // Ensure arena is used (suppress unused warning)
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    // ── Test 2: qualified ctor happy path ─────────────────────────────────────
    // `Http.Response { status = 200, ... }` — schema-level test
    // (Qualified resolution via BindingMap is wired in T17;
    //  here we test that the schema-level inference works for any TyConId.)

    #[test]
    fn t2_qualified_ctor_happy_path_response() {
        let schema = make_schema(&[
            ("status", Type::Con(TyConId(0), vec![])), // Int placeholder
        ]);
        let (arena, b, resp_id) = make_arena_with_record("Response", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let fields = vec![fi("status", Some(int_lit("200")))];
        let ty =
            infer_record_construction(&mut ctx, &b, &schema, resp_id, "Response", &fields, ds());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(ty, Type::Con(id, _) if id == resp_id),
            "expected Type::Con(Response, ..), got {ty:?}"
        );
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    // ── Test 3: missing field → T004 ─────────────────────────────────────────

    #[test]
    fn t3_missing_field_emits_t004() {
        let schema = make_schema(&[
            ("name", Type::Con(TyConId(3), vec![])),
            ("age", Type::Con(TyConId(0), vec![])),
        ]);
        let (arena, b, user_id) = make_arena_with_record("User", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Only provide `name`, omit `age`.
        let fields = vec![fi("name", Some(text_lit("Alice")))];
        let _ = infer_record_construction(&mut ctx, &b, &schema, user_id, "User", &fields, ds());

        let t004 = ctx.errors.iter().any(|e| e.code() == "T004");
        assert!(t004, "expected T004 MissingField; got {:?}", ctx.errors);
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    // ── Test 4: extra/unknown field → T005 with did-you-mean ─────────────────

    #[test]
    fn t4_unknown_field_emits_t005_with_suggestion() {
        let schema = make_schema(&[("name", Type::Con(TyConId(3), vec![]))]);
        let (arena, b, user_id) = make_arena_with_record("User", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Provide `nme` (typo of `name`) plus the required `name`.
        let fields = vec![
            fi("name", Some(text_lit("Alice"))),
            fi("nme", Some(text_lit("oops"))),
        ];
        let _ = infer_record_construction(&mut ctx, &b, &schema, user_id, "User", &fields, ds());

        let t005 = ctx.errors.iter().any(|e| e.code() == "T005");
        assert!(t005, "expected T005 UnknownField; got {:?}", ctx.errors);

        // Check did-you-mean includes "name".
        let has_suggestion = ctx.errors.iter().any(|e| {
            if let TypeError::UnknownField { suggestions, .. } = e {
                suggestions.iter().any(|s| s == "name")
            } else {
                false
            }
        });
        assert!(
            has_suggestion,
            "expected 'name' in suggestions; errors: {:?}",
            ctx.errors
        );
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    // ── Test 5: field-init type mismatch → T001 ───────────────────────────────

    #[test]
    fn t5_field_type_mismatch_emits_t001() {
        // `age` is declared as Int but supplied as Text.
        let schema = make_schema(&[("age", Type::Con(TyConId(0), vec![]))]);
        let (arena, b, user_id) = make_arena_with_record("User", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Supply `age = "not-a-number"` (Text, not Int).
        let fields = vec![fi("age", Some(text_lit("bad")))];
        let _ = infer_record_construction(&mut ctx, &b, &schema, user_id, "User", &fields, ds());

        let t001 = ctx.errors.iter().any(|e| e.code() == "T001");
        assert!(t001, "expected T001 TypeMismatch; got {:?}", ctx.errors);
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    // ── Test 6: field access happy path ──────────────────────────────────────
    // `u.name` where u: User { name: Text } → Text

    #[test]
    fn t6_field_access_happy_path() {
        let schema = make_schema(&[("name", Type::Con(TyConId(3), vec![]))]);
        let (arena, b, user_id) = make_arena_with_record("User", schema);
        let mut ctx = InferCtx::new();

        let base_ty = Type::Con(user_id, vec![]);
        let field = id("name");
        let ty = infer_field_access(&mut ctx, &b, &base_ty, &field, ds(), arena.all());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got {:?}",
            ctx.errors
        );
        // Should be Text (TyConId(3) in our placeholder schema).
        assert!(
            matches!(ty, Type::Con(TyConId(3), _)),
            "expected Text (TyConId 3), got {ty:?}"
        );
    }

    // ── Test 7: field access unknown field → T005 ─────────────────────────────

    #[test]
    fn t7_field_access_unknown_field_emits_t005() {
        let schema = make_schema(&[("name", Type::Con(TyConId(3), vec![]))]);
        let (arena, b, user_id) = make_arena_with_record("User", schema);
        let mut ctx = InferCtx::new();

        let base_ty = Type::Con(user_id, vec![]);
        let field = id("nme"); // typo
        let _ = infer_field_access(&mut ctx, &b, &base_ty, &field, ds(), arena.all());

        let t005 = ctx.errors.iter().any(|e| e.code() == "T005");
        assert!(t005, "expected T005 UnknownField; got {:?}", ctx.errors);
    }

    // ── Test 8: field access on non-record → T006 ─────────────────────────────

    #[test]
    fn t8_field_access_on_non_record_emits_t006() {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let mut ctx = InferCtx::new();

        // Use Int as the base type — definitely not a record.
        let base_ty = Type::Con(b.int, vec![]);
        let field = id("name");
        let _ = infer_field_access(&mut ctx, &b, &base_ty, &field, ds(), arena.all());

        let t006 = ctx.errors.iter().any(|e| e.code() == "T006");
        assert!(t006, "expected T006 WithOnNonRecord; got {:?}", ctx.errors);
    }

    // ── Test 9: with-update happy path ────────────────────────────────────────
    // `u with { age = 31 }` → same record type

    #[test]
    fn t9_with_update_happy_path() {
        let schema = make_schema(&[
            ("name", Type::Con(TyConId(3), vec![])),
            ("age", Type::Con(TyConId(0), vec![])),
        ]);
        let (arena, b, user_id) = make_arena_with_record("User", schema);
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let base_ty = Type::Con(user_id, vec![]);
        let fields = vec![fi("age", Some(int_lit("31")))];
        let ty = infer_record_with(&mut ctx, &b, &base_ty, &fields, ds(), arena.all());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(ty, Type::Con(id, _) if id == user_id),
            "expected same User type, got {ty:?}"
        );
        ctx.env.pop_frame();
    }

    // ── Test 10: with-update on non-record → T006 ────────────────────────────

    #[test]
    fn t10_with_update_on_non_record_emits_t006() {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Use Int as base — not a record.
        let base_ty = Type::Con(b.int, vec![]);
        let fields = vec![fi("age", Some(int_lit("1")))];
        let _ = infer_record_with(&mut ctx, &b, &base_ty, &fields, ds(), arena.all());

        let t006 = ctx.errors.iter().any(|e| e.code() == "T006");
        assert!(t006, "expected T006 WithOnNonRecord; got {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Test 11: with-update unknown field → T005 ─────────────────────────────

    #[test]
    fn t11_with_update_unknown_field_emits_t005() {
        let schema = make_schema(&[("age", Type::Con(TyConId(0), vec![]))]);
        let (arena, b, user_id) = make_arena_with_record("User", schema);
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let base_ty = Type::Con(user_id, vec![]);
        // `nme` is not a field of User.
        let fields = vec![fi("nme", Some(text_lit("bad")))];
        let _ = infer_record_with(&mut ctx, &b, &base_ty, &fields, ds(), arena.all());

        let t005 = ctx.errors.iter().any(|e| e.code() == "T005");
        assert!(t005, "expected T005 UnknownField; got {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Test 12: record type with params — instantiation ────────────────────

    #[test]
    fn t12_record_with_type_params_instantiated() {
        // type Box a = { value: a }
        // Box { value = 42 } → Box Int (fresh var resolved to Int)
        let a = TyVid(0);
        let schema = RecordSchema::new(
            vec![a],
            vec![RecordField {
                name: "value".to_string(),
                ty: Type::Var(a),
            }],
        );
        let (arena, b, box_id) = make_arena_with_record("Box", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let fields = vec![fi("value", Some(int_lit("42")))];
        let ty = infer_record_construction(&mut ctx, &b, &schema, box_id, "Box", &fields, ds());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got {:?}",
            ctx.errors
        );
        // Should be Type::Con(box_id, [Type::Var(fresh_v)])
        // and after deep_resolve the fresh var should be Int.
        if let Type::Con(id, args) = &ty {
            assert_eq!(*id, box_id);
            assert_eq!(args.len(), 1);
            let resolved = ctx.deep_resolve(&args[0]);
            assert!(
                matches!(resolved, Type::Con(iid, _) if iid == b.int),
                "expected fresh var to resolve to Int, got {resolved:?}"
            );
        } else {
            panic!("expected Type::Con(Box, [..]), got {ty:?}");
        }
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    // ── Record-body pattern inference ────────────────────────────────────────

    fn fp_short(name: &str) -> FieldPattern {
        FieldPattern {
            name: id(name),
            pattern: None,
            span: ds(),
        }
    }

    /// `User { name, .. }` — shorthand binds `name`; `..` allows omitting `age`.
    #[test]
    fn record_pattern_rest_binds_named_field() {
        let schema = make_schema(&[
            ("name", Type::Con(TyConId(3), vec![])),
            ("age", Type::Con(TyConId(0), vec![])),
        ]);
        let (arena, b, user_id) = make_arena_with_record("User", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let fields = vec![fp_short("name")];
        let expected = Type::Var(ctx.fresh_tyvid());
        infer_record_pattern(
            &mut ctx,
            &b,
            &schema,
            user_id,
            "User",
            &fields,
            true,
            &expected,
            ds(),
        );
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got {:?}",
            ctx.errors
        );
        assert!(
            ctx.env.lookup("name").is_some(),
            "shorthand `name` must be bound"
        );
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    /// `User { name, age }` — all fields named, no `..` → exhaustive, no errors.
    #[test]
    fn record_pattern_all_fields_no_rest_ok() {
        let schema = make_schema(&[
            ("name", Type::Con(TyConId(3), vec![])),
            ("age", Type::Con(TyConId(0), vec![])),
        ]);
        let (arena, b, user_id) = make_arena_with_record("User", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let fields = vec![fp_short("name"), fp_short("age")];
        let expected = Type::Var(ctx.fresh_tyvid());
        infer_record_pattern(
            &mut ctx,
            &b,
            &schema,
            user_id,
            "User",
            &fields,
            false,
            &expected,
            ds(),
        );
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got {:?}",
            ctx.errors
        );
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    /// `User { nope, .. }` — unknown field name reported.
    #[test]
    fn record_pattern_unknown_field_reported() {
        let schema = make_schema(&[("name", Type::Con(TyConId(3), vec![]))]);
        let (arena, b, user_id) = make_arena_with_record("User", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let fields = vec![fp_short("nope")];
        let expected = Type::Var(ctx.fresh_tyvid());
        infer_record_pattern(
            &mut ctx,
            &b,
            &schema,
            user_id,
            "User",
            &fields,
            true,
            &expected,
            ds(),
        );
        assert!(
            ctx.errors
                .iter()
                .any(|e| matches!(e, TypeError::UnknownField { .. })),
            "expected UnknownField; got {:?}",
            ctx.errors
        );
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    /// `User { name }` without `..` on a 2-field record → missing `age`.
    #[test]
    fn record_pattern_missing_field_without_rest() {
        let schema = make_schema(&[
            ("name", Type::Con(TyConId(3), vec![])),
            ("age", Type::Con(TyConId(0), vec![])),
        ]);
        let (arena, b, user_id) = make_arena_with_record("User", schema.clone());
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let fields = vec![fp_short("name")];
        let expected = Type::Var(ctx.fresh_tyvid());
        infer_record_pattern(
            &mut ctx,
            &b,
            &schema,
            user_id,
            "User",
            &fields,
            false,
            &expected,
            ds(),
        );
        assert!(
            ctx.errors
                .iter()
                .any(|e| matches!(e, TypeError::MissingField { .. })),
            "expected MissingField for `age`; got {:?}",
            ctx.errors
        );
        let _ = arena.len();
        ctx.env.pop_frame();
    }

    // ── Structural records (anonymous / inline) — field access & with (R3) ────

    #[test]
    fn structural_field_access_returns_field_type() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let rec = Type::record(
            vec![
                ("name".to_string(), Type::Con(b.text, vec![])),
                ("age".to_string(), Type::Con(b.int, vec![])),
            ],
            ridge_types::RowTail::Closed,
        );
        let ty = infer_field_access(&mut ctx, &b, &rec, &id("age"), ds(), &[]);
        assert!(matches!(ty, Type::Con(tc, _) if tc == b.int), "got {ty:?}");
        assert!(ctx.errors.is_empty(), "{:?}", ctx.errors);
    }

    #[test]
    fn structural_field_access_unknown_field_errors() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let rec = Type::record(
            vec![("name".to_string(), Type::Con(b.text, vec![]))],
            ridge_types::RowTail::Closed,
        );
        let ty = infer_field_access(&mut ctx, &b, &rec, &id("missing"), ds(), &[]);
        assert!(ty.is_error());
        assert_eq!(ctx.errors.len(), 1);
        assert_eq!(ctx.errors[0].code(), "T005");
    }

    #[test]
    fn structural_with_present_field_unifies_and_returns_base() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let rec = Type::record(
            vec![("count".to_string(), Type::Con(b.int, vec![]))],
            ridge_types::RowTail::Closed,
        );
        let updates = vec![fi("count", Some(int_lit("5")))];
        let ty = infer_record_with(&mut ctx, &b, &rec, &updates, ds(), &[]);
        assert!(matches!(ty, Type::Record { .. }), "got {ty:?}");
        assert!(ctx.errors.is_empty(), "{:?}", ctx.errors);
    }

    #[test]
    fn structural_with_unknown_field_errors() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let rec = Type::record(
            vec![("count".to_string(), Type::Con(b.int, vec![]))],
            ridge_types::RowTail::Closed,
        );
        let updates = vec![fi("nope", Some(int_lit("1")))];
        let _ = infer_record_with(&mut ctx, &b, &rec, &updates, ds(), &[]);
        assert!(
            ctx.errors.iter().any(|e| e.code() == "T005"),
            "{:?}",
            ctx.errors
        );
    }
}
