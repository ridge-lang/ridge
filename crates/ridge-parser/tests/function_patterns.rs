//! Named function parameters preserve inferred destructuring syntax.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_ast::Item;
use ridge_parser::parse_source;

#[test]
fn inferred_function_parameter_patterns_parse_without_changing_arity() {
    for (source, arity) in [
        (include_str!("fixtures/inferred_tuple_param.ridge"), 1),
        ("fn distance (x1, y1) (x2, y2) = x1 + y1 + x2 + y2\n", 2),
        ("fn record (Point { x, y }) = x + y\n", 1),
        ("fn constructor (MkBox n) = n\n", 1),
        ("fn nested ((x, y), z) = x\n", 1),
        ("fn alias (pair @ (x, y)) = pair\n", 1),
        ("fn tuple ((x, y): (Int, Int)) = x + y\n", 1),
        ("fn partial (Some x) = x\n", 1),
        ("fn literal (0) = 0\n", 1),
        ("fn boolean (true) = 0\n", 1),
        ("fn nullary () = 0\n", 0),
        ("fn unit (_: Unit) = 0\n", 1),
        (
            "fn main () =\n    fn inner (x, y) = x + y\n    inner (1, 2)\n",
            0,
        ),
    ] {
        let result = parse_source(source);
        assert!(
            result.lex_errors.is_empty(),
            "{source}: {:?}",
            result.lex_errors
        );
        assert!(result.errors.is_empty(), "{source}: {:?}", result.errors);
        assert_eq!(result.module.items.len(), 1, "{source}");
        let Item::Fn(decl) = &result.module.items[0] else {
            panic!("expected function")
        };
        assert_eq!(decl.params.len(), arity, "{source}");
    }
    assert!(!parse_source("fn unitPattern (()) = 0\n").errors.is_empty());
    assert!(!parse_source("fn reserved (__param_0, y) = y\n")
        .errors
        .is_empty());
}

#[test]
fn abstract_method_patterns_require_input_annotations() {
    let inferred = parse_source("class Total a =\n    total (x, y) -> Int\n");
    assert!(
        inferred.errors.iter().any(|e| e.code() == "P005"),
        "{:?}",
        inferred.errors
    );
    let annotated = parse_source("class Total a =\n    total ((x, y): (Int, Int)) -> Int\n");
    assert!(annotated.errors.is_empty(), "{:?}", annotated.errors);
}
