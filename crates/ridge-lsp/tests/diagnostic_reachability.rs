//! Missing literal members reach the same diagnostic conversion as source errors.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ridge_driver::{check_workspace_incremental, collect_diagnostics, CheckOptions};
use ridge_lsp::diagnostics::{source_id_to_uri, to_lsp_diagnostic};
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Url};

#[test]
fn missing_literal_members_reach_editor_diagnostics_and_recheck_cleanly() {
    let directory = tempfile::tempdir().unwrap();
    let root = ridge_manifest::canonicalize(directory.path()).unwrap();
    for (member, rejected) in [("missing", true), ("apps/*", false)] {
        std::fs::write(
            root.join("ridge.toml"),
            format!(
                "[workspace]\nname = \"editor\"\nversion = \"0.1.0\"\nmembers = [\"{member}\"]\n"
            ),
        )
        .unwrap();
        let state = check_workspace_incremental(CheckOptions::new(root.clone())).unwrap();
        let sources = state.source_cache();
        let diagnostics = collect_diagnostics(
            &state.disc_resolve_errors,
            &state.resolved,
            &state.type_errors,
            &state.lower_errors,
            &sources,
            &state.typed.tycons,
        );
        let missing = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "M004");
        assert_eq!(missing.is_some(), rejected);
        if let Some(diagnostic) = missing {
            let uri = source_id_to_uri(&root, diagnostic.source_id.as_str());
            assert_eq!(uri, Url::from_file_path(&root).unwrap());
            let editor = to_lsp_diagnostic(diagnostic, &uri, None);
            assert_eq!(editor.code, Some(NumberOrString::String("M004".to_owned())));
            assert_eq!(editor.severity, Some(DiagnosticSeverity::ERROR));
            assert!(editor.message.contains("missing"));
            assert!(editor.code_description.is_some());
        }
    }
}
