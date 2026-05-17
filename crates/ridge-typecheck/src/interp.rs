//! String interpolation typing: `ToText` closed-set resolution (T11).
//!
//! # Spec reference: §4.11 / D038
//!
//! In Ridge 0.1.0 only five built-in types may appear inside `${…}` holes:
//! `Int`, `Float`, `Bool`, `Text`, `Timestamp`.  Any other type produces a
//! `T012 ToTextNotDerivable` diagnostic.  The 0.2.0 path will replace the
//! closed-set constant below with a typeclass (`ToText`) lookup; the constant
//! is the intentional seam.
//!
//! # Absorbing rule
//!
//! If a sub-expression already resolved to `Type::Error` (meaning an earlier
//! `T###` was already emitted for it), this pass is silent — no `T012` is
//! stacked on top.

use ridge_ast::{InterpPart, Span};
use ridge_types::{BuiltinTyCons, TyConId, Type};

use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::infer::infer_expr;

// ── Closed set ────────────────────────────────────────────────────────────────

/// The closed set of `TyConId`s whose values can appear in string-interpolation
/// holes in Ridge 0.1.0 (D038).
///
/// 0.2.0 will replace this array with a typeclass lookup.  The constant is the
/// architectural seam — callers that need the set reference this function rather
/// than hardcoding the IDs inline.
#[must_use]
pub const fn to_text_tycons(b: &BuiltinTyCons) -> [TyConId; 5] {
    [b.int, b.float, b.bool, b.text, b.timestamp]
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Infer the type of an interpolated string `$"…"`.
///
/// Each `${expr}` hole is inferred and checked against the D038 closed set.
/// Literal text segments (`InterpPart::Text`) require no inference.
///
/// The result type is always `Text` regardless of the hole types (or errors).
pub fn infer_interp(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    parts: &[InterpPart],
    _span: Span,
) -> Type {
    let allowed = to_text_tycons(b);

    for part in parts {
        match part {
            // Literal text segment — no inference needed.
            InterpPart::Text { .. } => {}

            // Expression hole `${expr}`.
            InterpPart::Expr { expr, span } => {
                let t = infer_expr(ctx, b, expr);
                // Use deep_resolve to follow the full union-find chain. This
                // handles cases where the hole type was unified transitively
                // (e.g. via HOF parameter unification) to a closed-set type.
                let tr = ctx.deep_resolve(&t);
                match &tr {
                    // Bare type-constructor: check against the closed set.
                    Type::Con(id, _) if allowed.contains(id) => {}
                    // Error: absorb silently (no T012 pile-on).
                    // Free type variable — the hole type is not yet fully resolved.
                    // Defer: if the var gets unified to a non-ToText type later,
                    // T001 will report that mismatch. We do NOT fire T012 here
                    // because in correct programs with HOF lambdas (e.g.
                    // `List.fold (fn a b -> $"${a}${b}") ""`), `a` and `b` are
                    // constrained to Text AFTER the lambda body is inferred.
                    Type::Error | Type::Var(_) => {}
                    // Everything else is not in the ToText closed set.
                    other => {
                        ctx.errors.push(TypeError::ToTextNotDerivable {
                            ty: format!("{other}"),
                            span: *span,
                        });
                    }
                }
            }
        }
    }

    // The interpolated string always produces a Text value.
    Type::Con(b.text, vec![])
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Expr, Ident, InterpPart, Span};
    use ridge_types::{TyConArena, TyConDecl, TyConId, TyConKind};

    fn ds() -> Span {
        Span::point(0)
    }

    fn make_builtins() -> BuiltinTyCons {
        let mut arena = TyConArena::new();
        BuiltinTyCons::allocate(&mut arena)
    }

    /// Build an `Expr::Interp` with a single expression hole containing `expr`.
    fn single_hole(expr: Expr) -> Expr {
        Expr::Interp {
            parts: vec![InterpPart::Expr {
                expr: Box::new(expr),
                span: ds(),
            }],
            span: ds(),
        }
    }

    /// Build an `Expr::Ident` bound to a variable already in scope.
    fn ident(name: &str) -> Expr {
        Expr::Ident(Ident {
            text: name.to_string(),
            span: ds(),
        })
    }

    /// Push a variable binding of type `ty` into `ctx.env`.
    fn bind(ctx: &mut InferCtx, name: &str, ty: Type) {
        use ridge_types::Scheme;
        ctx.env.push_frame();
        ctx.env.bind(name.to_string(), Scheme::mono(ty));
    }

    // ─── T1: Int hole ────────────────────────────────────────────────────────

    /// Test 1 — `"x = ${x}"` where x : Int → Text, no errors.
    #[test]
    fn interp_int_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "x", Type::Con(b.int, vec![]));

        let parts = vec![
            InterpPart::Text {
                raw: "x = ".to_string(),
                span: ds(),
            },
            InterpPart::Expr {
                expr: Box::new(ident("x")),
                span: ds(),
            },
        ];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Int hole; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.text),
            "expected Text result; got {ty:?}"
        );
    }

    // ─── T2: Float hole ──────────────────────────────────────────────────────

    /// Test 2 — `"y = ${y}"` where y : Float → Text, no errors.
    #[test]
    fn interp_float_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "y", Type::Con(b.float, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("y")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Float hole; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T3: Bool hole ───────────────────────────────────────────────────────

    /// Test 3 — `"flag = ${flag}"` where flag : Bool → Text, no errors.
    #[test]
    fn interp_bool_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "flag", Type::Con(b.bool, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("flag")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Bool hole; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T4: Text hole ───────────────────────────────────────────────────────

    /// Test 4 — `"name = ${name}"` where name : Text → Text, no errors.
    #[test]
    fn interp_text_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "name", Type::Con(b.text, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("name")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Text hole; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T5: Timestamp hole ──────────────────────────────────────────────────

    /// Test 5 — `"at = ${ts}"` where ts : Timestamp → Text, no errors.
    #[test]
    fn interp_timestamp_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "ts", Type::Con(b.timestamp, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("ts")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Timestamp hole; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T6: User record fires T012 ──────────────────────────────────────────

    /// Test 6 — `"u = ${user}"` where user : User { name: Text, age: Int } → T012.
    #[test]
    fn interp_user_record_t012() {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        // Intern a minimal User record type.
        let user_id = arena.intern(TyConDecl {
            id: TyConId(0), // overwritten by intern
            name: "User".to_string(),
            arity: 0,
            kind: TyConKind::Record(ridge_types::RecordSchema::new(
                vec![],
                vec![
                    ridge_types::RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    ridge_types::RecordField {
                        name: "age".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                ],
            )),
            def_span: None,
        });

        let mut ctx = InferCtx::new();
        bind(&mut ctx, "user", Type::Con(user_id, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("user")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        assert_eq!(
            ctx.errors.len(),
            1,
            "expected exactly one T012; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(&ctx.errors[0], TypeError::ToTextNotDerivable { .. }),
            "expected T012 ToTextNotDerivable; got {:?}",
            ctx.errors[0]
        );
        // Result is still Text (errors don't change the return type).
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T7: List fires T012 ─────────────────────────────────────────────────

    /// Test 7 — `"l = ${l}"` where l : List Int → T012.
    #[test]
    fn interp_list_t012() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        // List Int = Con(list, [Con(int, [])])
        bind(
            &mut ctx,
            "l",
            Type::Con(b.list, vec![Type::Con(b.int, vec![])]),
        );

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("l")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        assert_eq!(
            ctx.errors.len(),
            1,
            "expected exactly one T012 for List; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(&ctx.errors[0], TypeError::ToTextNotDerivable { .. }),
            "expected T012; got {:?}",
            ctx.errors[0]
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T8: Type::Error absorbing — no T012 stacked ─────────────────────────

    /// Test 8 — when the hole expression already resolves to `Type::Error`,
    /// `T012` must NOT be emitted (absorbing rule).
    #[test]
    fn interp_error_absorbing() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // Simulate a hole that types as Error (e.g., an unknown identifier).
        // `Expr::Ident` for an unbound name → T001 + Type::Error.
        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("unknown_var_xyz")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        // There should be exactly one error (T001 from the unbound ident), NOT T012.
        let t012_count = ctx
            .errors
            .iter()
            .filter(|e| matches!(e, TypeError::ToTextNotDerivable { .. }))
            .count();
        assert_eq!(
            t012_count, 0,
            "T012 must not fire when sub-expr is Type::Error; errors: {:?}",
            ctx.errors
        );
        // Result is still Text.
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T9: No-hole interp — literal text only, types as Text ───────────────

    /// Test 9 — `$"hello world"` with only a `Text` segment (no `${…}` holes)
    /// types as `Text` with no errors.
    ///
    /// Applicable: the parser *does* emit `Expr::Interp` for zero-hole strings
    /// (the T3 note in the AST says "only Text variant is ever emitted in T3").
    #[test]
    fn interp_no_holes_just_text() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        let parts = vec![InterpPart::Text {
            raw: "hello world".to_string(),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T10: Multiple holes, both in closed set ──────────────────────────────

    /// Test 10 — `"a=${a}, b=${b}"` where a : Int, b : Float → Text, no errors.
    #[test]
    fn interp_multiple_parts_ok() {
        use ridge_types::Scheme;
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        ctx.env
            .bind("a".to_string(), Scheme::mono(Type::Con(b.int, vec![])));
        ctx.env.bind(
            "b_var".to_string(),
            Scheme::mono(Type::Con(b.float, vec![])),
        );

        let parts = vec![
            InterpPart::Text {
                raw: "a=".to_string(),
                span: ds(),
            },
            InterpPart::Expr {
                expr: Box::new(ident("a")),
                span: ds(),
            },
            InterpPart::Text {
                raw: ", b=".to_string(),
                span: ds(),
            },
            InterpPart::Expr {
                expr: Box::new(ident("b_var")),
                span: ds(),
            },
        ];
        let ty = infer_interp(&mut ctx, &b, &parts, ds());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for multi-part interp; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T11: infer_interp via infer_expr dispatch ────────────────────────────

    /// Test 11 — verify that `infer_expr` dispatches into `infer_interp`
    /// correctly: an `Expr::Interp` with a single Int hole types as Text.
    #[test]
    fn infer_expr_dispatches_interp() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "n", Type::Con(b.int, vec![]));

        let expr = single_hole(ident("n"));
        let ty = infer_expr(&mut ctx, &b, &expr);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors from infer_expr dispatch; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.text),
            "expected Text; got {ty:?}"
        );
        // Critically, no T999 InternalTypeError should be present.
        let t999_count = ctx
            .errors
            .iter()
            .filter(|e| matches!(e, TypeError::InternalTypeError { .. }))
            .count();
        assert_eq!(t999_count, 0, "T999 must not fire after T11 wiring");
    }
}
