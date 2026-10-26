//! Reply diagnostics follow body edits without changing the declared ask type.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{
    check_workspace_incremental, collect_diagnostics, CheckOptions, IncrementalState,
};
use ridge_lsp::{diagnostics::to_lsp_diagnostic, index::WorkspaceIndex};
use ridge_resolve::ModuleId;
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Position, Range, Url};

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url, ModuleId) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"reply-editor\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let file = root.join("lib/src/Main.ridge");
    std::fs::write(&file, source).unwrap();
    let uri = Url::from_file_path(file).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    (dir, state, uri, mid)
}

fn position(source: &str, offset: usize) -> Position {
    let prefix = &source[..offset];
    Position::new(
        u32::try_from(prefix.bytes().filter(|b| *b == b'\n').count()).unwrap(),
        u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

fn assert_parsed(state: &IncrementalState) {
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
}

#[test]
fn handler_reply_diagnostics_follow_unicode_line_endings_and_edits() {
    for ending in ["\n", "\r\n"] {
        let valid =
            "-- café 🧭\nactor Counter =\n    on value () -> Int = 7\n".replace('\n', ending);
        let invalid = valid.replace("= 7", "= \"wrong\"");
        let corrected = invalid.replace("-> Int", "-> Text");
        let (_dir, mut state, uri, mid) = workspace(&valid);
        for (source, rejects) in [
            (&valid, false),
            (&invalid, true),
            (&corrected, false),
            (&invalid, true),
            (&valid, false),
        ] {
            state.recompile(mid, source);
            assert_parsed(&state);
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
            if rejects {
                assert_eq!(errors.len(), 1, "{errors:?}");
                let error = &errors[0];
                assert_eq!(error.code, Some(NumberOrString::String("T001".to_owned())));
                let start = source.find("\"wrong\"").unwrap();
                assert_eq!(
                    error.range,
                    Range::new(position(source, start), position(source, start + 7))
                );
                assert!(
                    error.message.contains("Int") && error.message.contains("Text"),
                    "{}",
                    error.message
                );
                assert!(
                    error.message.contains("handler `value`"),
                    "{}",
                    error.message
                );
            } else {
                assert!(errors.is_empty(), "{errors:?}");
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            }
        }
    }
}

#[test]
fn invalid_handler_body_preserves_ask_contract_and_parameter_navigation() {
    let valid = "import std.actor (AskError)\ntype Book = { value: Int }\nactor Counter =\n    on book () -> Book = Book { value = 7 }\npub fn time fetch (h: Handle Counter) -> Result Book AskError = h ?> book ()\n";
    let invalid = valid.replace("= Book { value = 7 }", "= 7");
    let (_dir, mut state, uri, mid) = workspace(valid);
    for (source, rejects) in [(valid, false), (invalid.as_str(), true), (valid, false)] {
        state.recompile(mid, source);
        assert_parsed(&state);
        assert_eq!(
            !state.type_errors.is_empty(),
            rejects,
            "{:?}",
            state.type_errors
        );
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let ask = position(source, source.find("?>").unwrap());
        let (card, _) = index.hover_at(&uri, ask.line, ask.character).unwrap();
        assert!(card.contains("Result Book AskError"), "{card}");
        let use_site = position(source, source.find("h ?>").unwrap());
        let declaration = position(source, source.find("(h:").unwrap() + 1);
        let definition = index
            .definition_at(&uri, use_site.line, use_site.character)
            .unwrap();
        assert_eq!(definition.uri, uri);
        assert_eq!(definition.range.start, declaration);
    }
}
