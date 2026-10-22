//! Inferred parameter patterns retain typing, irrefutability and lowering.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

fn diagnostics(source: &str) -> Vec<(String, String)> {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("apps/demo/src")).unwrap();
    for (path, content) in [
        (
            "ridge.toml",
            "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
        ),
        (
            "apps/demo/ridge.toml",
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
        ),
        ("apps/demo/src/Main.ridge", source),
    ] {
        std::fs::write(workspace.path().join(path), content).unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    compile_workspace(
        CompileOptions::new(workspace.path().to_owned())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache.path().to_owned()),
    )
    .unwrap()
    .diagnostics
    .iter()
    .map(|d| (d.code.to_owned(), d.primary_message.clone()))
    .collect()
}

#[test]
fn inferred_named_patterns_typecheck_and_lower_in_all_body_positions() {
    let source = r#"
import std.float as Float

type Point = { x: Int, y: Int }
type Box = | MkBox Int

fn sum (x, y) -> Int = x + y
fn combine (x, y) (a, b) -> Int = x + y + a + b
fn record (Point { x, y }) -> Int = x + y
fn unbox (MkBox n) -> Int = n
fn nested ((x, y), z) -> Int = x + y + z
fn first (x, _) = x
fn identity (value) = value
fn all ([xs @ ..]) = xs
fn alias (pair @ (x, y)) -> Int =
    let (a, b) = pair
    a + b + x + y
fn recurse (n, total) -> Int =
    if n <= 0 then total else recurse (n - 1, total + n)

fn distance (x1, y1) (x2, y2) -> Float =
    let dx = x2 - x1
    let dy = y2 - y1
    Float.sqrt (dx * dx + dy * dy)

pub fn use () -> Int =
    fn inner (x, y) -> Int = x + y
    let partial = combine (1, 2)
    let text: Text = first ("kept", 1)
    let number: Int = first (3, true)
    let values: List Int = all [1, 2]
    inner (1, 2) + sum (3, 4) + partial (5, 6) + nested ((1, 2), 3) + number + recurse (3, 0)

class Total a =
    total (value: a) -> Int

instance Total Point =
    total (Point { x, y }) -> Int = x + y

pub fn method () -> Int = total (Point { x = 2, y = 3 })

actor Counter =
    state count: Int = 0
    init (x, y) =
        count <- x + y
    on add (x, y) =
        count <- count + x + y
    on value () -> Int = count
    terminate (reason) =
        let _ = reason
        ()
    onDown (monitor) (reason) =
        let _ = monitor
        let _ = reason
        ()
"#;
    let errors = diagnostics(source);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn inferred_named_patterns_still_reject_refutable_binders() {
    for source in [
        "fn partial (0) -> Int = 0\n",
        "fn partial (true) -> Int = 0\n",
        "fn partial (0, n) -> Int = n\n",
        "fn partial ([x]) = x\n",
        "fn partial (x :: xs) = x\n",
        "type Choice = A Int | B\nfn partial (A n) -> Int = n\n",
        "type Point = { x: Int }\nfn partial (Point { x = 0 }) -> Int = 0\n",
    ] {
        let errors = diagnostics(source);
        assert!(
            errors.iter().any(|(code, _)| code == "T043"),
            "{source}: {errors:?}"
        );
    }
}

#[test]
fn inferred_named_patterns_retain_argument_types_and_duplicate_binding_checks() {
    let wrong = diagnostics("fn sum (x, y) -> Int = x + y\npub fn use () -> Int = sum (1, true)\n");
    assert!(wrong.iter().any(|(code, _)| code == "T002"), "{wrong:?}");
    let duplicate = diagnostics("fn sum (x, x) -> Int = x\n");
    assert!(
        duplicate.iter().any(|(code, _)| code == "R011"),
        "{duplicate:?}"
    );
    let capability =
        diagnostics("import std.io as Io\nfn printPair (x, y) -> Unit = Io.println x\n");
    assert!(
        capability.iter().any(|(code, _)| code == "T014"),
        "{capability:?}"
    );
}

#[test]
fn inferred_actor_callback_patterns_keep_the_runtime_input_types() {
    for source in [
        "actor Counter =\n    terminate (x, y) =\n        ()\n",
        "actor Counter =\n    onDown (x, y) reason =\n        ()\n",
        "actor Counter =\n    terminate reason =\n        let _ = reason + 1\n        ()\n",
        "type ExitReason = { value: Int }\nactor Counter =\n    terminate (ExitReason { value }) =\n        ()\n",
        "type Monitor = { value: Int }\nactor Counter =\n    onDown (Monitor { value }) reason =\n        ()\n",
    ] {
        let errors = diagnostics(source);
        assert!(errors.iter().any(|(code, _)| code.starts_with('T') && code != "T999"), "runtime callback input mismatch: {source}: {errors:?}");
    }
}
