//! Editor surfaces for the additive HTTP response metadata API.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]
use ridge_driver::{check_workspace_incremental, CheckOptions};
use ridge_lsp::cancel::Cancel;
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::Url;

const SOURCE: &str = "import std.net.http as Http (Response, ResponseWithHeaders)\n\npub fn wrap (r: Response) -> ResponseWithHeaders =\n    let wrapped = Http.withoutHeaders r\n    wrapped\n\npub fn attach (r: Response) -> Result ResponseWithHeaders Error = Http.withHeaders [(\"Location\", \"/target\")] r\n\npub fn net serve () -> Unit = Http.listenWithHeaders 18086 (fn _ -> Http.withoutHeaders (Http.respond 200 \"ok\"))\n";

fn checked_http_source() -> &'static str {
    let source = ridge_stdlib::STDLIB_SOURCES
        .iter()
        .find(|(path, _)| *path == "net/http.ridge")
        .unwrap()
        .1;
    let parsed = ridge_parser::parse_source(source);
    assert!(
        parsed.errors.is_empty(),
        "stdlib parse: {:?}",
        parsed.errors
    );
    let http = ridge_resolve::BUILTINS
        .iter()
        .find(|b| b.name == "std.net.http")
        .unwrap();
    for name in [
        "respond",
        "withoutHeaders",
        "withHeaders",
        "listenWithHeaders",
    ] {
        assert!(
            ridge_lsp::stdlib_defs::stdlib_location(http.id, name).is_some(),
            "stdlib location {name}"
        );
    }
    source
}

fn position(needle: &str) -> (u32, u32) {
    let at = SOURCE.find(needle).unwrap();
    let before = &SOURCE[..at];
    (
        before.matches('\n').count() as u32,
        (at - before.rfind('\n').map_or(0, |p| p + 1)) as u32,
    )
}

fn assert_function_surfaces(index: &WorkspaceIndex, uri: &Url, name: &str, source: &str) {
    let (line, column) = position(&format!("Http.{name}"));
    let offered = index.completions_at(uri, line, column + 5);
    assert!(
        offered.iter().any(|item| item.label == name),
        "missing completion {name}"
    );
    let item = offered.iter().find(|item| item.label == name).unwrap();
    let detail = index
        .resolve_completion(item.data.as_ref().expect("completion resolve payload"))
        .expect("resolved HTTP completion");
    assert!(detail.0.contains(name));
    let references = index
        .references_at(uri, line, column + 6, false, &Cancel::default())
        .expect("HTTP references");
    assert!(references
        .iter()
        .any(|location| location.uri == *uri && location.range.start.line == line));
    assert!(!index
        .document_highlights_at(uri, line, column + 6)
        .expect("HTTP highlights")
        .is_empty());
    assert!(
        index.prepare_rename_at(uri, line, column + 6).is_none(),
        "stdlib functions cannot be renamed"
    );
    assert!(index
        .rename_at(uri, line, column + 6, "renamed", &Cancel::default())
        .unwrap()
        .is_none());
    let hover = index
        .hover_at(uri, line, column + 6)
        .expect("HTTP API hover");
    assert!(hover.0.contains(name), "wrong hover: {}", hover.0);
    let definition = index
        .definition_at(uri, line, column + 6)
        .unwrap_or_else(|| panic!("HTTP API definition {name}, hover {}", hover.0));
    assert!(definition.uri.path().ends_with("net/http.ridge"));
    assert_eq!(
        std::fs::read(definition.uri.to_file_path().unwrap()).unwrap(),
        source.as_bytes(),
        "goto must point to the current embedded source"
    );
    let signature = index
        .signature_help_at(uri, line, column + 5 + name.len() as u32, false)
        .expect("HTTP API signature");
    assert!(
        signature.signatures.iter().any(|s| s.label.contains(name)),
        "wrong signature"
    );
}

#[test]
fn http_metadata_completion_hover_signature_and_definition() {
    let source = checked_http_source();
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src")).unwrap();
    std::fs::write(directory.path().join("ridge.toml"), "[workspace]\nname = \"metadata\"\nversion = \"0.1.0\"\nmembers = [\".\"]\n[project]\nname = \"metadata\"\nversion = \"0.1.0\"\nkind = \"library\"\n[capabilities]\nallow = [\"net\"]\n").unwrap();
    std::fs::write(directory.path().join("src/Main.ridge"), SOURCE).unwrap();
    let root = ridge_manifest::canonicalize(directory.path()).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root.clone()).with_retain_indices(true))
            .unwrap();
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    assert!(
        state.type_errors.is_empty(),
        "type errors: {:?}",
        state.type_errors
    );
    assert!(
        state.disc_resolve_errors.is_empty(),
        "resolve errors: {:?}",
        state.disc_resolve_errors
    );
    let uri = Url::from_file_path(root.join("src/Main.ridge")).unwrap();
    assert!(!index
        .semantic_token_data(&uri)
        .expect("HTTP semantic tokens")
        .is_empty());
    for name in ["withoutHeaders", "withHeaders", "listenWithHeaders"] {
        assert_function_surfaces(&index, &uri, name, source);
    }
    let (line, column) = position("    wrapped");
    let column = column + 4;
    let target = index
        .type_definition_at(&uri, line, column)
        .expect("response metadata type definition");
    let contents = std::fs::read_to_string(target.uri.to_file_path().unwrap()).unwrap();
    let declaration = contents
        .lines()
        .nth(target.range.start.line as usize)
        .unwrap();
    assert!(declaration.contains("pub opaque type ResponseWithHeaders"));

    let (line, column) = position("ResponseWithHeaders =");
    let hover = index
        .hover_at(&uri, line, column)
        .expect("metadata type hover");
    assert!(hover.0.contains("ResponseWithHeaders"));
    assert!(index
        .definition_at(&uri, line, column)
        .expect("metadata type definition")
        .uri
        .path()
        .ends_with("net/http.ridge"));
    let offered = index.completions_at(&uri, line, column);
    let item = offered
        .iter()
        .find(|item| item.label == "ResponseWithHeaders")
        .expect("metadata type completion");
    assert!(index
        .resolve_completion(
            item.data
                .as_ref()
                .expect("metadata type completion payload")
        )
        .expect("metadata type completion resolve")
        .0
        .contains("ResponseWithHeaders"));
}
