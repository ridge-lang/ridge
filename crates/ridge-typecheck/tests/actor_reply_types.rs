//! A declared reply type constrains the value produced by a handler.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError, TypecheckResult};

fn check(source: &str) -> TypecheckResult {
    let (result, errors) = check_with_resolution(source);
    assert!(errors.is_empty(), "{source}\n{errors:?}");
    result
}

#[test]
fn explicit_message_inventory_follows_actor_aliases_inside_handles() {
    let result = check("actor Counter =\n    on ping = ()\ntype Target = Counter\npub fn exercise (h: Handle Target) -> Unit = List.forEach (fn target -> target ! ping) [h]\n");
    assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
    assert_eq!(result.errors[0].1.code(), "T063");
    let sites = &result.typed.modules[0].message_call_sites;
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].replacement().as_deref(), Some("ping ()"));
    let migrated = check("actor Counter =\n    on ping = ()\ntype Target = Counter\npub fn exercise (h: Handle Target) -> Unit = List.forEach (fn target -> target ! ping ()) [h]\n");
    assert!(migrated.errors.is_empty(), "{:?}", migrated.errors);
    let module = &migrated.typed.modules[0];
    assert!(module
        .nullary_message_sites
        .contains(&module.message_call_sites[0].operation));
}

#[test]
fn function_type_aliases_preserve_latent_permissions() {
    for parameter in ["Unit", "()"] {
        let source = format!(
            "type Action = fn io {parameter} -> Unit\npub fn blocked (f: Action) -> Unit = f ()\n"
        );
        let result = check(&source);
        assert!(
            result
                .errors
                .iter()
                .any(|(_, error)| matches!(error, TypeError::CapabilityNotDeclared { .. })),
            "{source}\n{:?}",
            result.errors
        );
        let allowed = check(&source.replace("pub fn blocked", "pub fn io approved"));
        assert!(allowed.errors.is_empty(), "{:?}", allowed.errors);
    }
}

#[test]
fn function_callbacks_keep_zero_parameters_distinct_from_unit() {
    let result = check("fn zero -> Bool = true\nfn one (_: Unit) -> Bool = true\npub fn exercise () -> Unit =\n    let _ = List.map (fn (f: fn () -> Bool) -> f ()) [zero]\n    let _ = List.map (fn f -> f ()) [one]\n");
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn explicit_message_guidance_does_not_duplicate_invalid_generic_receivers() {
    for (source, expected) in [
        ("pub fn send (h: a) -> Unit = h ! ping\n", "T020"),
        ("pub fn time query (h: a) = h ?> ping\n", "T021"),
    ] {
        let result = check(source);
        assert!(
            result.errors.iter().any(|(_, e)| e.code() == expected),
            "{:?}",
            result.errors
        );
        assert!(
            !result.errors.iter().any(|(_, e)| e.code() == "T063"),
            "{:?}",
            result.errors
        );
    }
}

#[test]
fn explicit_message_inventory_uses_selected_arity_and_rechecks_edits() {
    let source = r"-- café 🧭
import std.actor as A
import std.actor (AskError)
actor Counter =
    on ping = ()
    on take (_: Unit) = ()
actor Other =
    on ping (_: Unit) = ()
actor Relay =
    on forward (target: Handle Counter) = target ! ping
pub fn exercise (h: Handle Counter) (other: Handle Other) -> Unit =
    h ! ping
    h ! ping ()
    h ! take ()
    other ! ping ()
    let _ = A.send h ping
    let _ = A.send h (ping ())
    List.forEach (fn target -> target ! ping) [h]
    List.forEach (fn target -> target ! take ()) [h]
pub fn time query (h: Handle Counter) -> Result Unit AskError = h ?> ping
pub fn time safeQuery (h: Handle Counter) -> Result Unit AskError = A.tryAsk h ping 1000
";
    for ending in ["\n", "\r\n"] {
        let source = source.replace('\n', ending);
        let normalised = source.replace("\r\n", "\n").replace('\r', "\n");
        let result = check(&source);
        assert_eq!(result.errors.len(), 6, "{:?}", result.errors);
        assert!(result
            .errors
            .iter()
            .all(|(_, error)| error.code() == "T063"));
        let sites = &result.typed.modules[0].message_call_sites;
        assert!(sites.iter().all(|site| site.parameter_count.is_some()));
        let mut edits: Vec<_> = sites
            .iter()
            .filter_map(|site| site.replacement().map(|text| (site.message, text)))
            .collect();
        let changes: Vec<_> = sites
            .iter()
            .filter_map(|site| {
                site.replacement().map(|text| {
                    (
                        normalised[site.operation.start as usize..site.operation.end as usize]
                            .to_owned(),
                        text,
                    )
                })
            })
            .collect();
        assert_eq!(
            changes,
            [
                ("target ! ping".to_owned(), "ping ()".to_owned()),
                ("h ! ping".to_owned(), "ping ()".to_owned()),
                ("A.send h ping".to_owned(), "(ping ())".to_owned()),
                ("target ! ping".to_owned(), "ping ()".to_owned()),
                ("h ?> ping".to_owned(), "ping ()".to_owned()),
                ("A.tryAsk h ping 1000".to_owned(), "(ping ())".to_owned()),
            ]
        );
        edits.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
        let mut migrated = normalised;
        for (span, text) in edits {
            migrated.replace_range(span.start as usize..span.end as usize, &text);
        }
        let migrated = check(&migrated);
        assert!(migrated.errors.is_empty(), "{:?}", migrated.errors);
        assert!(migrated.typed.modules[0]
            .message_call_sites
            .iter()
            .all(|site| site.replacement().is_none()));
    }
}

fn check_with_resolution(source: &str) -> (TypecheckResult, Vec<ridge_resolve::ResolveError>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"replies\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/src/Main.ridge"), source).unwrap();
    let resolved = resolve_workspace(discover_workspace(dir.path()).graph.unwrap());
    assert!(resolved.lex_errors.is_empty(), "{:?}", resolved.lex_errors);
    assert!(
        resolved.parse_errors.is_empty(),
        "{source}\n{:?}",
        resolved.parse_errors
    );
    let errors = resolved
        .errors
        .iter()
        .map(|(_, error)| error.clone())
        .collect();
    (typecheck_workspace(&resolved), errors)
}

