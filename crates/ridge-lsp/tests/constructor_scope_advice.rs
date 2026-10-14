//! Advice for unavailable qualified names follows the effective import scope.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, collect_diagnostics, CheckOptions};
use ridge_lsp::diagnostics::to_lsp_diagnostic;
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Url};

fn workspace(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"advice\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/src/Main.ridge"), source).unwrap();
    dir
}

#[test]
fn editor_advises_the_bare_name_and_repair_clears_the_error() {
    for (import, qualified, bare, ty) in [
        ("std.json as Json", "Json.JInt 3", "JInt 3", "JsonValue"),
        ("std.json as Json", "Json.JNull", "JNull", "JsonValue"),
        ("std.text as Text", "Text.Some 3", "Some 3", "Option Int"),
        ("std.json as Json", "Option.JInt 3", "JInt 3", "JsonValue"),
    ] {
        let source = format!("import {import}\npub fn f () -> {ty} = {qualified}\n");
        let dir = workspace(&source);
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root.clone()).with_retain_indices(true))
                .unwrap();
        let uri = Url::from_file_path(root.join("lib/src/Main.ridge")).unwrap();
        let diagnostics = collect_diagnostics(
            &state.disc_resolve_errors,
            &state.resolved,
            &state.type_errors,
            &state.lower_errors,
            &state.source_cache(),
            &state.typed.tycons,
        );
        let diag = diagnostics
            .iter()
            .find(|d| d.code == "R014")
            .expect("qualified name is unavailable");
        assert_eq!(diag.notes.len(), 1);
        let note = &diag.notes[0].message;
        let name = bare.split_whitespace().next().unwrap();
        assert_eq!(
            note,
            &format!("`{name}` is already in scope; write `{name}` without the prefix")
        );
        let editor = to_lsp_diagnostic(diag, &uri, Some(&source));
        assert_eq!(editor.code, Some(NumberOrString::String("R014".into())));
        assert_eq!(editor.severity, Some(DiagnosticSeverity::ERROR));
        // Resolver help notes travel as related information in the server's
        // existing diagnostic protocol, alongside the unchanged headline.
        let related = editor.related_information.unwrap();
        assert_eq!(related[0].message, *note);
        assert_eq!(related[0].location.uri, uri);
        assert_eq!(related[0].location.range, editor.range);
        assert_eq!(editor.range.start.line, 1);
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        state.recompile(mid, &source.replace(qualified, bare));
        let repaired = collect_diagnostics(
            &state.disc_resolve_errors,
            &state.resolved,
            &state.type_errors,
            &state.lower_errors,
            &state.source_cache(),
            &state.typed.tycons,
        );
        assert!(repaired.is_empty(), "{repaired:?}");
    }
}

#[test]
fn typo_keeps_near_spelling_and_unavailable_or_shadowed_names_get_no_bare_advice() {
    for (source, expected) in [
        ("import std.actor as Actor\npub fn f () -> AskError = Actor.Timeuot\n", Some("Timeout")),
        ("import std.sql as Sql\npub fn f () -> Int = Sql.SqlInt 3\n", None),
        ("import std.json as Json\nimport std.text as JInt\npub fn f () -> JsonValue = Json.JInt 3\n", None),
        ("import std.text as Text\npub fn f () -> Int = Text.QAdd\n", None),
    ] {
        let dir = workspace(source);
        let state = check_workspace_incremental(CheckOptions::new(dir.path().to_path_buf())).unwrap();
        let diagnostics = collect_diagnostics(&state.disc_resolve_errors, &state.resolved,
            &state.type_errors, &state.lower_errors, &state.source_cache(), &state.typed.tycons);
        let diag = diagnostics.iter().find(|d| d.code == "R014").expect("unknown stdlib name");
        assert!(diag.notes.iter().all(|n| !n.message.contains("already in scope")), "{diag:?}");
        if let Some(expected) = expected {
            assert!(diag.notes.iter().any(|n| n.message.contains(expected)), "{diag:?}");
        }
    }
}
