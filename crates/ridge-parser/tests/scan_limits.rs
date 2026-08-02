//! Boundary tests for the parser's forward-scan windows (B3).
//!
//! Two `fn`-disambiguation scans look ahead for a decisive token before
//! committing to a parse: `fn_is_inner_fn` (inner fn vs lambda) and
//! `lambda_has_return_type_eq` (return-type annotation vs body). Both are
//! capped at 4096 tokens as an anti-pathology bound. These tests pin the
//! contract:
//!
//! - headers that are large but within the window classify **correctly**
//!   (an earlier 200/64-token window silently misparsed these — regression
//!   coverage),
//! - genuine lambdas with long bodies still classify as lambdas,
//! - a `match` with a scrutinee wider than its own 200-token window keeps
//!   parsing correctly (that scan's default is benign).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_ast::{Expr, Item};
use ridge_parser::parse_source;

/// An n-element tuple type, ~2n+2 tokens.
fn tuple_type(n: usize) -> String {
    format!("({})", vec!["Int"; n].join(", "))
}

/// An inner fn whose signature (`name … =`) spans far past the old 200-token
/// window must still parse as an `InnerFn` declaration, not silently degrade
/// to a discarded lambda.
#[test]
fn inner_fn_with_wide_signature_stays_inner_fn() {
    let src = format!(
        "fn outer x =\n    fn inner (y: {}) -> Int = 0\n    inner x\n",
        tuple_type(500)
    );
    let r = parse_source(&src);
    assert!(
        r.errors.is_empty(),
        "wide inner fn must parse clean: {:?}",
        r.errors
    );
    let Some(Item::Fn(outer)) = r.module.items.first() else {
        panic!("expected a top-level fn, got {:?}", r.module.items);
    };
    let ridge_ast::Body::Expr(Expr::Block(block)) = &outer.body else {
        panic!("expected a block body, got {:?}", outer.body);
    };
    assert!(
        matches!(block.stmts.first(), Some(Expr::InnerFn { .. })),
        "first statement must be an InnerFn, got {:?}",
        block.stmts.first()
    );
}

/// A lambda with an annotated return type wider than the old 64-token window
/// must keep its real body (`x`), not parse the annotation as the body.
#[test]
fn lambda_with_wide_return_type_keeps_real_body() {
    let src = format!(
        "const g: Int -> Int = fn (x: Int) -> {} = x\n",
        tuple_type(300)
    );
    let r = parse_source(&src);
    assert!(
        r.errors.is_empty(),
        "wide return type must parse clean: {:?}",
        r.errors
    );
    let Some(Item::Const(c)) = r.module.items.first() else {
        panic!("expected a top-level const, got {:?}", r.module.items);
    };
    let Expr::Lambda { body, .. } = &c.value else {
        panic!("expected a lambda value, got {:?}", c.value);
    };
    assert!(
        matches!(**body, Expr::Ident(_)),
        "lambda body must be the real body `x`, got {body:?}"
    );
}

/// A genuine lambda whose body alone spans hundreds of tokens must classify
/// as a lambda (the window's default favours lambdas; this pins that a long
/// body does not flip the classification).
#[test]
fn lambda_with_long_body_stays_lambda() {
    let long_body = vec!["x"; 500].join(" + ");
    let src = format!("const g: Int -> Int = fn x -> {long_body}\n");
    let r = parse_source(&src);
    assert!(
        r.errors.is_empty(),
        "long lambda body must parse clean: {:?}",
        r.errors
    );
    let Some(Item::Const(c)) = r.module.items.first() else {
        panic!("expected a top-level const, got {:?}", r.module.items);
    };
    assert!(
        matches!(c.value, Expr::Lambda { .. }),
        "value must be a Lambda, got {:?}",
        c.value
    );
}

/// A `match` whose scrutinee spans more tokens than the layout-detection
/// window still parses as a match with the full scrutinee (that scan's
/// out-of-window default is benign — pinned so it stays so).
#[test]
fn match_with_wide_scrutinee_parses_correctly() {
    let scrut = format!("({})", vec!["0"; 300].join(", "));
    let src = format!("fn f x =\n  match {scrut}\n    _ -> 0\n");
    let r = parse_source(&src);
    assert!(
        r.errors.is_empty(),
        "wide scrutinee must parse clean: {:?}",
        r.errors
    );
    let Some(Item::Fn(f)) = r.module.items.first() else {
        panic!("expected a top-level fn, got {:?}", r.module.items);
    };
    let ridge_ast::Body::Expr(Expr::Block(block)) = &f.body else {
        panic!("expected a block body, got {:?}", f.body);
    };
    let Some(Expr::Match { scrutinee, .. }) = block.stmts.first() else {
        panic!("expected a match statement, got {:?}", block.stmts.first());
    };
    let Expr::Tuple { elems, .. } = &**scrutinee else {
        panic!("expected a tuple scrutinee, got {scrutinee:?}");
    };
    assert_eq!(elems.len(), 300, "all scrutinee elements must survive");
}

/// The patterns in the wide-inner-fn case keep their full annotation: the
/// tuple type's 500 elements must all appear in the AST (guards against a
/// parse that truncates silently instead of misrouting).
#[test]
fn inner_fn_wide_signature_keeps_full_annotation() {
    let src = format!(
        "fn outer x =\n    fn inner (y: {}) -> Int = 0\n    inner x\n",
        tuple_type(500)
    );
    let r = parse_source(&src);
    assert!(r.errors.is_empty(), "must parse clean: {:?}", r.errors);
    let Some(Item::Fn(outer)) = r.module.items.first() else {
        panic!("expected a top-level fn");
    };
    let ridge_ast::Body::Expr(Expr::Block(block)) = &outer.body else {
        panic!("expected a block body");
    };
    let Some(Expr::InnerFn { decl, .. }) = block.stmts.first() else {
        panic!("expected an InnerFn");
    };
    let Some(ridge_ast::Param::Annotated { ty, .. }) = decl.params.first() else {
        panic!("expected an annotated parameter");
    };
    let ridge_ast::Type::Tuple { elems, .. } = ty else {
        panic!("expected a tuple annotation, got {ty:?}");
    };
    assert_eq!(elems.len(), 500);
}
