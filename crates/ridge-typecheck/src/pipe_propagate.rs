//! Typing for `|>` (pipe), `?` (propagate), and `try` blocks (T10).
//!
//! # Pipe `|>` (§4.10, D024)
//!
//! `a |> f` is sugar for `f a`.  If `rhs` is a `Call { callee, args }` the `lhs`
//! is appended as the *last* argument (data-last convention, D024).  Otherwise
//! `rhs` is treated as the callee with `lhs` as its sole argument.
//!
//! Implementation choice: **(A) clone-and-dispatch**.  We build a synthetic
//! `Expr::Call` from the desugared shape and dispatch into `infer_expr`, which
//! already covers all call-inference logic.  Clone cost is O(sub-tree size) but
//! acceptable for the small trees typical in Ridge source.
//!
//! # Propagate `?` (§4.10, D039)
//!
//! Inside a `Result a e`-returning context the inner expression must unify with
//! `Result alpha beta`, and the beta (error type) must unify with the enclosing
//! context's error type.  The `?` expression's own type is `alpha` (the Ok
//! payload).  Inside an `Option a` context, the inner expression must unify with
//! `Option alpha`.  Outside either context → `T021`.
//!
//! The propagation context is `ctx.current_propagate_target` (set by `try` blocks)
//! falling back to `ctx.current_fn_ret` (set by function-body entry in T6).
//!
//! # Try (§4.10, D060)
//!
//! `try { … }` introduces a fresh `Result alpha beta` propagation context.  `?`
//! operators inside the block propagate into this shape rather than the enclosing
//! function's return type.  The expression's type is `Result alpha beta` where
//! `alpha` is unified with the block's final-expression type.
//!
//! Default shape: `Result a e` (always — spec D060 does not distinguish a
//! try-as-Option form; if the body contains Option `?` the unifier will produce
//! a T001).
//!
//! # T022 `DiscardedResult` (§4.10 final paragraph)
//!
//! Type-based: fires at the end of each non-last statement in `infer_block` when
//! the statement's type is not `Unit` and not `Type::Error` and the statement is
//! not a `Let` / `Var` binding (those consume the value into a binding).
//! `Send` returns `Unit` and is naturally exempt.  `print`/`Io.println` return
//! `Unit` and are naturally exempt.

use ridge_ast::{Block, Expr, Span};
use ridge_types::{BuiltinTyCons, Type};

use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::unify::unify;

// ── Pipe `|>` ─────────────────────────────────────────────────────────────────

/// Infers the type of `lhs |> rhs` by desugaring to a function call.
///
/// Desugaring rules:
/// - `rhs` is `Call { callee, args }` → desugar to `Call { callee, args ++ [lhs] }`.
/// - `rhs` is any other expression → desugar to `Call { callee: rhs, args: [lhs] }`.
///
/// Implementation choice (A): clone the sub-expressions to build a synthetic
/// `Expr::Call` and dispatch through `infer_expr`.
pub fn infer_pipe(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
) -> Type {
    use crate::infer::infer_expr;

    // When `rhs` is `Expr::Propagate { inner: fn_expr }`, the Pratt parser
    // fired `?` on just `fn_expr` before the `|>` could claim it.
    // E.g. `xs |> List.head ?` parses as `Pipe(xs, Propagate(List.head))`.
    // The correct semantics is `Propagate(Pipe(xs, List.head))`: apply the
    // pipe FIRST, then propagate.  We handle this by re-threading: build
    // a synthetic `Pipe(lhs, inner)` and wrap it in `Propagate`.
    if let Expr::Propagate {
        inner: prop_inner,
        span: prop_span,
    } = rhs
    {
        let pipe_before_prop = Expr::Pipe {
            lhs: Box::new(lhs.clone()),
            rhs: prop_inner.clone(),
            span,
        };
        let wrap = Expr::Propagate {
            inner: Box::new(pipe_before_prop),
            span: *prop_span,
        };
        return infer_expr(ctx, b, &wrap);
    }

    let synthetic_call = match rhs {
        Expr::Call {
            callee,
            args,
            span: call_span,
        } => {
            // Append lhs as the last argument — data-last convention (D024).
            let mut new_args = args.clone();
            new_args.push(lhs.clone());
            Expr::Call {
                callee: callee.clone(),
                args: new_args,
                span: *call_span,
            }
        }
        // rhs is a bare callable — treat as single-arg call.
        other => Expr::Call {
            callee: Box::new(other.clone()),
            args: vec![lhs.clone()],
            span,
        },
    };

    infer_expr(ctx, b, &synthetic_call)
}

