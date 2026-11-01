//! Formatter keeps predicate grouping and the source pattern intact.
#![allow(clippy::unwrap_used)]

#[test]
fn predicates_round_trip_with_comments_and_explicit_parentheses() {
    let source = "fn probe value =\n    -- preserve grouping\n    (value is Some _) == true\n";
    let formatted = ridge_fmt::format_source(source).unwrap();
    assert!(formatted.contains("(value is Some _) == true"));
    assert!(formatted.contains("-- preserve grouping"));
    assert_eq!(ridge_fmt::format_source(&formatted).unwrap(), formatted);
    assert!(ridge_parser::parse_source(&formatted).errors.is_empty());
}
