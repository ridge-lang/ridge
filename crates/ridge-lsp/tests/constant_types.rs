//! Constant metadata changes retain the editor's type and navigation contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions};
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::Url;

#[test]
fn imported_constant_permissions_reject_and_clear_incrementally() {
    for ending in ["\n", "\r\n"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
        std::fs::write(
            dir.path().join("ridge.toml"),
            "[workspace]\nname=\"constant-effects\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("lib/ridge.toml"),
            "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("lib/src/Defs.ridge"), "import std.io as Io\npub const effect: fn io Int -> Int = fn (n: Int) -> Int =\n    Io.println \"effect\"\n    n\n".replace('\n', ending)).unwrap();
        let valid =
            "import lib.Defs as D\npub fn io value () -> Int =\n    let f = D.effect\n    f 7\n"
                .replace('\n', ending);
        std::fs::write(dir.path().join("lib/src/Main.ridge"), &valid).unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        let invalid = valid.replace("fn io value", "fn value");
        for (source, forbidden) in [(&valid, false), (&invalid, true), (&valid, false)] {
            state.recompile(module, source);
            assert!(
                state.resolved.parse_errors.is_empty(),
                "{:?}",
                state.resolved.parse_errors
            );
            let diagnostics = ridge_driver::collect_diagnostics(
                &[],
                &state.resolved,
                &state.type_errors,
                &state.lower_errors,
                &state.source_cache(),
                &state.typed.tycons,
            );
            assert_eq!(
                diagnostics.iter().any(|d| d.code == "T014"),
                forbidden,
                "{diagnostics:?}"
            );
            if !forbidden {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
            }
        }
    }
}

#[test]
fn functional_constant_calls_keep_imported_types_and_incremental_errors() {
    for ending in ["\n", "\r\n"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
        std::fs::write(
            dir.path().join("ridge.toml"),
            "[workspace]\nname=\"constant-call-editor\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("lib/ridge.toml"),
            "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
        )
        .unwrap();
        let defs = "pub const increment: fn Int -> Int = fn (n: Int) -> Int = n + 1\npub const zero: fn () -> Int = fn () -> Int = 7\npub const unit: fn Unit -> Int = fn (_u: Unit) -> Int = 7\n".replace('\n', ending);
        std::fs::write(dir.path().join("lib/src/Defs.ridge"), defs).unwrap();
        let valid = "import lib.Defs as D\npub fn value () -> Int =\n    let f = D.increment\n    D.increment 6 + f 6 + D.zero () + D.unit ()\n".replace('\n', ending);
        std::fs::write(dir.path().join("lib/src/Main.ridge"), &valid).unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let uri = Url::from_file_path(root.join("lib/src/Main.ridge")).unwrap();
        let definition_uri = Url::from_file_path(root.join("lib/src/Defs.ridge")).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        let invalid = valid.replace("D.increment 6", "D.increment \"wrong\"");
        for (source, has_error) in [(&valid, false), (&invalid, true), (&valid, false)] {
            state.recompile(module, source);
            assert!(state.resolved.parse_errors.is_empty());
            assert!(state.lower_errors.is_empty());
            let diagnostics = ridge_driver::collect_diagnostics(
                &[],
                &state.resolved,
                &state.type_errors,
                &state.lower_errors,
                &state.source_cache(),
                &state.typed.tycons,
            );
            assert_eq!(
                diagnostics.iter().any(|d| d.code == "T002"),
                has_error,
                "{diagnostics:?}"
            );
            if !has_error {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let column =
                    u32::try_from(source.lines().nth(3).unwrap().find("increment").unwrap())
                        .unwrap();
                let (hover, _) = index.hover_at(&uri, 3, column).unwrap();
                assert!(hover.contains("Int"), "{hover}");
                let target = index.definition_at(&uri, 3, column).unwrap();
                assert_eq!(target.uri, definition_uri);
                assert_eq!(target.range.start.line, 0);
            }
        }
    }
}

#[test]
fn constant_types_and_definitions_follow_an_incremental_edit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"constants\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    let source = "const sizes: List Int = [1, 2]\npub fn values () -> List Int = sizes\n";
    std::fs::write(dir.path().join("lib/src/Constants.ridge"), source).unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let mut state =
        check_workspace_incremental(CheckOptions::new(root.clone()).with_retain_indices(true))
            .unwrap();
    let uri = Url::from_file_path(root.join("lib/src/Constants.ridge")).unwrap();
    let module = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Constants.ridge"))
        .unwrap()
        .id;
    let edited = source
        .replace("List Int", "List Text")
        .replace("[1, 2]", "[\"a\"]");
    for (text, expected) in [(source, "List Int"), (edited.as_str(), "List Text")] {
        state.recompile(module, text);
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let column = u32::try_from(text.lines().nth(1).unwrap().rfind("sizes").unwrap()).unwrap();
        let (hover, _) = index.hover_at(&uri, 1, column).unwrap();
        assert!(hover.contains(expected), "{hover}");
        let definition = index.definition_at(&uri, 1, column).unwrap();
        assert_eq!(definition.uri, uri);
        assert_eq!(definition.range.start.line, 0);
    }
}