// ── Propagate `?` ─────────────────────────────────────────────────────────────

/// Infers the type of `inner?`.
///
/// Looks up the enclosing propagation context:
/// 1. `ctx.current_propagate_target` — set by an enclosing `try` block.
/// 2. `ctx.current_fn_ret` — the enclosing function's declared return type.
///
/// If the target is `Result a e`: unify `inner` with `Result alpha beta`, unify
/// `beta` with the context's error type, and return `alpha`.
/// If the target is `Option a`: unify `inner` with `Option alpha` and return
/// `alpha`.
/// Otherwise: emit `T021 PropagateOutsideResultOrOption` and return
/// `Type::Error`.
pub fn infer_propagate(ctx: &mut InferCtx, b: &BuiltinTyCons, inner: &Expr, span: Span) -> Type {
    use crate::infer::infer_expr;

    let inner_ty = infer_expr(ctx, b, inner);

    // Determine propagation target.
    let target = ctx
        .current_propagate_target
        .clone()
        .or_else(|| ctx.current_fn_ret.clone());

    if let Some(target_ty) = target {
        let resolved_target = ctx.shallow_resolve(&target_ty);
        match &resolved_target {
            Type::Con(id, args) if *id == b.result => {
                // Context is Result a e — inner must be Result alpha beta,
                // and beta must unify with the context's error type (args[1]).
                let alpha = Type::Var(ctx.fresh_tyvid());
                let beta = Type::Var(ctx.fresh_tyvid());
                let expected_inner = Type::Con(b.result, vec![alpha.clone(), beta.clone()]);

                match unify(ctx, &inner_ty, &expected_inner) {
                    Ok(()) => {
                        // Unify the error slots.
                        let err_type = args.get(1).cloned().unwrap_or(Type::Error);
                        if let Err(e) = unify(ctx, &beta, &err_type) {
                            ctx.errors.push(attach_span(e, span));
                            Type::Error
                        } else {
                            ctx.shallow_resolve(&alpha)
                        }
                    }
                    Err(e) => {
                        // Inner does not unify with Result _ _ — regular
                        // T001 type mismatch (not T021; the context IS a
                        // Result but the inner type is wrong).
                        ctx.errors.push(attach_span(e, span));
                        Type::Error
                    }
                }
            }
            Type::Con(id, _) if *id == b.option => {
                // Context is Option a — inner must be Option alpha.
                let alpha = Type::Var(ctx.fresh_tyvid());
                let expected_inner = Type::Con(b.option, vec![alpha.clone()]);

                match unify(ctx, &inner_ty, &expected_inner) {
                    Ok(()) => ctx.shallow_resolve(&alpha),
                    Err(e) => {
                        ctx.errors.push(attach_span(e, span));
                        Type::Error
                    }
                }
            }
            other => {
                // Target is neither Result nor Option — T021.
                ctx.errors.push(TypeError::PropagateOutsideResultOrOption {
                    found_ty: format!("{inner_ty:?}"),
                    expected: format!("{other:?}"),
                    span,
                });
                Type::Error
            }
        }
    } else {
        // No enclosing fn return type or try block — T021.
        ctx.errors.push(TypeError::PropagateOutsideResultOrOption {
            found_ty: format!("{inner_ty:?}"),
            expected: "no enclosing Result/Option context".to_string(),
            span,
        });
        Type::Error
    }
}

// ── Try block ─────────────────────────────────────────────────────────────────

