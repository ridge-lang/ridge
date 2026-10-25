//! Immutable let schemes are fresh at uses and share only captured types.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};

fn check(source: &str) -> Vec<TypeError> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"let-schemes\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), source).unwrap();
    let discovery = discover_workspace(dir.path());
    assert!(discovery.manifest_errors.is_empty());
    assert!(discovery.resolve_errors.is_empty());
    let resolved = resolve_workspace(discovery.graph.unwrap());
    assert!(resolved.lex_errors.is_empty(), "{:?}", resolved.lex_errors);
    assert!(
        resolved.parse_errors.is_empty(),
        "{:?}",
        resolved.parse_errors
    );
    assert!(resolved.errors.is_empty(), "{:?}", resolved.errors);
    typecheck_workspace(&resolved)
        .errors
        .into_iter()
        .map(|(_, error)| error)
        .collect()
}

#[test]
fn writes_and_recovery_facts_do_not_establish_polymorphism() {
    let shared = "pub fn value () -> Text =\n    let duplicate = fn f -> (f, f)\n    let (identity, apply) = duplicate (fn x -> x)\n    identity <- fn _ -> 7\n    apply \"hello\"\n";
    let errors = check(shared);
    assert!(
        errors.iter().any(|e| matches!(
            e,
            TypeError::TypeMismatch { .. } | TypeError::TypeMismatchInCall { .. }
        )),
        "{errors:?}"
    );
    for binding in [
        "let identity = fn x -> x",
        "let (identity, n) = (fn x -> x, 0)",
        "let { identity } = { identity = fn x -> x }",
    ] {
        for write in [
            "identity <- fn _ -> 7",
            "let ignored =\n        identity <- fn _ -> 7\n        0",
            "let overwrite = fn _ ->\n        identity <- fn _ -> 7\n    overwrite ()",
        ] {
            let source = format!("pub fn value () -> Text =\n    {binding}\n    let call = fn x -> identity x\n    {write}\n    call \"hello\"\n");
            let errors = check(&source);
            assert!(
                errors.iter().any(|e| matches!(
                    e,
                    TypeError::TypeMismatch { .. } | TypeError::TypeMismatchInCall { .. }
                )),
                "{source}: {errors:?}"
            );
        }
    }
    for binding in [
        "let read = fn record -> record.value",
        "let (read, n) = (fn record -> record.value, 0)",
    ] {
        let source = format!("pub fn value () -> Int =\n    {binding}\n    let first: Int = read ({{ value = 1 }})\n    read 7\n");
        let errors = check(&source);
        assert!(
            errors.iter().any(|e| matches!(
                e,
                TypeError::TypeMismatch { .. } | TypeError::TypeMismatchInCall { .. }
            )),
            "{source}: {errors:?}"
        );
    }
}

