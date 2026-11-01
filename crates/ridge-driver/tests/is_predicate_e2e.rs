//! Pattern recognition is structural, total, and independent of custom Eq.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

#[test]
fn predicates_match_payloads_and_evaluate_effectful_subjects_once() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"predicate\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/ridge.toml"), "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"io\"]\n").unwrap();
    std::fs::write(
        dir.path().join("app/src/Main.ridge"),
        r#"
import std.io as Io
import app.Other as Other
type Shape = | Square Int | Point Int Int | Circle { radius: Int }
type User = { age: Int, active: Bool }
type Color = | Red | Blue
instance Eq Color =
    eq (_left: Color) (_right: Color) -> Bool = true
fn io observed () -> Shape =
    Io.println "once"
    Square 5
pub fn imported () -> Bool = Other.make () is Other.Square
pub fn io exercise () -> List Bool =
    let square = Square 5
    let circle = Circle { radius = 7 }
    let user = User { age = 18, active = true }
    [square is Square, square is Circle, square is Square 5, square is Square 7,
     Point 1 2 is Point, circle is Circle, circle is Circle { radius = 7 },
     circle is Circle { radius = 8 }, user is User, user is User { age = 18, .. },
     Some square is Some (Square 5), Some square is Some (Circle { .. }),
     [1, 2, 3] is ([_, .., _]), [1] is [_, .., _], [1, 2, 3] is [.., _],
     [1, 2, 3] is [1, ..], [1, 2] is [1, 2], [] is [],
     (5, true) is (5, _), 5 is 5, 5 is 7, "a" is "a", "a" is "b",
     Red == Blue, Red is Blue, observed () is Square,
     false && observed () is Square, true || observed () is Square,
     Other.make () is Other.Square, Other.makeRecord () is Other.Entry,
     1.50m is 1.5m, 1.50m is 1.6m, 1.0 is 1.0, true is false,
     [Some 1, Some 2] is [Some 1, ..], Some [1, 2] is Some [1, ..], observed () is _]
"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Other.ridge"), "pub type Shape = | Square Bool | Circle\npub type Entry = { value: Int }\npub fn make () -> Shape = Square true\npub fn makeRecord () -> Entry = Entry { value = 5 }\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    assert_imported_pattern_owner(dir.path());
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let output = std::process::Command::new("erl").args(["-noshell", "-pa"]).arg(module.parent().unwrap()).args(["-eval", "[true,false,true,false,true,true,true,false,true,true,true,false,true,false,true,true,true,true,true,true,false,true,false,true,false,true,false,true,true,true,true,false,true,false,true,true,true]=ridge_app_Main:exercise(),io:put_chars(\"predicate-ok\\n\"),halt()."]).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        "once\nonce\npredicate-ok\n"
    );
}

fn assert_imported_pattern_owner(root: &std::path::Path) {
    let checked =
        ridge_driver::check_workspace_typed(ridge_driver::CheckOptions::new(root.to_path_buf()))
            .unwrap();
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let other = checked
        .resolved
        .graph
        .modules
        .iter()
        .find(|module| module.file_path.ends_with("Other.ridge"))
        .unwrap()
        .id;
    let owner = checked
        .typed
        .tycons
        .iter()
        .find(|decl| decl.name == "Shape" && decl.def_module_raw == Some(other.0))
        .unwrap()
        .id;
    let lowered = ridge_lower::lower_workspace(&checked.typed, &checked.resolved);
    assert!(lowered.errors.is_empty(), "{:?}", lowered.errors);
    let function = lowered
        .workspace
        .modules
        .iter()
        .flatten()
        .flat_map(|module| &module.items)
        .find_map(|item| match item {
            ridge_ir::IrItem::Fn(function) if function.name == "imported" => Some(function),
            _ => None,
        })
        .unwrap();
    let ridge_ir::IrExpr::Match { arms, .. } = &function.body else {
        panic!("expected typed matching");
    };
    let ridge_ir::IrPat::Ctor {
        sym: ridge_ir::SymbolRef::Constructor { owner_type, .. },
        ..
    } = &arms[0].pat
    else {
        panic!("expected constructor pattern");
    };
    assert_eq!(
        *owner_type,
        Some(owner),
        "imported pattern must retain the producer's type identity"
    );
}
