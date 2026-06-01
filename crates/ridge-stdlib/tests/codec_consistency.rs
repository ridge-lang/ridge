//! Keeps `stdlib/codec.ridge` and the Rust-registered `Encode`/`Decode` classes
//! in sync.
//!
//! `Encode` and `Decode` are built-in prelude classes registered in Rust
//! (`register_prelude_classes`), so `codec.ridge` is not compiled as part of
//! the standard library — it is the canonical, human-readable declaration of
//! the same classes. This test parses `codec.ridge` and asserts that every
//! `class` it declares matches the Rust registration (method names and
//! arities), so the documentation can never silently drift from the compiler.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ridge_ast::Item;
use ridge_typecheck::{register_prelude_classes, ClassTable};

fn codec_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("stdlib")
        .join("codec.ridge");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

#[test]
fn codec_ridge_parses_cleanly() {
    let src = codec_source();
    let result = ridge_parser::parse_source(&src);
    assert!(
        result.errors.is_empty(),
        "codec.ridge must be valid Ridge; parse errors: {:?}",
        result.errors
    );
}

#[test]
fn codec_classes_match_rust_registration() {
    let src = codec_source();
    let result = ridge_parser::parse_source(&src);
    assert!(
        result.errors.is_empty(),
        "codec.ridge must parse; errors: {:?}",
        result.errors
    );

    let mut ct = ClassTable::new();
    register_prelude_classes(&mut ct);

    let mut seen = Vec::new();
    for item in &result.module.items {
        let Item::ClassDecl(decl) = item else {
            continue;
        };
        let class_name = decl.name.text.as_str();
        seen.push(class_name.to_string());

        let class_id = ct.id_by_name(class_name).unwrap_or_else(|| {
            panic!("codec.ridge declares class `{class_name}` but it is not registered in Rust")
        });
        let info = ct
            .get(class_id)
            .expect("registered class id must resolve to ClassInfo");

        // Same number of methods, same names, same arities.
        assert_eq!(
            decl.methods.len(),
            info.method_sigs.len(),
            "class `{class_name}`: method count differs (codec.ridge {} vs Rust {})",
            decl.methods.len(),
            info.method_sigs.len()
        );
        for ast_method in &decl.methods {
            let name = ast_method.name.text.as_str();
            let arity = ast_method.params.len();
            let registered = info
                .method_sigs
                .iter()
                .find(|m| m.name == name)
                .unwrap_or_else(|| panic!("class `{class_name}`: method `{name}` is in codec.ridge but not registered in Rust"));
            assert_eq!(
                registered.arity, arity,
                "class `{class_name}`: method `{name}` arity differs (codec.ridge {arity} vs Rust {})",
                registered.arity
            );
        }
    }

    // codec.ridge must declare exactly the two codec classes.
    assert!(
        seen.contains(&"Encode".to_string()),
        "codec.ridge must declare `class Encode`; saw {seen:?}"
    );
    assert!(
        seen.contains(&"Decode".to_string()),
        "codec.ridge must declare `class Decode`; saw {seen:?}"
    );
}
