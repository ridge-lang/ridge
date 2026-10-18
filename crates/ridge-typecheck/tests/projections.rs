//! Written projections must name a type in their receiver domain.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};

fn check(source: &str) -> Vec<TypeError> {
    check_files(&[("Main", source)])
}

fn check_files(files: &[(&str, &str)]) -> Vec<TypeError> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"projections\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    for (name, source) in files {
        std::fs::write(dir.path().join(format!("app/src/{name}.ridge")), source).unwrap();
    }
    let discovered = discover_workspace(dir.path());
    assert!(
        discovered.manifest_errors.is_empty(),
        "{:?}",
        discovered.manifest_errors
    );
    assert!(
        discovered.resolve_errors.is_empty(),
        "{:?}",
        discovered.resolve_errors
    );
    let resolved = resolve_workspace(discovered.graph.unwrap());
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
fn concrete_projection_domains_are_checked_where_written() {
    for (projection, class) in [
        ("Rows Int", "Fetchable"),
        ("InsertShape Int", "HasSchema"),
        ("JoinCond Int Text", "Joinable"),
        ("JoinResult Int Text", "Joinable"),
        ("LeftJoinResult Int Text", "LeftJoinable"),
        ("RightJoinResult Int Text", "RightJoinable"),
        ("FullJoinResult Int Text", "FullJoinable"),
    ] {
        let source = format!("pub fn annotated (x: {projection}) -> Int = 1\n");
        let errors = check(&source);
        assert!(
            errors.iter().any(|error| matches!(error,
                TypeError::NoInstance { class: found, span, .. }
                if found == class && &source[span.start as usize..span.end as usize] == projection
            )),
            "{projection}: {errors:?}"
        );
    }
    let errors = check("pub fn annotated (x: Ret Int) -> Int = 1\n");
    assert!(errors.iter().any(|e| matches!(e, TypeError::InvalidTypeProjection { detail, .. } if detail.contains("function"))), "{errors:?}");
}

#[test]
fn nested_alias_and_non_function_annotation_sites_are_checked() {
    for source in [
        "pub type Bad = Rows Int\n",
        "pub type Bad = { value: Rows Int }\n",
        "pub type Bad = Wrap Int\ntype Wrap a = Rows a\n",
        "pub type Bad = { value: Wrap Int }\ntype Wrap a = Rows a\n",
        "type Pair a = ({ value: Rows a }, Int)\npub fn bad (x: Pair Int) -> Int = 1\n",
        "pub fn bad (x: List (Rows Int)) -> Int = 1\n",
        "pub fn bad () -> Int =\n    let f = fn (x: Rows Int) -> 1\n    1\n",
        "actor Bad =\n    state value: Rows Int\n    init (x: Rows Int) =\n        value <- x\n    on count () -> Int = 1\n",
    ] {
        let errors = check(source);
        assert!(errors.iter().any(|e| matches!(e, TypeError::NoInstance { class, .. } if class == "Fetchable")), "{source}: {errors:?}");
    }
}

#[test]
fn specializing_an_open_projection_checks_the_concrete_receiver() {
    for projection in ["Rows", "InsertShape"] {
        let source = format!("pub fn same (witness: q) (x: {projection} q) -> {projection} q = x\npub fn bad () -> Int = same 1 2\n");
        let errors = check(&source);
        assert!(errors.iter().any(|error| matches!(error, TypeError::NoInstance { span, .. } if span.start as usize == source.rfind("same").unwrap())), "{projection}: {errors:?}");
    }
}

