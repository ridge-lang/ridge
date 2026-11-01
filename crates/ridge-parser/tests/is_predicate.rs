//! Predicate grammar, binding boundaries and comparison precedence.
#![allow(clippy::unwrap_used, clippy::panic)]
use ridge_parser::parse_source;

#[test]
fn predicate_patterns_and_parenthesized_comparisons_parse() {
    for expression in [
        "shape is Square",
        "shape is Square 5",
        "shape is M.Square",
        "shape is Square { length = 5, .. }",
        "shape is { length = _, .. }",
        "shape is (Some _)",
        "shape is [_, .., _]",
        "shape is (_ :: _)",
        "shape is (_, true)",
        "shape is _",
        "shape is 5",
        "shape is Square && ready",
        "ready || shape is Square",
        "(shape is Square) == ready",
        "(1 < 2) is true",
        "shape is Square || shape is Circle",
        "get shape is Square",
        "shape\n    is Square",
        "shape is\n    Square",
    ] {
        let source = format!("fn probe () = {expression}\n");
        let parsed = parse_source(&source);
        assert!(
            parsed.lex_errors.is_empty(),
            "{source}: {:?}",
            parsed.lex_errors
        );
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
    }
}

#[test]
fn predicates_reject_every_binding_form_with_match_guidance() {
    for pattern in [
        "x",
        "Some x",
        "whole @ Some _",
        "User { name }",
        "{ name }",
        "[rest @ ..]",
        "(_, x)",
        "_ :: tail",
    ] {
        let source = format!("fn probe () = subject is {pattern}\n");
        let parsed = parse_source(&source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| error.to_string().contains("use `match`")),
            "{source}: {:?}",
            parsed.errors
        );
    }
}

#[test]
fn predicate_comparison_chains_require_parentheses() {
    for expression in [
        "value is Some is None",
        "value is Some == true",
        "value == other is Some",
        "1 < 2 is true",
        "value is 1 < 2",
        "value != other is Some",
    ] {
        let parsed = parse_source(&format!("fn probe () = {expression}\n"));
        assert!(
            parsed.errors.iter().any(|error| error.code() == "P009"),
            "{expression}: {:?}",
            parsed.errors
        );
    }
}

#[test]
fn is_is_reserved_but_identifier_prefixes_stay_legal() {
    assert!(parse_source("fn is () = true\n")
        .errors
        .iter()
        .any(|error| error.to_string().contains("rename this identifier")));
    assert!(parse_source("fn island () = true\n").errors.is_empty());
}

#[test]
fn unsupported_slice_shapes_are_rejected_instead_of_becoming_wildcards() {
    for pattern in [
        "Some [.., _]",
        "([.., _], _)",
        "[1, .., _]",
        "[.., 5]",
        "[([.., _])]",
        "{ xs = [.., _], .. }",
    ] {
        let parsed = parse_source(&format!("fn probe () = subject is {pattern}\n"));
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| error.to_string().contains("rest patterns")),
            "{pattern}: {:?}",
            parsed.errors
        );
    }
}
