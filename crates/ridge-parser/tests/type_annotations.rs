//! Artefact type annotations retain complete source structure and precedence.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_ast::{Capability, Item, Type, TypeAnnotation};
use ridge_parser::parse_source;

fn annotation(spelling: &str) -> String {
    let source = format!("const value: {spelling} = 0\n");
    let parsed = parse_source(&source);
    assert!(parsed.errors.is_empty(), "{spelling}: {:?}", parsed.errors);
    let Item::Const(decl) = &parsed.module.items[0] else {
        panic!("expected a const declaration");
    };
    TypeAnnotation::from(&decl.ty).to_string()
}

#[test]
fn source_type_annotations_are_complete_and_roundtrip() {
    for (source, expected) in [
        ("Text", "Text"),
        ("Caption", "Caption"),
        ("a", "a"),
        ("[List Int]", "List (List Int)"),
        (
            "Map Text (Option (List Int))",
            "Map Text (Option (List Int))",
        ),
        ("(Int, Text)", "(Int, Text)"),
        ("(Int -> Int) -> Text", "(Int -> Int) -> Text"),
        ("Int -> Text -> Bool", "Int -> Text -> Bool"),
        ("fn io net Text Int -> Unit", "fn io net Text Int -> Unit"),
        (
            "fn io fs net time random env proc ffi db Unit -> Unit",
            "fn io fs net time random env proc ffi db Unit -> Unit",
        ),
        ("fn () -> Unit", "fn () -> Unit"),
        ("fn io () -> Unit", "fn io () -> Unit"),
        ("List (fn io Text -> Unit)", "List (fn io Text -> Unit)"),
        ("{name: Text, count: Int}", "{ name: Text, count: Int }"),
        ("{name: Text | r}", "{ name: Text | r }"),
        (
            "List (List (List (List (List (List Int)))))",
            "List (List (List (List (List (List Int)))))",
        ),
    ] {
        assert_eq!(annotation(source), expected, "{source}");
        assert_eq!(annotation(expected), expected, "roundtrip: {source}");
    }
}

#[test]
fn source_type_annotation_names_spawn_capability_from_ast() {
    let mut parsed = parse_source("const value: fn io Unit -> Unit = 0\n");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let Item::Const(decl) = &mut parsed.module.items[0] else {
        panic!("expected a const declaration");
    };
    let Type::Fn { fn_ty, .. } = &mut decl.ty else {
        panic!("expected a function type");
    };
    fn_ty.caps = vec![Capability::Spawn];
    assert_eq!(
        TypeAnnotation::from(&decl.ty).to_string(),
        "fn spawn Unit -> Unit"
    );
}
