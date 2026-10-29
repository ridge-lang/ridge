//! Callback context selects a declared arity without conflating Unit payloads.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};

fn analyze(source: &str) -> ridge_typecheck::TypecheckResult {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"callbacks\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), source).unwrap();
    let discovery = discover_workspace(dir.path());
    assert!(discovery.manifest_errors.is_empty(), "{source}");
    assert!(discovery.resolve_errors.is_empty(), "{source}");
    let resolved = resolve_workspace(discovery.graph.unwrap());
    assert!(
        resolved.lex_errors.is_empty(),
        "{source}\n{:?}",
        resolved.lex_errors
    );
    assert!(
        resolved.parse_errors.is_empty(),
        "{source}\n{:?}",
        resolved.parse_errors
    );
    assert!(
        resolved.errors.is_empty(),
        "{source}\n{:?}",
        resolved.errors
    );
    typecheck_workspace(&resolved)
}

fn check(source: &str) -> Vec<TypeError> {
    analyze(source)
        .errors
        .into_iter()
        .map(|(_, error)| error)
        .collect()
}

#[test]
fn contextual_results_do_not_constrain_unreachable_values() {
    let header = "type Invoke = fn (fn () -> Bool) -> Bool\nfn choose (n: Int) (f: a) -> a = f\n";
    let callback = "(fn (f: fn () -> Bool) -> f ())";
    let other = "(fn (f: fn Text -> Bool) -> f \"unused\")";
    for body in [
        format!("\n    return {callback}\n    {other}"),
        format!("choose (return {callback}) {other}"),
        format!("choose (if true then (return {callback}) else (return {callback})) {other}"),
        format!("if (return {callback}) then {other} else {other}"),
        format!("match (return {callback})\n    true -> {other}\n    false -> {other}"),
    ] {
        let source = format!("{header}pub fn make () -> Invoke = {body}\n");
        let errors = check(&source);
        assert!(errors.is_empty(), "{source}\n{errors:?}");
    }
    // Unreachable values still have their own type obligations, and an actual
    // return operand must continue to meet the enclosing function's contract.
    for source in [
        format!("{header}pub fn make () -> Invoke =\n    return {callback}\n    fn (f: fn Text -> Bool) -> f 1\n"),
        format!("{header}pub fn make () -> Invoke = choose (return {other}) {callback}\n"),
    ] {
        let errors = check(&source);
        assert!(!errors.is_empty(), "{source}");
        assert!(!errors.iter().any(|error| matches!(error, TypeError::InternalTypeError { .. })), "{errors:?}");
    }
}

#[test]
fn contextual_actor_callbacks_retain_the_nullary_marker_type() {
    let source = "actor Counter =\n    on ping = ()\npub fn exercise (h: Handle Counter) -> Unit = List.forEach (fn target -> target ! ping ()) [h]\n";
    let result = analyze(source);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let module = &result.typed.modules[0];
    let (nodes, _) = ridge_resolve::assign_node_ids(&module.ast);
    let start = u32::try_from(source.find("ping ()").unwrap() + "ping ".len()).unwrap();
    let marker = nodes
        .get(
            ridge_ast::Span::new(start, start + 2),
            ridge_resolve::NodeKind::Expr,
        )
        .unwrap();
    assert!(
        matches!(&module.node_types[marker.0 as usize], Some(ridge_types::Type::Con(id, args)) if *id == result.typed.builtins.unit && args.is_empty())
    );
    assert!(!module.nullary_message_sites.is_empty());
}

#[test]
fn contextual_callbacks_use_signatures_arguments_and_immediate_application() {
    for source in [
        "fn zero -> Bool = true\npub fn exercise () -> List Bool = List.map (fn f -> f ()) [zero]\n",
        "fn zero -> Bool = true\nfn apply (g: fn (fn () -> Bool) -> Bool) -> Bool = g zero\npub fn exercise () -> Bool = apply (fn f -> f ())\n",
        "fn zero -> Bool = true\nfn apply (g: fn a -> Bool) (value: a) -> Bool = g value\npub fn exercise () -> Bool = apply (fn f -> f ()) zero\n",
        "fn zero -> Bool = true\nfn apply (value: a) (g: fn a -> Bool) -> Bool = g value\npub fn exercise () -> Bool = apply zero (fn f -> f ())\n",
        "pub fn exercise (f: fn () -> Bool) -> Bool = (fn g -> g ()) f\n",
        "fn zero -> Bool = true\npub fn exercise (flag: Bool) -> List Bool = List.map (if flag then (fn f -> f ()) else (fn f -> f ())) [zero]\n",
    ] {
        let errors = check(source);
        assert!(errors.is_empty(), "{source}\n{errors:?}");
    }
}

#[test]
fn contextual_callbacks_follow_bindings_returns_aliases_and_branches() {
    for source in [
        "pub fn exercise (value: fn () -> Bool) -> Bool =\n    let invoke: fn (fn () -> Bool) -> Bool = fn f -> f ()\n    invoke value\n",
        "pub fn make () -> fn (fn () -> Bool) -> Bool = fn f -> f ()\n",
        "type Callback = fn () -> Bool\ntype Invoke = fn Callback -> Bool\npub fn make () -> Invoke = fn f -> f ()\n",
        "pub fn make (flag: Bool) -> fn (fn () -> Bool) -> Bool = if flag then (fn f -> f ()) else (fn f -> f ())\n",
        "pub fn make () -> fn (fn () -> Bool) -> Bool =\n    let retained = true\n    fn f -> if retained then f () else false\n",
        "pub fn make () -> fn (fn () -> Bool) -> Bool = return (fn f -> f ())\n",
        "type Invoke = fn (fn () -> Bool) -> Bool\npub fn make () -> Option Invoke = Some (fn f -> f ())\n",
        "type Invoke = fn (fn () -> Bool) -> Bool\npub fn make () -> Result Invoke Text = Ok (fn f -> f ())\n",
        "pub fn exercise (value: fn () -> Bool) -> Bool =\n    var invoke: fn (fn () -> Bool) -> Bool = fn f -> f ()\n    invoke <- (fn f -> f ())\n    invoke value\n",
    ] {
        let errors = check(source);
        assert!(errors.is_empty(), "{source}\n{errors:?}");
    }
}