fn assert_reply_mismatch(source: &str) {
    let result = check(source);
    assert!(
        result
            .errors
            .iter()
            .any(|(_, error)| matches!(error, TypeError::TypeMismatch { .. })),
        "{source}\n{:?}",
        result.errors
    );
}

#[test]
fn declared_handler_replies_reject_incompatible_values() {
    for source in [
        "actor Counter =\n    on value () -> Int = \"wrong\"\n",
        "actor Counter =\n    on value (x: Int) -> Text = x\n",
        "actor Counter =\n    on value () -> Result Int Text = Ok \"wrong\"\n",
        "actor Counter =\n    on value () -> Result Int Text = Err 7\n",
        "type Book = { value: Int }\nactor Counter =\n    on value () -> Book = 7\n",
        "fn answer () -> Text = \"wrong\"\nactor Counter =\n    on value () -> Int = answer ()\n",
        "actor Counter =\n    on value (b: Bool) -> Int = if b then \"first\" else \"second\"\n",
        "actor Counter =\n    on value (b: Bool) -> Int =\n        match b\n            true -> \"first\"\n            false -> \"second\"\n",
        "actor Counter =\n    on value () -> Unit = 7\n",
    ] {
        assert_reply_mismatch(source);
    }
    let source = "actor Counter =\n    on value () -> Int = \"wrong\"\n";
    let result = check(source);
    let (_, error) = result
        .errors
        .iter()
        .find(|(_, e)| matches!(e, TypeError::TypeMismatch { .. }))
        .unwrap();
    let TypeError::TypeMismatch { span, hint, .. } = error else {
        unreachable!()
    };
    let body = source.find("\"wrong\"").unwrap();
    assert_eq!((span.start as usize, span.end as usize), (body, body + 7));
    assert!(hint.as_ref().unwrap().contains("handler `value`"));
    let message = error.render(&result.typed.tycons);
    assert!(
        message.contains("Int") && message.contains("Text"),
        "{message}"
    );
}

#[test]
fn reply_contract_preserves_nominal_identity_and_callback_capabilities() {
    assert_reply_mismatch("type Book = { value: Int }\ntype Other = { value: Int }\nactor Counter =\n    on value () -> Book = Other { value = 7 }\n");
    let invalid = "actor Relay =\n    on value (f: fn io Int -> Int) -> fn Int -> Int = f\n";
    assert!(
        !check(invalid).errors.is_empty(),
        "a reply cannot erase callback capabilities"
    );
    let valid = "actor Relay =\n    on value (f: fn io Int -> Int) -> fn io Int -> Int = f\n";
    assert!(check(valid).errors.is_empty());
}

#[test]
fn valid_replies_keep_state_binding_and_nested_return_scopes() {
    for source in [
        "actor Counter =\n    state n: Int = 0\n    on value (b: Bool) -> Int =\n        if b then\n            n <- n + 1\n            n\n        else\n            n <- n + 2\n            n\n",
        "actor Counter =\n    on value () -> Int =\n        let f = fn () -> Int = return 7\n        f ()\n",
        "actor Counter =\n    on value () -> Int =\n        fn answer () -> Int = return 7\n        answer ()\n",
        "actor Counter =\n    on value () -> Result Int Text = Ok 7\n",
        "type Book = { value: Int }\nactor Counter =\n    on value () -> Book = Book { value = 7 }\n",
        "actor Counter =\n    on value () -> Int =\n        var x = 1\n        x <- x + 1\n        x\n",
        "actor Counter =\n    on value () = 7\n",
        "import std.io as Io\nactor Counter =\n    on io value () -> Unit = Io.println \"value\"\n",
    ] {
        let result = check(source);
        assert!(result.errors.is_empty(), "{source}\n{:?}", result.errors);
    }
}

#[test]
fn reply_checks_keep_body_permissions_and_propagation_rules() {
    for source in [
        "import std.io as Io\nactor Counter =\n    on value () -> Unit = Io.println \"value\"\n",
        "actor Counter =\n    state n: Int = 0\n    on value () -> Unit = n <- \"wrong\"\n",
    ] {
        assert!(!check(source).errors.is_empty(), "{source}");
    }
    let (_, errors) = check_with_resolution(
        "actor Counter =\n    on value () -> Int =\n        let n = 1\n        n <- 7\n        n\n",
    );
    assert!(errors
        .iter()
        .any(|e| matches!(e, ridge_resolve::ResolveError::ImmutableAssignment { .. })));
    let prefix = "import std.actor (AskError)\nactor Source =\n    on value () -> Int = 7\n";
    let invalid = format!("{prefix}actor Relay =\n    on time value (h: Handle Source) -> Result Int AskError =\n        let n = h ?> value () ?\n        Ok n\n");
    assert!(check(&invalid)
        .errors
        .iter()
        .any(|(_, e)| e.code() == "T058"));
    let valid = format!("{prefix}fn time get (h: Handle Source) -> Result Int AskError =\n    let n = h ?> value () ?\n    Ok n\nactor Relay =\n    on time value (h: Handle Source) -> Result Int AskError = get h\n");
    assert!(check(&valid).errors.is_empty());
}