#[test]
fn open_and_reducible_projections_remain_valid() {
    for source in [
        "pub fn same (x: Rows q) -> Rows q where Fetchable q = x\n",
        "pub fn same (x: InsertShape e) -> InsertShape e where HasSchema e = x\n",
        "pub fn same (x: Ret p) -> Ret p = x\n",
        "pub fn same (x: Ret (Rows q)) -> Ret (Rows q) = x\n",
        "pub fn same (x: Ret (fn Int -> Text)) -> Text = x\n",
        "type User = { name: Text } deriving (Schema)\npub fn same (x: InsertShape User) -> User = x\n",
        "type User = { name: Text }\npub fn same (x: Rows (Seq User)) -> User = x\n",
        "import std.repo (Seq)\ntype User = { name: Text }\ntype Items = Seq User\npub fn same (x: Rows Items) -> User = x\n",
        "type Handler = fn Int -> Text\npub fn same (x: Ret Handler) -> Text = x\npub fn use () -> Text = same \"ok\"\n",
        "type User = { name: Text } deriving (Schema)\ntype Entity = User\npub fn same (x: InsertShape Entity) -> User = x\n",
    ] {
        let errors = check(source);
        assert!(errors.is_empty(), "{source}: {errors:?}");
    }
}

#[test]
fn an_open_projection_at_a_use_does_not_hide_an_invalid_sibling() {
    let source = "pub fn same (p: p) (q: q) (x: Rows p) (y: Rows q) -> Int = 1\npub fn bad (w: q) (x: Rows q) -> Int = same w 1 x 2\n";
    let errors = check(source);
    assert!(errors.iter().any(|error| matches!(error, TypeError::NoInstance { span, .. } if span.start as usize == source.rfind("same").unwrap())), "{errors:?}");
}

#[test]
fn projection_arity_is_not_silently_accepted() {
    for projection in ["Rows", "Rows Int Text", "Ret", "JoinResult Int"] {
        let errors = check(&format!("pub fn bad (x: {projection}) -> Int = 1\n"));
        assert!(errors.iter().any(|e| matches!(e, TypeError::InvalidTypeProjection { detail, .. } if detail.contains("argument"))), "{projection}: {errors:?}");
    }
}

#[test]
fn record_and_union_specializations_preserve_the_projection_domain() {
    for source in [
        "pub type Box a = { value: InsertShape a }\npub fn bad () -> Box Int = Box { value = 42 }\n",
        "pub type Box a = { value: InsertShape a }\npub fn bad (box: Box Int) -> Int = box.value\n",
        "pub type Box a = { value: InsertShape a }\npub fn bad (box: Box Int) -> Box Int = box with { value = 42 }\n",
        "pub type Box a = { value: InsertShape a }\npub fn bad (box: Box Int) -> Int =\n    match box\n        Box { value } -> value\n",
        "pub type Holder a = | Wrapped (InsertShape a)\npub fn bad () -> Holder Int = Wrapped 42\n",
        "pub type Holder a = | Wrapped (InsertShape a)\npub fn bad () -> Holder Int =\n    let make = Wrapped\n    make 42\n",
        "pub type Holder a = | Wrapped (InsertShape a)\npub fn bad (box: Holder Int) -> Int =\n    match box\n        Wrapped value -> value\n",
        "pub type Holder a = | Wrapped { value: InsertShape a }\npub fn bad () -> Holder Int = Wrapped { value = 42 }\n",
        "pub type Box a = { value: { payload: InsertShape a } }\npub fn bad () -> Box Int = Box { value = { payload = 42 } }\n",
    ] {
        let errors = check(source);
        assert!(errors.iter().any(|e| matches!(e, TypeError::NoInstance { class, .. } if class == "HasSchema")), "{source}: {errors:?}");
    }
    let errors = check("pub type User = { name: Text } deriving (Schema)\npub type Box a = { value: InsertShape a }\npub fn good () -> Box User = Box { value = User { name = \"ok\" } }\n");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn alias_expansion_preserves_cycle_checks_inside_structural_records() {
    let errors = check("type Loop = ({ value: Loop }, Int)\npub fn bad (x: Loop) -> Int = 1\n");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, TypeError::RecursiveTypeAlias { .. })),
        "{errors:?}"
    );
}