#[test]
fn contextual_callback_branches_preserve_return_and_propagation_order() {
    let source = "fn invoke (f: fn Unit -> Int) (n: Int) -> Int = f () + n\npub fn exercise () -> Result Int Text =\n    try\n        invoke (if (return Ok 7) then (fn _ -> 1) else (fn _ -> 2)) (Err \"later\" ?)\n    Ok 9\n";
    let errors = check(source);
    assert!(errors.is_empty(), "{source}\n{errors:?}");
}

#[test]
fn divergent_try_keeps_its_result_target_separate_from_its_value() {
    let base = "pub fn exercise () -> Result Int Text =\n    try\n        return Ok 7\n        Err \"later\" ?\n    Ok 9\n";
    let integer = "pub fn exercise () -> Int = try\n    return 7\n    let value: Int = Err \"later\" ?\n    value\n";
    for (source, integer_error, integer_value) in [
        (base.to_owned(), false, false),
        (base.replace("Err \"later\"", "Err 9"), true, false),
        (integer.to_owned(), false, true),
    ] {
        let result = analyze(&source);
        assert!(result.errors.is_empty(), "{source}\n{:?}", result.errors);
        let module = &result.typed.modules[0];
        assert_eq!(module.try_scope_types.len(), 1);
        let (span, target) = module.try_scope_types.iter().next().unwrap();
        let ridge_types::Type::Con(id, args) = target else {
            panic!("{target:?}")
        };
        assert_eq!(*id, result.typed.builtins.result);
        let error = if integer_error {
            result.typed.builtins.int
        } else {
            result.typed.builtins.text
        };
        assert!(matches!(&args[1], ridge_types::Type::Con(id, _) if *id == error));
        let (nodes, _) = ridge_resolve::assign_node_ids(&module.ast);
        let node = nodes.get(*span, ridge_resolve::NodeKind::Try).unwrap();
        let value = module.node_types[node.0 as usize].as_ref().unwrap();
        if integer_value {
            assert!(
                matches!(value, ridge_types::Type::Con(id, _) if *id == result.typed.builtins.int),
                "{value:?}"
            );
        } else {
            assert!(matches!(value, ridge_types::Type::Var(_)), "{value:?}");
        }
    }
    assert!(!check(&base.replace(
        "Err \"later\" ?",
        "let bad: Bool = 1\n        Err \"later\" ?"
    ))
    .is_empty());
}

#[test]
fn contextual_callbacks_preserve_unit_and_generalized_helper_contracts() {
    for source in [
        "fn one (_: Unit) -> Bool = true\npub fn exercise () -> List Bool = List.map (fn f -> f ()) [one]\n",
        "fn one (_: Unit) -> Bool = true\npub fn exercise () -> Bool =\n    let invoke = fn f -> f ()\n    invoke one\n",
    ] {
        let errors = check(source);
        assert!(errors.is_empty(), "{source}\n{errors:?}");
    }
    for source in [
        "fn zero -> Bool = true\npub fn exercise () -> Bool =\n    let invoke = fn f -> f ()\n    invoke zero\n",
        "fn one (_: Unit) -> Bool = true\nfn apply (f: fn () -> Bool) -> Bool = f ()\npub fn exercise () -> Bool = apply one\n",
        "fn zero -> Bool = true\nfn apply (f: fn Unit -> Bool) -> Bool = f ()\npub fn exercise () -> Bool = apply zero\n",
        "pub fn exercise (value: fn () -> Bool) -> Bool = (fn (f: fn Unit -> Bool) -> f ()) value\n",
        "fn zero -> Bool = true\npub fn exercise () -> Bool =\n    var invoke = fn f -> f ()\n    invoke zero\n",
        "type Book = { value: Int }\nfn zero -> Book = Book { value = 7 }\nfn apply (f: fn (fn () -> Bool) -> Bool) -> Bool = f (fn () -> true)\npub fn exercise () -> Bool = apply (fn f -> (fn g -> g ()) zero)\n",
    ] {
        let errors = check(source);
        assert!(!errors.is_empty(), "{source}");
        assert!(errors.iter().all(|error| !matches!(error, TypeError::InternalTypeError { .. })), "{errors:?}");
    }
}

#[test]
fn contextual_callbacks_keep_latent_permissions() {
    for arity in ["()", "Unit"] {
        let source = format!("type Action = fn io {arity} -> Bool\nfn io apply (f: fn io Action -> Bool) (action: Action) -> Bool = f action\npub fn blocked (action: Action) -> Bool = apply (fn f -> f ()) action\n");
        let errors = check(&source);
        assert!(
            errors
                .iter()
                .any(|error| matches!(error, TypeError::CapabilityNotDeclared { .. })),
            "{source}\n{errors:?}"
        );
        let allowed = source.replace("pub fn blocked", "pub fn io allowed");
        let errors = check(&allowed);
        assert!(errors.is_empty(), "{allowed}\n{errors:?}");
    }
}
