//! Unit payloads and nullary markers remain distinct through editor edits.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, collect_diagnostics, CheckOptions};
use ridge_lsp::{diagnostics::to_lsp_diagnostic, index::WorkspaceIndex};
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Position, Url};

fn position(source: &str, offset: usize) -> Position {
    let prefix = &source[..offset];
    Position::new(
        u32::try_from(prefix.matches('\n').count()).unwrap(),
        u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"actor-unit-editor\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    dir
}

#[test]
fn actor_unit_payload_types_and_marker_facts_follow_edits() {
    for ending in ["\n", "\r\n"] {
        let dir = workspace();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        let valid = "-- café 🧭\nimport std.actor (AskError)\nactor Counter =\n    on value (payload: Unit) -> Int = 9\npub fn time fetch (h: Handle Counter) -> Result Int AskError = h ?> value ()\n".replace('\n', ending);
        std::fs::write(&file, &valid).unwrap();
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
        let nullary = valid.replace("(payload: Unit)", "()");
        for (source, code, marker) in [
            (valid.clone(), None, false),
            (nullary, None, true),
            (valid.clone(), None, false),
            (
                valid.replace("h ?> value ()", "h ?> value"),
                Some("T003"),
                false,
            ),
            (
                valid.replace("h ?> value ()", "h ?> value 7"),
                Some("T001"),
                false,
            ),
            (
                valid.replace("h ?> value ()", "h ?> absent ()"),
                Some("T015"),
                false,
            ),
            (
                valid.replace("fn time fetch", "fn fetch"),
                Some("T014"),
                false,
            ),
            (valid.clone(), None, false),
        ] {
            state.recompile(module, &source);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(
                state.resolved.parse_errors.is_empty(),
                "{:?}",
                state.resolved.parse_errors
            );
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            let diagnostics = collect_diagnostics(
                &[],
                &state.resolved,
                &state.type_errors,
                &state.lower_errors,
                &state.source_cache(),
                &state.typed.tycons,
            );
            let errors: Vec<_> = diagnostics
                .iter()
                .map(|d| to_lsp_diagnostic(d, &uri, Some(&source)))
                .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
                .collect();
            if let Some(code) = code {
                assert!(
                    errors
                        .iter()
                        .any(|d| d.code == Some(NumberOrString::String(code.to_owned()))),
                    "{errors:?}"
                );
            } else {
                assert!(errors.is_empty(), "{errors:?}");
                assert_eq!(
                    !state.typed.modules[module.0 as usize]
                        .nullary_message_sites
                        .is_empty(),
                    marker
                );
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let at = position(&source, source.find("fetch").unwrap());
                let (hover, _) = index.hover_at(&uri, at.line, at.character).unwrap();
                assert!(hover.contains("Int") && hover.contains("Result"), "{hover}");
                if !marker {
                    let at = position(&source, source.find("payload").unwrap());
                    let (hover, _) = index.hover_at(&uri, at.line, at.character).unwrap();
                    assert!(hover.contains("Unit"), "{hover}");
                }
            }
        }
    }
}
