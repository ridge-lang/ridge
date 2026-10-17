//! Error conversion is confined to Result propagation; ordinary type equality stays strict.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};

fn check(source: &str) -> Vec<TypeError> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname = \"errors\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), source).unwrap();
    let discovery = discover_workspace(dir.path());
    let resolved = resolve_workspace(discovery.graph.unwrap());
    assert!(
        resolved.parse_errors.is_empty(),
        "parse: {:?}",
        resolved.parse_errors
    );
    assert!(resolved.errors.is_empty(), "resolve: {:?}", resolved.errors);
    typecheck_workspace(&resolved)
        .errors
        .into_iter()
        .map(|(_, e)| e)
        .collect()
}

#[test]
fn propagation_uses_explicit_error_conversion_instance() {
    let errors = check(
        r"
type Inner = Failed | Other
type Outer = Wrapped Inner | OtherOuter

instance ErrorFrom Inner Outer =
    errorFrom (x: Inner) -> Outer = Wrapped x

fn read () -> Result Int Inner = Err Failed

pub fn run () -> Result Int Outer =
    let n = read () ?
    Ok n
",
    );
    assert!(errors.is_empty(), "conversion should typecheck: {errors:?}");
}

#[test]
fn identical_errors_need_no_conversion_instance() {
    assert!(
        check("fn f (r: Result Int Text) -> Result Int Text =\n    let n = r ?\n    Ok n\n")
            .is_empty()
    );
}

#[test]
fn instance_method_must_accept_the_source_and_return_the_target() {
    for method in [
        "errorFrom (x: Int) -> Outer = Wrapped First",
        "errorFrom (x: Inner) -> Int = 1",
    ] {
        let source = format!("type Inner = First | Second\ntype Outer = Wrapped Inner | OtherOuter\ninstance ErrorFrom Inner Outer =\n    {method}\n\npub fn run (r: Result Int Inner) -> Result Int Outer =\n    let n = r ?\n    Ok n\n");
        let errors = check(&source);
        assert!(
            !errors.is_empty(),
            "invalid conversion instance accepted: {method}"
        );
    }
}

#[test]
fn conversion_method_cannot_hide_capabilities_or_omit_its_contract() {
    let prefix = "type Inner = First | Second\nfn io effect (x: Inner) -> Text = \"effect\"\n";
    for method in [
        "errorFrom (x: Inner) -> Text = effect x",
        "other (x: Inner) -> Text = \"other\"",
        "errorFrom (x: Inner) (y: Inner) -> Text = \"extra\"",
    ] {
        let source = format!("{prefix}instance ErrorFrom Inner Text =\n    {method}\n");
        assert!(
            !check(&source).is_empty(),
            "invalid conversion accepted: {method}"
        );
    }
}

#[test]
fn generic_conversion_instance_cannot_specialize_its_head() {
    let source = "type Box a = Boxed a | Empty\ninstance ErrorFrom (Box a) Text =\n    errorFrom (x: Box a) -> Text =\n        match x\n            Boxed value -> toText (value + 1)\n            Empty -> \"empty\"\n";
    assert!(!check(source).is_empty());
    let valid = "type Box a = Boxed a | Empty\ninstance ErrorFrom (Box a) Text where ToText a =\n    errorFrom (x: Box a) -> Text =\n        match x\n            Boxed value -> toText value\n            Empty -> \"empty\"\n";
    let errors = check(valid);
    assert!(
        errors.is_empty(),
        "generic conversion with a promised dictionary: {errors:?}"
    );
}

