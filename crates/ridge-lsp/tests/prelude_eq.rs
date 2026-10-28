//! Prelude equality keeps editor types and rejection checks through edits.
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

#[test]
fn prelude_eq_types_navigation_and_rejections_follow_edits() {
    for ending in ["\n", "\r\n"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
        std::fs::write(
            dir.path().join("ridge.toml"),
            "[workspace]\nname=\"eq-editor\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app/ridge.toml"),
            "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
        )
        .unwrap();
        let helpers = "import std.io as Io\npub fn same (x: a) (y: a) -> Bool where Eq a = eq x y\npub fn io effect (x: Int) -> Int =\n    Io.println \"effect\"\n    x\n".replace('\n', ending);
        std::fs::write(dir.path().join("app/src/Helpers.ridge"), helpers).unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        let helpers_uri = Url::from_file_path(root.join("app/src/Helpers.ridge")).unwrap();
        let valid = "-- café 🧭\nimport app.Helpers as H\npub fn checkValue (x: Int) -> Bool =\n    let f = H.same\n    f x x\n".replace('\n', ending);
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
        let effectful = valid.replace("f x x", "f (H.effect x) x");
        for (source, code) in [
            (valid.clone(), None),
            (valid.replace("x: Int", "x: Float"), Some("T029")),
            (valid.replace("f x x", "f x \"wrong\""), Some("T002")),
            (effectful.clone(), Some("T014")),
            (
                effectful.replace("pub fn checkValue", "pub fn io checkValue"),
                None,
            ),
            (valid.clone(), None),
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
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let method = position(&source, source.find("H.same").unwrap() + 2);
                let definition = index
                    .definition_at(&uri, method.line, method.character)
                    .unwrap();
                assert_eq!(definition.uri, helpers_uri);
                assert_eq!(definition.range.start.line, 1);
                let binder = position(&source, source.find("let f").unwrap() + 4);
                let (hover, _) = index.hover_at(&uri, binder.line, binder.character).unwrap();
                assert!(hover.contains("Int") && hover.contains("Bool"), "{hover}");
            }
        }
    }
}
