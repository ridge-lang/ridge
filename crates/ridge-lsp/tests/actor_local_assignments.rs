//! Valid actor-local updates retain their types and resolved binding identity.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, collect_diagnostics, CheckOptions};
use ridge_lsp::{diagnostics::to_lsp_diagnostic, index::WorkspaceIndex};
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Position, Url};

fn position(source: &str, offset: usize) -> Position {
    let prefix = &source[..offset];
    Position::new(
        u32::try_from(prefix.bytes().filter(|b| *b == b'\n').count()).unwrap(),
        u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

#[test]
fn actor_local_assignment_edits_keep_types_navigation_and_permissions() {
    for (ending, stateful_try) in [("\n", false), ("\r\n", false), ("\n", true), ("\r\n", true)] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
        std::fs::write(
            dir.path().join("ridge.toml"),
            "[workspace]\nname=\"assignment-editor\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("lib/ridge.toml"),
            "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
        )
        .unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let file = root.join("lib/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        let mut valid = "-- café 🧭\nactor Counter =\n    on value () -> Int =\n        var x = 1\n        x <- 7\n        x\n".to_owned();
        if stateful_try {
            valid = valid
                .replace("actor Counter =", "actor Counter =\n    state n: Int = 0")
                .replace("        x <- 7", "        let _: Result Unit Text = try\n            n <- n + 1\n            x <- 7\n            ()");
        }
        let valid = valid.replace('\n', ending);
        let self_read = valid.replace("x <- 7", "x <- x + 1");
        let immutable = valid.replace("var x", "let x");
        let wrong_type = valid.replace("x <- 7", "x <- \"wrong\"");
        std::fs::write(&file, &valid).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (source, code) in [
            (&valid, None),
            (&self_read, None),
            (&immutable, Some("R031")),
            (&wrong_type, Some("T001")),
            (&valid, None),
        ] {
            state.recompile(mid, source);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(
                state.resolved.parse_errors.is_empty(),
                "{:?}",
                state.resolved.parse_errors
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
                .map(|d| to_lsp_diagnostic(d, &uri, Some(source)))
                .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
                .collect();
            if let Some(code) = code {
                assert!(
                    errors
                        .iter()
                        .any(|d| d.code == Some(NumberOrString::String(code.to_owned()))),
                    "{errors:?}"
                );
                if code == "R031" {
                    let target = position(source, source.find("x <-").unwrap());
                    let error = errors
                        .iter()
                        .find(|d| d.code == Some(NumberOrString::String(code.to_owned())))
                        .unwrap();
                    assert_eq!(error.range.start, target);
                }
            } else {
                assert!(errors.is_empty(), "{errors:?}");
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let use_site = position(source, source.rfind("        x").unwrap() + 8);
                let declaration = position(source, source.find("var x").unwrap() + 4);
                let (hover, _) = index
                    .hover_at(&uri, use_site.line, use_site.character)
                    .unwrap();
                assert!(hover.contains("Int"), "{hover}");
                let definition = index
                    .definition_at(&uri, use_site.line, use_site.character)
                    .unwrap();
                assert_eq!(definition.uri, uri);
                assert_eq!(definition.range.start, declaration);
            }
        }
    }
}
