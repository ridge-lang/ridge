//! Continuation-line and nested-layout parsing.
//!
//! Long headers, list elements, and lambda bodies routinely wrap across lines.
//! These pin the three shapes that used to break:
//!
//! - a `fn` signature whose return type lands on a `->`-leading line,
//! - a list whose elements carry a nested `[ … ]` argument opening on the next
//!   indented line, and
//! - a multi-statement lambda body whose first statement is a multi-line
//!   `match`.
//!
//! Each asserts the *structure* the parser builds, not merely that it does not
//! error, so a future regression that silently drops the return type, splits a
//! list element, or strands a lambda statement is caught.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_ast::{Body, Expr, FnDecl, Module};
use ridge_parser::parse_source;

/// The single `fn` declaration in `src` (panics if there is not exactly one).
fn only_fn(module: &Module) -> &FnDecl {
    let mut fns = module.items.iter().filter_map(|it| match it {
        ridge_ast::Item::Fn(f) => Some(f),
        _ => None,
    });
    let f = fns.next().expect("expected a fn declaration");
    assert!(fns.next().is_none(), "expected exactly one fn declaration");
    f
}

/// The function's value expression, unwrapping a single-statement block (the
/// `= <INDENT> expr <DEDENT>` form wraps a lone expression in a `Block`).
fn fn_value(f: &FnDecl) -> &Expr {
    match &f.body {
        Body::Expr(Expr::Block(b)) if b.stmts.len() == 1 => &b.stmts[0],
        Body::Expr(e) => e,
        Body::Ffi { .. } => panic!("fn has an FFI body, not an expression"),
    }
}

/// A signature whose return type wraps onto a `->`-leading line keeps it.
#[test]
fn wrapped_signature_captures_return_type() {
    let src = "\
fn seed (a: Int) (b: Int)
        -> Result Unit Error =
    let x = a
    x
";
    let r = parse_source(src);
    assert!(r.lex_errors.is_empty(), "lex errors: {:?}", r.lex_errors);
    assert!(r.errors.is_empty(), "parse errors: {:?}", r.errors);

    let f = only_fn(&r.module);
    assert_eq!(f.params.len(), 2, "both parameters parsed");
    assert!(
        f.ret.is_some(),
        "the return type on the `->` continuation line must be captured"
    );
}

/// List elements keep the nested bracket argument that opens on their
/// continuation line, instead of being split apart.
#[test]
fn list_elements_fold_in_nested_bracket_continuation() {
    let src = "\
fn migrations () -> List Int =
    [ mig \"0001\"
        [ createSchema ]
    , mig \"0002\"
        [ createIndex ] ]
";
    let r = parse_source(src);
    assert!(r.lex_errors.is_empty(), "lex errors: {:?}", r.lex_errors);
    assert!(r.errors.is_empty(), "parse errors: {:?}", r.errors);

    let f = only_fn(&r.module);
    let Expr::List { elems, .. } = fn_value(f) else {
        panic!("fn body is not a list literal: {:?}", f.body);
    };
    assert_eq!(elems.len(), 2, "both elements parsed, not split apart");
    for e in elems {
        let Expr::Call { args, .. } = e else {
            panic!("each element should be a call: {e:?}");
        };
        assert!(
            matches!(args.last(), Some(Expr::List { .. })),
            "the nested `[ … ]` continuation folds in as the element's argument: {e:?}"
        );
    }
}

/// A multi-statement lambda body survives a multi-line `match` — the
/// statement after the match is not stranded.
#[test]
fn lambda_body_keeps_statement_after_multiline_match() {
    let src = "\
fn f () -> Unit =
    let _ = List.forEach (fn n ->
        let t = match n
            0 -> \"zero\"
            _ -> \"nonzero\"
        Io.println t) (List.range 0 2)
    Ok ()
";
    let r = parse_source(src);
    assert!(r.lex_errors.is_empty(), "lex errors: {:?}", r.lex_errors);
    assert!(r.errors.is_empty(), "parse errors: {:?}", r.errors);

    // fn body block → first stmt `let _ = <call>` → arg 0 is the lambda →
    // the lambda body is a two-statement block (`let t = match …`, `Io.println t`).
    let f = only_fn(&r.module);
    let Body::Expr(Expr::Block(outer)) = &f.body else {
        panic!("fn body is not a block: {:?}", f.body);
    };
    let Expr::Let { value, .. } = &outer.stmts[0] else {
        panic!("first statement is not a let: {:?}", outer.stmts[0]);
    };
    let Expr::Call { args, .. } = value.as_ref() else {
        panic!("let value is not a call: {value:?}");
    };
    // The lambda was written parenthesised — `(fn n -> …)` — so unwrap the
    // `Paren` wrapper before reaching the lambda.
    let lambda = match &args[0] {
        Expr::Paren { inner, .. } => inner.as_ref(),
        other => other,
    };
    let Expr::Lambda { body, .. } = lambda else {
        panic!("first argument is not a lambda: {lambda:?}");
    };
    let Expr::Block(lam) = body.as_ref() else {
        panic!("lambda body collapsed to a single expression — the statement after the match was stranded: {body:?}");
    };
    assert_eq!(
        lam.stmts.len(),
        2,
        "the lambda keeps both the `let`/`match` statement and the trailing `Io.println t`"
    );
}
