//! Compiler spans and editor positions agree across CRLF and Unicode text.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::{cancel::Cancel, index::WorkspaceIndex};
use tower_lsp::lsp_types::{Position, Range, TextEdit, Url};

const ENTITIES: &str = "-- Definitions\npub fn identity (n: Int) -> Int = n\n";
const MAIN: &str = "import lib.Entities (identity)\n\npub fn choose (label: Text) (n: Int) -> Int = n\npub fn value (n: Int) -> Int =\n    choose \"caf\u{e9} \u{1f9ed}\" (identity n)\n";

fn clean(state: &IncrementalState) {
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
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

fn position(text: &str, offset: usize) -> Position {
    let prefix = &text[..offset];
    Position::new(
        u32::try_from(prefix.bytes().filter(|b| *b == b'\n').count()).unwrap(),
        u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

fn range(text: &str, start: usize, len: usize) -> Range {
    Range::new(position(text, start), position(text, start + len))
}

fn apply(text: &str, edits: &[TextEdit]) -> String {
    // These offsets belong to the original buffer, not the compiler snapshot.
    let lines = ridge_lexer::LineIndex::new(text);
    let mut edits: Vec<_> = edits
        .iter()
        .map(|e| {
            (
                lines.utf16_to_byte(e.range.start.line, e.range.start.character) as usize,
                lines.utf16_to_byte(e.range.end.line, e.range.end.character) as usize,
                e.new_text.as_str(),
            )
        })
        .collect();
    edits.sort_unstable_by_key(|e| std::cmp::Reverse(e.0));
    let mut result = text.to_owned();
    for (start, end, replacement) in edits {
        assert_eq!(&result[start..end], "identity");
        result.replace_range(start..end, replacement);
    }
    result
}

#[test]
fn crlf_unicode_navigation_and_rename_preserve_original_buffers_after_rechecks() {
    for ending in ["\n", "\r\n"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
        std::fs::write(
            dir.path().join("ridge.toml"),
            "[workspace]\nname=\"positions\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("lib/ridge.toml"),
            "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
        )
        .unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let main_path = root.join("lib/src/Main.ridge");
        let entities_path = root.join("lib/src/Entities.ridge");
        std::fs::write(&main_path, MAIN.replace('\n', ending)).unwrap();
        std::fs::write(&entities_path, ENTITIES.replace('\n', ending)).unwrap();
        let main_uri = Url::from_file_path(&main_path).unwrap();
        let entities_uri = Url::from_file_path(&entities_path).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root.clone()).with_retain_indices(true))
                .unwrap();
        clean(&state);
        let main_id = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path == main_path)
            .unwrap()
            .id;
        for current in [ending, "\r\n", "\n", ending] {
            let main = MAIN.replace('\n', current);
            state.recompile(main_id, &main);
            clean(&state);
            let index =
                WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
            let start = main.rfind("identity").unwrap();
            let pos = position(&main, start);
            assert_ne!(
                pos.character as usize,
                main[..start].rsplit('\n').next().unwrap().len(),
                "Unicode must distinguish UTF-16 from UTF-8"
            );
            let (hover, span) = index.hover_at(&main_uri, pos.line, pos.character).unwrap();
            assert!(
                hover.contains("identity") && hover.contains("Int"),
                "{hover}"
            );
            assert_eq!(
                index.span_to_range(&main_uri, span).unwrap(),
                range(&main, start, 8)
            );
            let definition = index
                .definition_at(&main_uri, pos.line, pos.character)
                .unwrap();
            assert_eq!(definition.uri, entities_uri);
            assert_eq!(
                definition.range,
                Range::new(Position::new(1, 4), Position::new(1, 35))
            );
            let refs = index
                .references_at(&main_uri, pos.line, pos.character, true, &Cancel::new())
                .unwrap();
            assert!(refs
                .iter()
                .any(|r| r.uri == main_uri && r.range == range(&main, start, 8)));
            assert!(refs
                .iter()
                .any(|r| r.uri == entities_uri && r.range == definition.range));
            let edit = index
                .rename_at(
                    &main_uri,
                    pos.line,
                    pos.character,
                    "renamed",
                    &Cancel::new(),
                )
                .unwrap()
                .unwrap();
            let changes = edit.changes.unwrap();
            let renamed_main = apply(&main, changes.get(&main_uri).unwrap());
            let entities = ENTITIES.replace('\n', ending);
            let renamed_entities = apply(&entities, changes.get(&entities_uri).unwrap());
            assert_eq!(renamed_main, main.replace("identity", "renamed"));
            assert_eq!(renamed_entities, entities.replace("identity", "renamed"));
            std::fs::write(&main_path, &renamed_main).unwrap();
            std::fs::write(&entities_path, &renamed_entities).unwrap();
            let repaired = check_workspace_incremental(
                CheckOptions::new(root.clone()).with_retain_indices(true),
            )
            .unwrap();
            clean(&repaired);
            std::fs::write(&main_path, MAIN.replace('\n', ending)).unwrap();
            std::fs::write(&entities_path, ENTITIES.replace('\n', ending)).unwrap();
        }
    }
}