#[test]
fn parentheses_do_not_introduce_a_second_projection_site() {
    let source = "pub fn bad (x: ((Rows Int))) -> Int = 1\n";
    let errors = check(source);
    let start = source.find("Rows Int").unwrap();
    let projections: Vec<_> = errors
        .iter()
        .filter_map(|e| match e {
            TypeError::NoInstance { class, span, .. } if class == "Fetchable" => Some(span),
            _ => None,
        })
        .collect();
    assert!(!projections.is_empty(), "{errors:?}");
    for span in projections {
        assert_eq!(span.start as usize, start);
        assert_eq!(span.end as usize, start + "Rows Int".len());
    }
}

#[test]
fn imported_class_signature_diagnostics_belong_to_the_declaring_module() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"projections\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/src/Types.ridge"),
        "pub class Probe a =\n    probe (x: a) -> Rows Int\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/src/Main.ridge"),
        "pub fn value () -> Int = 1\n",
    )
    .unwrap();
    let discovered = discover_workspace(dir.path());
    assert!(discovered.manifest_errors.is_empty());
    assert!(discovered.resolve_errors.is_empty());
    let resolved = resolve_workspace(discovered.graph.unwrap());
    assert!(
        resolved.parse_errors.is_empty(),
        "{:?}",
        resolved.parse_errors
    );
    assert!(resolved.errors.is_empty(), "{:?}", resolved.errors);
    let result = typecheck_workspace(&resolved);
    assert!(
        result
            .errors
            .iter()
            .any(|(_, e)| matches!(e, TypeError::NoInstance { class, .. } if class == "Fetchable")),
        "{:?}",
        result.errors
    );
    for (mid, error) in result.errors {
        if matches!(error, TypeError::NoInstance { .. }) {
            assert!(
                resolved.graph.modules[mid.0 as usize]
                    .file_path
                    .ends_with("Types.ridge"),
                "{error:?}"
            );
        }
    }
}

const CONDITIONED_SCHEMA: &str = r#"
import std.schema (HasSchema, EntitySchema, schema)
import std.sql (Row, SqlValue)
import std.map as Map

pub type Box a = { value: a }
pub type User = { name: Text }

instance HasSchema (Box a) where Row a =
    schemaOf (_witness: Option (Box a)) -> EntitySchema (Box a) = schema "Box" "boxes"
    toInsertRow (_shape: InsertShape (Box a)) -> Map Text SqlValue = Map.empty

pub fn through (x: InsertShape (Box a)) -> Box a = x
pub fn relay (x: Box a) -> Box a = through x
"#;

#[test]
fn fixed_receiver_heads_must_satisfy_the_instance_context() {
    for source in [
        "pub fn bad (x: Rows (Seq Int)) -> Int = x\n".to_owned(),
        format!("{CONDITIONED_SCHEMA}\npub fn bad (x: InsertShape (Box (fn a -> a))) -> Int = 1\n"),
    ] {
        let errors = check(&source);
        assert!(
            errors.iter().any(|error| matches!(error,
                TypeError::NoInstance { class, .. } if class == "Row"
            )),
            "{source}: {errors:?}"
        );
        if source.contains("(fn a -> a)") {
            assert!(errors.iter().any(|error| matches!(error,
                TypeError::NoInstance { class, ty, .. } if class == "Row" && ty.render(&[]).contains("->") && !ty.render(&[]).contains("Fn1")
            )), "the diagnostic must describe the function type: {errors:?}");
        }
    }
}

#[test]
fn recursive_class_promises_cover_their_method_projection_domains() {
    let errors = check("import std.repo (Fetchable, toList)\npub fn count (witness: q) (n: Int) -> Int where Fetchable q =\n    let rows = toList witness\n    if n == 0 then 0 else count witness (n - 1)\n");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn open_arguments_do_not_hide_an_unsupported_projection_head() {
    let source = "import std.repo (Fetchable)\nimport std.data (DbError)\npub type Box a = { value: a }\ninstance Fetchable (Box a) =\n    toList (_box: Box a) -> Result (List (Rows (Box a))) DbError = Ok []\n    first (_box: Box a) -> Result (Option (Rows (Box a))) DbError = Ok None\n";
    let errors = check(source);
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, TypeError::InvalidTypeProjection { .. })),
        "{errors:?}"
    );
    let source = "import std.repo (Joined)\ntype User = { name: Text }\npub fn bad (x: JoinCond (Joined Int User a) f) -> Int = 1\n";
    let errors = check(source);
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, TypeError::InvalidTypeProjection { .. })),
        "{errors:?}"
    );
}