/// Infers the type of `try { block }`.
///
/// Introduces a fresh `Result alpha beta` as the propagation target for the
/// block's body.  Any `?` inside the block propagates into this type rather
/// than the enclosing function's return type.
///
/// The try expression's type is `Result alpha beta` where `alpha` is unified
/// with the block's final expression type.
///
/// Default shape is always `Result a e` (D060 does not define an Option-try
/// form; an Option `?` inside a Result-try context will produce a T001).
pub fn infer_try(ctx: &mut InferCtx, b: &BuiltinTyCons, block: &Block, span: Span) -> Type {
    use crate::infer::infer_block;

    // Allocate the try expression's Ok and Err type variables.
    let alpha = Type::Var(ctx.fresh_tyvid());
    let beta = Type::Var(ctx.fresh_tyvid());
    let try_result_ty = Type::Con(b.result, vec![alpha.clone(), beta]);

    // Save and override the propagation context for the block body.
    let saved = ctx.current_propagate_target.take();
    ctx.current_propagate_target = Some(try_result_ty.clone());

    let block_ty = infer_block(ctx, b, block);

    // Restore the outer propagation context.
    ctx.current_propagate_target = saved;

    // The block's final expression type is the Ok payload.
    if let Err(e) = unify(ctx, &block_ty, &alpha) {
        ctx.errors.push(attach_span(e, span));
    }

    // Return the fully-resolved try type.
    ctx.shallow_resolve(&try_result_ty)
}

// ── T022 DiscardedResult helpers ──────────────────────────────────────────────

/// Returns `true` if `ty` is `Unit` (the absorbing Unit type-constructor).
#[must_use]
pub fn is_unit_type(ty: &Type, unit_id: ridge_types::TyConId) -> bool {
    matches!(ty, Type::Con(id, _) if *id == unit_id)
}

/// Returns `true` if the statement expression is a `Let` or `Var` binding.
///
/// Let/Var bindings consume their value into a name; the value is not
/// "discarded" in the `DiscardedResult` sense.
#[must_use]
pub const fn is_binding_stmt(stmt: &Expr) -> bool {
    matches!(stmt, Expr::Let { .. } | Expr::Var { .. })
}

/// Emits `T022 DiscardedResult` if the statement's inferred type is non-Unit
/// and the statement is not a binding (`let`/`var`).
///
/// Call this for every non-last statement in `infer_block` after inferring its
/// type.  The last statement is the block's value and must not trigger T022.
pub fn check_discarded_result(ctx: &mut InferCtx, b: &BuiltinTyCons, stmt: &Expr, stmt_ty: &Type) {
    if is_binding_stmt(stmt) {
        return;
    }
    let resolved = ctx.deep_resolve(stmt_ty);
    if matches!(resolved, Type::Error) {
        return;
    }
    if is_unit_type(&resolved, b.unit) {
        return;
    }
    ctx.errors.push(TypeError::DiscardedResult {
        ty: format!("{resolved:?}"),
        span: stmt.span(),
    });
}

// ── Span helper ───────────────────────────────────────────────────────────────

