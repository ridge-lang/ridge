//! Typed predicate domain, arity, effects, and imported identity.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};

fn check(files: &[(&str, &str)]) -> Vec<TypeError> {
    let resolved = resolve(files);
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

fn resolve(files: &[(&str, &str)]) -> ridge_resolve::ResolvedWorkspace {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"predicate\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/ridge.toml"), "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"io\"]\n").unwrap();
    for (name, source) in files {
        std::fs::write(dir.path().join(format!("app/src/{name}.ridge")), source).unwrap();
    }
    resolve_workspace(discover_workspace(dir.path()).graph.unwrap())
}

#[test]
fn root_payload_elision_and_explicit_nested_shapes_are_typed() {
    let source = r"
type Shape = | Square Int | Point Int Int | Circle { radius: Int }
type User = { age: Int, active: Bool }
pub fn square (shape: Shape) -> Bool = shape is Square
pub fn point (shape: Shape) -> Bool = shape is Point
pub fn circle (shape: Shape) -> Bool = shape is Circle
pub fn radius (shape: Shape) -> Bool = shape is Circle { radius = 5 }
pub fn user (value: User) -> Bool = value is User
pub fn adult (value: User) -> Bool = value is User { age = 18, .. }
pub fn nested (value: Option Shape) -> Bool = value is Some (Square _)
pub fn tuple (value: (Int, Bool)) -> Bool = value is (5, _)
pub fn list (value: List Int) -> Bool = value is [_, .., _]
pub fn inferred value -> Bool = value is Some _
pub fn predicate value -> Bool = value is 5
";
    let errors = check(&[("Main", source)]);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn typed_predicate_does_not_require_equality() {
    let errors = check(&[("Main", "type Action = | Ready (Unit -> Int) | Idle\npub fn probe (action: Action) -> Bool = action is Ready\n")]);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn stdlib_constructor_identity_is_available_without_value_schemes() {
    let source = "import std.actor as Actor\npub fn probe value -> Bool = value is Actor.Crashed\n";
    let errors = check(&[("Main", source)]);
    assert!(errors.is_empty(), "{errors:?}");
    let source = "import std.actor (Crashed)\npub fn probe value -> Bool = value is Crashed\n";
    let errors = check(&[("Main", source)]);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn unrelated_types_nested_arity_and_ordinary_match_stay_errors() {
    for source in [
        "type Shape = | Square Int | Circle Int\npub fn probe () -> Bool = 5 is Square\n",
        "type Shape = | Square Int | Circle Int\npub fn probe (s: Option Shape) -> Bool = s is Some Square\n",
        "type Shape = | Square Int | Circle Int\npub fn probe (s: Shape) -> Bool =\n    match s\n        Square -> true\n        _ -> false\n",
        "type Shape = | Square Int | Circle Int\npub fn probe (s: Shape) -> Bool = s is Square true\n",
        "type Shape = | Square Int | Circle Int\npub fn probe (s: Shape) -> Bool = s is Shape\n",
        "type Shape = | Square Int | Circle Int\npub fn probe (s: Shape) -> Bool = s is Shape { .. }\n",
        "type Shape = | Square Int | Circle Int\npub fn probe (s: Shape) -> Bool = s is Circle { radius = 5 }\n",
        "pub fn probe () -> Bool = 5 is Int\n",
    ] {
        let errors = check(&[("Main", source)]);
        assert!(!errors.is_empty(), "accepted {source}");
        assert!(!errors.iter().any(|error| error.code() == "T999"), "{source}: {errors:?}");
    }
}

#[test]
fn qualified_constructor_identity_does_not_follow_same_spelling() {
    let a = "pub type Shape = | Square Int | Circle\npub fn make () -> Shape = Square 5\n";
    let b = "pub type Shape = | Square Bool | Circle\npub fn make () -> Shape = Square true\n";
    for (source, valid) in [
        ("import app.a as A\nimport app.b as B\npub fn probe () -> Bool = A.make () is A.Square\n", true),
        ("import app.a as A\nimport app.b as B\npub fn probe () -> Bool = A.make () is B.Square\n", false),
        ("import app.a as A\nimport app.b as B\npub fn probe () -> Bool = B.make () is B.Square true\n", true),
        ("import app.a as A\nimport app.b as B\npub fn probe () -> Bool = B.make () is B.Square 5\n", false),
    ] {
        let errors = check(&[("a", a), ("b", b), ("Main", source)]);
        assert_eq!(errors.is_empty(), valid, "{source}: {errors:?}");
    }
}

#[test]
fn subject_capabilities_are_preserved() {
    let source = "import std.io as Io\nfn io observed () -> Int =\n    Io.println \"observed\"\n    5\npub fn probe () -> Bool = observed () is 5\n";
    let errors = check(&[("Main", source)]);
    assert!(!errors.is_empty(), "subject effects escaped");
    let errors = check(&[("Main", &source.replace("pub fn probe", "pub fn io probe"))]);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn opaque_constructor_patterns_do_not_escape_their_module() {
    let hidden = "pub opaque type Secret = | Hidden Int\npub fn make () -> Secret = Hidden 7\n";
    let source = "import app.Hidden as H\npub fn probe () -> Bool = H.make () is H.Hidden\n";
    let resolved = resolve(&[("Hidden", hidden), ("Main", source)]);
    assert!(
        resolved.parse_errors.is_empty(),
        "{:?}",
        resolved.parse_errors
    );
    assert!(
        !resolved.errors.is_empty(),
        "opaque constructor was exposed"
    );
    let source = source.replace("is H.Hidden", "is _");
    let errors = check(&[("Hidden", hidden), ("Main", &source)]);
    assert!(errors.is_empty(), "{errors:?}");
}