#[test]
fn normalized_requirements_survive_a_recursive_declaration_group() {
    let source = format!(
        r"{CONDITIONED_SCHEMA}
pub fn first (x: Box a) -> Box a = second x
pub fn second (x: Box a) -> Box a =
    let next = first
    through x
pub fn bad () -> Box Int = first (Box {{ value = 42 }})
"
    );
    let errors = check(&source);
    assert!(
        errors.iter().any(|error| matches!(error,
            TypeError::MissingConstraint { class, decl, .. } if class == "Row" && decl == "second"
        )),
        "{errors:?}"
    );
    let explicit = source.replace(
        "(x: Box a) -> Box a =",
        "(x: InsertShape (Box a)) -> Box a =",
    );
    let errors = check(&explicit);
    assert!(
        errors.iter().any(|error| matches!(error,
            TypeError::NoInstance { class, .. } if class == "Row"
        )),
        "{errors:?}"
    );
    let valid = explicit.replace(
        "pub fn bad () -> Box Int = first (Box { value = 42 })",
        "pub fn good () -> Box User = first (Box { value = User { name = \"Ada\" } })",
    );
    assert!(check(&valid).is_empty());
    let promised = source
        .replace("(x: Box a) -> Box a =", "(x: Box a) -> Box a where Row a =")
        .replace(
            "pub fn bad () -> Box Int = first (Box { value = 42 })",
            "pub fn good () -> Box User = first (Box { value = User { name = \"Ada\" } })",
        );
    let errors = check(&promised);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn normalized_generic_projections_keep_their_domain_at_calls_and_exports() {
    for callee in ["through", "relay"] {
        let call = format!("pub fn bad () -> Box Int = {callee} (Box {{ value = 42 }})\n");
        let source = format!("{CONDITIONED_SCHEMA}\n{call}");
        let errors = check(&source);
        assert!(
            errors.iter().any(|error| matches!(error,
                TypeError::NoInstance { class, span, .. }
                if class == "Row" && span.start as usize == source.rfind(callee).unwrap()
            )),
            "{callee}: {errors:?}"
        );

        let consumer = format!("import app.producer (Box, {callee})\n{call}");
        let errors = check_files(&[("producer", CONDITIONED_SCHEMA), ("consumer", &consumer)]);
        assert!(
            errors.iter().any(|error| matches!(error,
                TypeError::NoInstance { class, span, .. }
                if class == "Row" && span.start as usize == consumer.rfind(callee).unwrap()
            )),
            "import {callee}: {errors:?}"
        );
    }
}

#[test]
fn normalized_requirements_allow_valid_specializations_and_unrelated_generics() {
    let valid = format!(
        r#"{CONDITIONED_SCHEMA}
pub fn same (x: a) -> a = x
pub fn good () -> Box User = relay (Box {{ value = User {{ name = "Ada" }} }})
pub fn unrelated () -> Int = same 42
pub fn viaLet () -> Box User =
    let call = relay
    call (Box {{ value = User {{ name = "Ada" }} }})
"#
    );
    let errors = check(&valid);
    assert!(errors.is_empty(), "{errors:?}");
    let invalid = format!(
        r"{CONDITIONED_SCHEMA}
pub fn bad () -> Box Int =
    let call = relay
    call (Box {{ value = 42 }})
"
    );
    let errors = check(&invalid);
    assert!(
        errors.iter().any(|error| matches!(error,
            TypeError::NoInstance { class, .. } if class == "Row"
        )),
        "{errors:?}"
    );
}