/// Attaches a source span to a `TypeError` (mirrors the one in `infer.rs`).
fn attach_span(err: TypeError, span: Span) -> TypeError {
    match err {
        TypeError::TypeMismatch {
            expected, found, ..
        } => TypeError::TypeMismatch {
            expected,
            found,
            span,
        },
        TypeError::ArityMismatch {
            callee,
            expected,
            found,
            hint,
            ..
        } => TypeError::ArityMismatch {
            callee,
            expected,
            found,
            span,
            hint,
        },
        TypeError::OccursCheck { var, ty, .. } => TypeError::OccursCheck { var, ty, span },
        other => other,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infer::infer_block;
    use crate::instantiate::monoscheme;
    use ridge_ast::{Ident, Literal, Span};
    use ridge_types::{BuiltinTyCons, TyConArena};

    fn dummy_span() -> Span {
        Span::point(0)
    }

    fn make_ident(text: &str) -> Ident {
        Ident {
            text: text.to_string(),
            span: dummy_span(),
        }
    }

    fn make_builtins() -> BuiltinTyCons {
        let mut arena = TyConArena::new();
        BuiltinTyCons::allocate(&mut arena)
    }

    fn int_lit(n: &str) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.to_string(),
            span: dummy_span(),
        })
    }

    fn text_lit(s: &str) -> Expr {
        Expr::Literal(Literal::Text {
            raw: s.to_string(),
            span: dummy_span(),
        })
    }

    fn ident(name: &str) -> Expr {
        Expr::Ident(make_ident(name))
    }

    /// Bind a name in the context with a given type (mono-scheme).
    fn bind(ctx: &mut InferCtx, name: &str, ty: Type) {
        if ctx.env.frames.is_empty() {
            ctx.env.push_frame();
        }
        ctx.env.bind(name.to_string(), monoscheme(ty));
    }

    /// Build a simple `Fn(params) -> ret` type with pure caps.
    fn make_fn_ty(_b: &BuiltinTyCons, params: Vec<Type>, ret: Type) -> Type {
        use ridge_types::{CapRow, CapabilitySet};
        Type::Fn {
            params,
            ret: Box::new(ret),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        }
    }

    /// Build `Result a e` from two concrete types.
    fn result_ty(b: &BuiltinTyCons, a: Type, e: Type) -> Type {
        Type::Con(b.result, vec![a, e])
    }

    /// Build `Option a` from a concrete type.
    fn option_ty(b: &BuiltinTyCons, a: Type) -> Type {
        Type::Con(b.option, vec![a])
    }

    fn int_ty(b: &BuiltinTyCons) -> Type {
        Type::Con(b.int, vec![])
    }

    fn text_ty(b: &BuiltinTyCons) -> Type {
        Type::Con(b.text, vec![])
    }

    fn unit_ty(b: &BuiltinTyCons) -> Type {
        Type::Con(b.unit, vec![])
    }

    // ── Test 1: pipe_basic ────────────────────────────────────────────────────

    /// `[1, 2, 3] |> List.length` desugars to `List.length [1, 2, 3]` → `Int`.
    ///
    /// We simulate this by binding `listLength` in the env as `List Int -> Int`
    /// and piping `[1,2,3]` into it.
    #[test]
    fn pipe_basic() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        let list_int = Type::Con(b.list, vec![int_ty(&b)]);
        let length_fn = make_fn_ty(&b, vec![list_int], int_ty(&b));
        bind(&mut ctx, "listLength", length_fn);

        let lhs = Expr::List {
            elems: vec![int_lit("1"), int_lit("2"), int_lit("3")],
            span: dummy_span(),
        };
        let rhs = ident("listLength");

        let ty = infer_pipe(&mut ctx, &b, &lhs, &rhs, dummy_span());
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "expected Int, got {ty:?}"
        );
        assert!(ctx.errors.is_empty(), "unexpected errors: {:?}", ctx.errors);
    }

    // ── Test 2: pipe_chain ────────────────────────────────────────────────────

    /// `[1,2,3] |> listMap double |> listLength` → `Int`.
    ///
    /// We use a simplified monomorphic model:
    /// - `listMap : (Int -> Int) -> List Int -> List Int` (curried 2-arg form).
    /// - First pipe: `listMap double [1,2,3]` → `List Int`.
    /// - Second pipe: `listLength (List Int)` → `Int`.
    ///
    /// Ridge's Call nodes are flat multi-arg; `listMap double` is a `Call` that
    /// partially applies, yielding a fn that is then piped with the list.
    /// For testing purposes we model the whole pipe chain in two steps using
    /// intermediate name bindings.
    #[test]
    fn pipe_chain() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        let list_int = Type::Con(b.list, vec![int_ty(&b)]);

        // double : Int -> Int
        let double_fn = make_fn_ty(&b, vec![int_ty(&b)], int_ty(&b));
        bind(&mut ctx, "double", double_fn);

        // listMap : (Int -> Int, List Int) -> List Int  (flat 2-arg, not curried)
        // We use a 2-param flat fn to keep Call inference simple.
        let map_fn = make_fn_ty(
            &b,
            vec![
                make_fn_ty(&b, vec![int_ty(&b)], int_ty(&b)),
                list_int.clone(),
            ],
            list_int.clone(),
        );
        bind(&mut ctx, "listMap", map_fn);

        // listLength : List Int -> Int
        let length_fn = make_fn_ty(&b, vec![list_int], int_ty(&b));
        bind(&mut ctx, "listLength", length_fn);

        let list_expr = Expr::List {
            elems: vec![int_lit("1"), int_lit("2"), int_lit("3")],
            span: dummy_span(),
        };

        // Step 1: `list_expr |> listMap double`
        // Pipe desugars to: listMap double list_expr  (lhs appended as last arg).
        let rhs1 = Expr::Call {
            callee: Box::new(ident("listMap")),
            args: vec![ident("double")],
            span: dummy_span(),
        };
        let step1 = infer_pipe(&mut ctx, &b, &list_expr, &rhs1, dummy_span());
        // step1 should be List Int
        let step1_resolved = ctx.deep_resolve(&step1);
        assert!(
            matches!(&step1_resolved, Type::Con(id, _) if *id == b.list),
            "step1 should be List _, got {step1_resolved:?}; errors: {:?}",
            ctx.errors
        );

        // Step 2: bind the intermediate result and pipe into listLength.
        bind(&mut ctx, "__step1", step1_resolved);

        let ty = infer_pipe(
            &mut ctx,
            &b,
            &ident("__step1"),
            &ident("listLength"),
            dummy_span(),
        );
        let ty_resolved = ctx.deep_resolve(&ty);
        assert!(
            matches!(ty_resolved, Type::Con(id, _) if id == b.int),
            "expected Int from chain, got {ty_resolved:?}; errors: {:?}",
            ctx.errors
        );
        assert!(ctx.errors.is_empty(), "unexpected errors: {:?}", ctx.errors);
    }

    // ── Test 3: propagate_in_result_fn ────────────────────────────────────────

    /// A fn returning `Result Text Text`, body `readFile "a" ?` → `Text`.
    #[test]
    fn propagate_in_result_fn() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // Set enclosing fn return type to Result Text Text.
        ctx.current_fn_ret = Some(result_ty(&b, text_ty(&b), text_ty(&b)));

        // readFile : Text -> Result Text Text
        let read_file_fn = make_fn_ty(
            &b,
            vec![text_ty(&b)],
            result_ty(&b, text_ty(&b), text_ty(&b)),
        );
        bind(&mut ctx, "readFile", read_file_fn);

        let inner = Expr::Call {
            callee: Box::new(ident("readFile")),
            args: vec![text_lit("a")],
            span: dummy_span(),
        };

        let ty = infer_propagate(&mut ctx, &b, &inner, dummy_span());
        // The `?` unwraps Result Text Text → Text.
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.text),
            "expected Text, got {ty:?}"
        );
        assert!(ctx.errors.is_empty(), "unexpected errors: {:?}", ctx.errors);
    }

    // ── Test 4: propagate_in_option_fn ────────────────────────────────────────

    /// A fn returning `Option Int`, body `lookup "key" ?` → `Int`.
    #[test]
    fn propagate_in_option_fn() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // Set enclosing fn return type to Option Int.
        ctx.current_fn_ret = Some(option_ty(&b, int_ty(&b)));

        // lookup : Text -> Option Int
        let lookup_fn = make_fn_ty(&b, vec![text_ty(&b)], option_ty(&b, int_ty(&b)));
        bind(&mut ctx, "lookup", lookup_fn);

        let inner = Expr::Call {
            callee: Box::new(ident("lookup")),
            args: vec![text_lit("key")],
            span: dummy_span(),
        };

        let ty = infer_propagate(&mut ctx, &b, &inner, dummy_span());
        // The `?` unwraps Option Int → Int.
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "expected Int, got {ty:?}"
        );
        assert!(ctx.errors.is_empty(), "unexpected errors: {:?}", ctx.errors);
    }

    // ── Test 5: propagate_outside_result_or_option_T021 ──────────────────────

    /// A fn returning `Int`, body contains `something ?` → T021 fires.
    #[test]
    fn propagate_outside_result_or_option_t021() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // Set enclosing fn return type to Int (not Result/Option).
        ctx.current_fn_ret = Some(int_ty(&b));

        // something : Result Int Text — but the *context* is wrong.
        let something_fn = make_fn_ty(&b, vec![], result_ty(&b, int_ty(&b), text_ty(&b)));
        bind(&mut ctx, "something", something_fn);

        let inner = Expr::Call {
            callee: Box::new(ident("something")),
            args: vec![],
            span: dummy_span(),
        };

        let ty = infer_propagate(&mut ctx, &b, &inner, dummy_span());
        assert!(matches!(ty, Type::Error), "expected Error, got {ty:?}");
        let has_t021 = ctx.errors.iter().any(|e| e.code() == "T021");
        assert!(has_t021, "expected T021, got: {:?}", ctx.errors);
    }

    // ── Test 6: propagate_inner_type_mismatch_T001 ────────────────────────────

    /// Fn returns `Result Text Text`, body `(5 : Int) ?` → T001 (Int does not
    /// unify with `Result Text Text`).
    #[test]
    fn propagate_inner_type_mismatch_t001() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // Context: fn returning Result Text Text.
        ctx.current_fn_ret = Some(result_ty(&b, text_ty(&b), text_ty(&b)));

        // Inner expression is just `5 : Int`.
        let inner = int_lit("5");

        let ty = infer_propagate(&mut ctx, &b, &inner, dummy_span());
        assert!(
            matches!(ty, Type::Error),
            "expected Error for type mismatch, got {ty:?}"
        );
        // A T001 TypeMismatch should have been emitted (Int ≠ Result _ _).
        assert!(!ctx.errors.is_empty(), "expected at least one error");
    }

    // ── Test 7: try_block_basic ───────────────────────────────────────────────

    /// `try { 5 }` → `Result Int _` (Ok payload = Int).
    #[test]
    fn try_block_basic() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        let block = ridge_ast::Block {
            stmts: vec![int_lit("5")],
            span: dummy_span(),
        };

        let ty = infer_try(&mut ctx, &b, &block, dummy_span());
        // Should be Result Int _ .
        match &ty {
            Type::Con(id, args) if *id == b.result => {
                let ok_ty = ctx.deep_resolve(&args[0]);
                assert!(
                    matches!(ok_ty, Type::Con(i, _) if i == b.int),
                    "expected Ok payload Int, got {ok_ty:?}"
                );
            }
            other => panic!("expected Result _, got {other:?}"),
        }
        assert!(ctx.errors.is_empty(), "unexpected errors: {:?}", ctx.errors);
    }

    // ── Test 8: try_block_with_propagate ─────────────────────────────────────

    /// `try { let x = readFile "a" ?; x }` → `Result Text _`.
    ///
    /// Inside the try block, `readFile "a" ?` should unwrap to `Text` and
    /// the try expression's Ok payload should be `Text`.
    #[test]
    fn try_block_with_propagate() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // readFile : Text -> Result Text Text
        let read_file_fn = make_fn_ty(
            &b,
            vec![text_ty(&b)],
            result_ty(&b, text_ty(&b), text_ty(&b)),
        );
        bind(&mut ctx, "readFile", read_file_fn);

        // Simulate: let x = readFile "a" ?; x
        // We build the block manually.
        // let x = readFile "a" ?  — a Let binding
        let read_call = Expr::Call {
            callee: Box::new(ident("readFile")),
            args: vec![text_lit("a")],
            span: dummy_span(),
        };
        let propagate_expr = Expr::Propagate {
            inner: Box::new(read_call),
            span: dummy_span(),
        };
        let let_stmt = Expr::Let {
            pat: ridge_ast::Pattern::Var {
                name: make_ident("x"),
                span: dummy_span(),
            },
            ty: None,
            value: Box::new(propagate_expr),
            span: dummy_span(),
        };
        let return_x = ident("x");

        let block = ridge_ast::Block {
            stmts: vec![let_stmt, return_x],
            span: dummy_span(),
        };

        let ty = infer_try(&mut ctx, &b, &block, dummy_span());
        match &ty {
            Type::Con(id, args) if *id == b.result => {
                let ok_ty = ctx.deep_resolve(&args[0]);
                assert!(
                    matches!(ok_ty, Type::Con(i, _) if i == b.text),
                    "expected Ok payload Text, got {ok_ty:?}"
                );
            }
            other => panic!("expected Result _, got {other:?}"),
        }
        assert!(ctx.errors.is_empty(), "unexpected errors: {:?}", ctx.errors);
    }

    // ── Test 9: try_block_outside_fn ─────────────────────────────────────────

    /// `try { 42 }` at module top-level (no `current_fn_ret`) — should type
    /// correctly as `Result Int _` without needing `current_fn_ret`.
    #[test]
    fn try_block_outside_fn() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        // No current_fn_ret set — simulates module-level expression.
        assert!(ctx.current_fn_ret.is_none());

        let block = ridge_ast::Block {
            stmts: vec![int_lit("42")],
            span: dummy_span(),
        };

        let ty = infer_try(&mut ctx, &b, &block, dummy_span());
        assert!(
            matches!(&ty, Type::Con(id, _) if *id == b.result),
            "expected Result _, got {ty:?}"
        );
        assert!(ctx.errors.is_empty(), "unexpected errors: {:?}", ctx.errors);
    }

    // ── Test 10: discarded_result_T022_basic ─────────────────────────────────

    /// A block `{ readFile "a"; 1 }` — the readFile result is discarded → T022.
    #[test]
    fn discarded_result_t022_basic() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // readFile : Text -> Result Text Text
        let read_file_fn = make_fn_ty(
            &b,
            vec![text_ty(&b)],
            result_ty(&b, text_ty(&b), text_ty(&b)),
        );
        bind(&mut ctx, "readFile", read_file_fn);

        let block = ridge_ast::Block {
            stmts: vec![
                Expr::Call {
                    callee: Box::new(ident("readFile")),
                    args: vec![text_lit("a")],
                    span: dummy_span(),
                },
                int_lit("1"),
            ],
            span: dummy_span(),
        };

        // Use the T10-aware infer_block exported from infer.rs.
        let ty = infer_block(&mut ctx, &b, &block);
        // Block value is Int.
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "expected Int, got {ty:?}"
        );
        // T022 should have fired for the discarded Result.
        let has_t022 = ctx.errors.iter().any(|e| e.code() == "T022");
        assert!(has_t022, "expected T022, got: {:?}", ctx.errors);
    }

    // ── Test 11: discarded_unit_no_warning ───────────────────────────────────

    /// `{ print "hi"; 1 }` — print returns Unit, no T022.
    #[test]
    fn discarded_unit_no_warning() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // print : Text -> Unit
        let print_fn = make_fn_ty(&b, vec![text_ty(&b)], unit_ty(&b));
        bind(&mut ctx, "print", print_fn);

        let block = ridge_ast::Block {
            stmts: vec![
                Expr::Call {
                    callee: Box::new(ident("print")),
                    args: vec![text_lit("hi")],
                    span: dummy_span(),
                },
                int_lit("1"),
            ],
            span: dummy_span(),
        };

        let ty = infer_block(&mut ctx, &b, &block);
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "expected Int, got {ty:?}"
        );
        // No T022 — print returns Unit.
        let has_t022 = ctx.errors.iter().any(|e| e.code() == "T022");
        assert!(!has_t022, "unexpected T022: {:?}", ctx.errors);
    }

    // ── Test 12: discarded_send_no_warning ───────────────────────────────────

    /// `{ (actor ! msg); 1 }` — Send returns Unit, no T022.
    ///
    /// We simulate this by binding a fake "`send_result`" function that returns Unit
    /// (since we can't easily construct `Expr::Send` without a real Handle type).
    /// The real Send is deferred to T15; we test the T022 exemption via the
    /// unit-return path.
    #[test]
    fn discarded_send_no_warning() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // Simulate Send result: a fn returning Unit.
        let send_like_fn = make_fn_ty(&b, vec![], unit_ty(&b));
        bind(&mut ctx, "sendAction", send_like_fn);

        let block = ridge_ast::Block {
            stmts: vec![
                Expr::Call {
                    callee: Box::new(ident("sendAction")),
                    args: vec![],
                    span: dummy_span(),
                },
                int_lit("1"),
            ],
            span: dummy_span(),
        };

        let ty = infer_block(&mut ctx, &b, &block);
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "expected Int, got {ty:?}"
        );
        let has_t022 = ctx.errors.iter().any(|e| e.code() == "T022");
        assert!(!has_t022, "unexpected T022: {:?}", ctx.errors);
    }
}