#[test]
fn shadowed_writes_and_unrelated_recovery_do_not_restrict_identity() {
    let source = "pub fn value () -> Int =\n    let ignored = fn record -> record.value\n    let identity = fn x -> x\n    let other =\n        var identity = 0\n        identity <- 1\n        identity\n    let first: Text = identity \"hello\"\n    identity 7\n";
    let errors = check(source);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn immutable_let_functions_instantiate_at_each_use() {
    for binding in [
        "let identity = fn x -> x",
        "fn same (x: a) -> a = x\n    let identity = same",
        "let (identity, count) = (fn x -> x, 0)",
        "let ((identity, count), other) = ((fn x -> x, 0), 1)",
        "let [identity] = [fn x -> x]",
        "let { identity } = { identity = fn x -> x }",
    ] {
        for (first, value, last, result) in [
            ("Text", "\"hello\"", "Int", "7"),
            ("Int", "7", "Text", "\"hello\""),
        ] {
            let source = format!("pub fn value () -> {last} =\n    {binding}\n    let first: {first} = identity {value}\n    identity {result}\n");
            let errors = check(&source);
            assert!(errors.is_empty(), "{binding}: {errors:?}");
        }
    }
}

#[test]
fn generalization_preserves_fixed_and_environment_dependent_types() {
    for source in [
        "pub fn value () -> Int =\n    let identity: Int -> Int = fn x -> x\n    let text: Text = identity \"bad\"\n    0\n",
        "pub fn value () -> Int =\n    var identity = fn x -> x\n    let text: Text = identity \"hello\"\n    identity 7\n",
        "pub fn value () -> Int =\n    let outer = 7\n    let keep = fn x -> outer\n    let text: Text = keep ()\n    0\n",
        "pub fn value f -> Text =\n    let apply = fn x -> f x\n    let text: Text = apply \"hello\"\n    let n: Int = apply 7\n    text\n",
        "pub fn value f -> Text =\n    let (apply, count) = (fn x -> f x, 0)\n    let text: Text = apply \"hello\"\n    let n: Int = apply 7\n    text\n",
        "pub fn value () -> Int =\n    let bad = fn f ->\n        let text: Text = f \"hello\"\n        f 7\n    bad (fn x -> x)\n",
        "pub fn value (outer: a) -> Int =\n    let copy = outer\n    let number: Int = copy\n    number\n",
        "pub fn value (outer: a) -> Int =\n    let (copy, count) = (outer, 0)\n    let number: Int = copy\n    number\n",
        "pub fn value (record: { value: Int | r }) -> Int =\n    let keep = fn _ -> record\n    let first: { value: Int, extra: Text } = keep ()\n    let second: { value: Int, extra: Int } = keep ()\n    second.value\n",
    ] {
        let errors = check(source);
        assert!(errors.iter().any(|e| matches!(e, TypeError::TypeMismatch { .. } | TypeError::TypeMismatchInCall { .. })), "{source}: {errors:?}");
    }
}

#[test]
fn class_checks_remain_active_alongside_let_generalization() {
    let valid = "import std.list as List\npub fn value () -> List Int =\n    let sort = List.sort\n    sort [3, 1, 2]\n";
    let errors = check(valid);
    assert!(errors.is_empty(), "{errors:?}");
    let invalid = "import std.list as List\ntype Item = | MkItem Int\npub fn value () -> List Item =\n    let identity = fn x -> x\n    let text: Text = identity \"hello\"\n    List.sort [identity (MkItem 1)]\n";
    let errors = check(invalid);
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, TypeError::NoInstance { class, .. } if class == "Ord")),
        "sorting Item without Ord must be rejected: {errors:?}"
    );
}

#[test]
fn polymorphic_helpers_preserve_callback_capabilities() {
    let source = "import std.io as Io\npub fn value () -> Unit =\n    let apply = fn callback x -> callback x\n    let before = apply (fn x -> x) 7\n    apply Io.println \"hello\"\n";
    let errors = check(source);
    assert!(
        errors.iter().any(|error| matches!(
            error,
            TypeError::CapabilityNotDeclared { .. }
                | TypeError::CallerCapabilityInsufficient { .. }
        )),
        "{errors:?}"
    );
    let errors = check(&source.replace("pub fn value", "pub fn io value"));
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn class_constraints_cannot_be_lost_when_a_let_is_specialized() {
    for binding in ["let sort = List.sort", "let (sort, count) = (List.sort, 0)"] {
        let source = format!("import std.list as List\ntype Item = | MkItem Int\npub fn value () -> List Item =\n    {binding}\n    sort [MkItem 1]\n");
        let errors = check(&source);
        assert!(
            errors.iter().any(
                |error| matches!(error, TypeError::NoInstance { class, .. } if class == "Ord")
            ),
            "alias must retain Ord: {errors:?}"
        );
        for (types, values) in [
            ("List Int, List Item", "sort [3, 1], sort [MkItem 1]"),
            ("List Item, List Int", "sort [MkItem 1], sort [3, 1]"),
        ] {
            let source = format!("import std.list as List\ntype Item = | MkItem Int\npub fn value () -> ({types}) =\n    {binding}\n    ({values})\n");
            let errors = check(&source);
            assert!(
                errors.iter().any(
                    |error| matches!(error, TypeError::TypeMismatchInCall { .. })
                        || matches!(error, TypeError::NoInstance { class, .. } if class == "Ord")
                ),
                "a prior specialization must not hide missing Ord: {errors:?}"
            );
        }
    }
}
