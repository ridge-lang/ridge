//! The standard library's `@primitive` declarations, as the shared table
//! reports them.
//!
//! What is being pinned is a layering claim rather than a behaviour: the table
//! every codegen backend reads must describe arithmetic as an operation of the
//! language, not as a call into a particular runtime. Before this existed,
//! `Int.add` was declared `@ffi("erlang", "+", 2)`, so a second backend was
//! handed `erlang` as the meaning of `+`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_stdlib::codegen_ffi_targets::extract_all_stdlib_decls;
use ridge_stdlib::stdlib_targets::{lookup, primitive_symbols, StdlibTarget};

/// Every symbol the language treats as a primitive operation.
const ARITHMETIC: &[(&str, &str, u32)] = &[
    ("std.float", "add", 2),
    ("std.float", "div", 2),
    ("std.float", "mul", 2),
    ("std.float", "neg", 1),
    ("std.float", "sub", 2),
    ("std.int", "add", 2),
    ("std.int", "div", 2),
    ("std.int", "mul", 2),
    ("std.int", "neg", 1),
    ("std.int", "rem", 2),
    ("std.int", "sub", 2),
];

#[test]
fn arithmetic_is_the_primitive_set_and_nothing_else_is() {
    let found = primitive_symbols();
    let expected: Vec<(&str, &str, u32)> = ARITHMETIC.to_vec();
    assert_eq!(
        found, expected,
        "the set of `@primitive` declarations changed. Adding one is a language \
         decision and every backend owes it an answer, so the backend tables have \
         to grow the same entry in the same change."
    );
}

#[test]
fn a_primitive_names_no_module_at_all() {
    // The failure mode this catches is quiet: if the build script stopped
    // recognising `@primitive`, the declaration below reads as an ordinary
    // Ridge body and the table answers `RidgeModule { module: "std.int" }`.
    // Everything still compiles and every program still runs — each `+` just
    // becomes a call into the compiled stdlib module first.
    for (module, name, arity) in ARITHMETIC {
        let target = lookup(module, name)
            .unwrap_or_else(|| panic!("{module}.{name} must be in the stdlib target table"));
        assert_eq!(
            target,
            &StdlibTarget::Primitive { arity: *arity },
            "{module}.{name} must resolve as a primitive"
        );
    }
}

#[test]
fn a_neighbouring_declaration_still_names_its_host() {
    // The control: `@ffi` in the same file is unaffected, so a green run above
    // means primitives were recognised rather than that the table went empty.
    let target = lookup("std.int", "toText").expect("std.int.toText must be in the table");
    assert_eq!(
        target,
        &StdlibTarget::Foreign {
            module: "erlang".to_owned(),
            fn_name: "integer_to_binary".to_owned(),
            arity: 1,
        }
    );
}

#[test]
fn the_reference_extractor_reports_no_target_for_a_primitive() {
    // `extract_all_stdlib_decls` is a second, independent scan of the same
    // sources. It has to agree that these declarations describe no target —
    // otherwise the two disagree and only one of them ships.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib");
    let decls = extract_all_stdlib_decls(&dir).expect("stdlib extraction must succeed");

    assert!(
        decls.iter().any(|d| d.ridge_module == "std.int"),
        "the extractor found nothing in std.int, so the assertions below prove nothing"
    );
    let leaked: Vec<_> = decls
        .iter()
        .filter(|d| {
            ARITHMETIC
                .iter()
                .any(|(m, n, _)| d.ridge_module == *m && d.ridge_fn == *n)
        })
        .map(|d| format!("{}.{}", d.ridge_module, d.ridge_fn))
        .collect();
    assert!(
        leaked.is_empty(),
        "these are primitives and describe no target, but the extractor gave them one: {leaked:?}"
    );
}