#[test]
fn context_conversion_reports_the_missing_destination_pair() {
    let source = "type Inner = First | Second\ntype Shell a = Wrapped a | Empty\ninstance ErrorFrom (Shell e) (Shell f) where ErrorFrom e f =\n    errorFrom (x: Shell e) -> Shell f = match x\n        Wrapped error -> Wrapped (errorFrom error)\n        Empty -> Empty\npub fn run (r: Result Int (Shell Inner)) -> Result Int (Shell Int) =\n    let n = r ?\n    Ok n\n";
    let errors = check(source);
    assert!(errors.iter().any(|error| matches!(error, TypeError::NoInstance { class, ty, .. } if class == "ErrorFrom" && ty.render(&[]).contains("Inner Int"))), "{errors:?}");
    assert!(
        !errors.iter().any(|error| error.code() == "T030"),
        "{errors:?}"
    );
}

#[test]
fn context_variables_must_be_projectable_from_the_instance_head() {
    let source = "type Shell a = Wrapped a | Empty\ninstance ErrorFrom (Shell e) Text where ErrorFrom e f =\n    errorFrom (x: Shell e) -> Text = \"unused\"\n";
    let errors = check(source);
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, TypeError::UnsupportedInstanceHead { .. })),
        "{errors:?}"
    );
}

#[test]
fn absent_conversion_is_a_missing_instance() {
    let errors =
        check("fn f (r: Result Int Int) -> Result Int Text =\n    let n = r ?\n    Ok n\n");
    assert!(errors.iter().any(|e| e.code() == "T029"), "{errors:?}");
    assert!(!errors.iter().any(|e| e.code() == "T001"), "{errors:?}");
}

#[test]
fn ordinary_result_returns_are_not_coerced() {
    let errors = check("fn f (r: Result Int Int) -> Result Int Text = r\n");
    assert!(errors.iter().any(|e| e.code() == "T001"), "{errors:?}");
}

#[test]
fn generic_conversion_uses_a_declared_two_parameter_constraint() {
    let errors = check("pub fn lift (r: Result a e) -> Result a f where ErrorFrom e f =\n    let n = r ?\n    Ok n\n");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn generic_conversion_cannot_invent_a_signature_promise() {
    let errors = check("pub fn lift (r: Result a e) -> Result a f =\n    let n = r ?\n    Ok n\n");
    assert!(errors.iter().any(|e| e.code() == "T055"), "{errors:?}");
}

#[test]
fn option_propagation_keeps_its_original_shape() {
    assert!(
        check("fn f (r: Option Int) -> Option Int =\n    let n = r ?\n    Some n\n").is_empty()
    );
    let errors = check("fn f (r: Option Int) -> Result Int Text =\n    let n = r ?\n    Ok n\n");
    assert!(errors.iter().any(|e| e.code() == "T001"), "{errors:?}");
}

#[test]
fn conversion_does_not_change_the_success_type() {
    let errors = check(
        r#"
type E = A | B
instance ErrorFrom E Text =
    errorFrom (x: E) -> Text = "error"
fn f (r: Result Int E) -> Result Bool Text =
    let n = r ?
    Ok n
"#,
    );
    assert!(errors.iter().any(|e| e.code() == "T001"), "{errors:?}");
}

#[test]
fn explicit_method_is_available_without_a_class_declaration() {
    let errors = check(
        r#"
type E = A | B
instance ErrorFrom E Text =
    errorFrom (x: E) -> Text = "error"
pub fn f (e: E) -> Text = errorFrom e
"#,
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn separate_promises_do_not_imply_a_direct_conversion() {
    let errors = check("pub fn lift (r: Result a e) -> Result a f where ErrorFrom e g, ErrorFrom g f =\n    let n = r ?\n    Ok n\n");
    assert!(
        errors.iter().any(|e| e.code() == "T055"),
        "a direct e-to-f promise is missing: {errors:?}"
    );
}

#[test]
fn annotated_try_result_selects_the_destination_error() {
    let errors = check(
        r#"
type E = LocalFault | OtherFault
instance ErrorFrom E Text =
    errorFrom (x: E) -> Text = "error"
pub fn f (r: Result Int E) -> Result Int Text =
    try
        let n = r ?
        n
"#,
    );
    assert!(errors.is_empty(), "{errors:?}");
}
