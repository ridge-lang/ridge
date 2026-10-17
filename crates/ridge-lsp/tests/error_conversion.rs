//! Error conversion is discoverable in the editor and updates incrementally.

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

const SOURCE: &str = r#"type LocalIssue = ProblemA | ProblemB

instance ErrorFrom LocalIssue Text =
    errorFrom (x: LocalIssue) -> Text = "problem"

pub fn describe (e: LocalIssue) -> Text = errorFrom e

pub fn run (r: Result Int LocalIssue) -> Result Int Text =
    let n = r ?
    Ok n
"#;

fn position(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset];
    (
        before.matches('\n').count() as u32,
        before.rsplit('\n').next().unwrap().encode_utf16().count() as u32,
    )
}

#[test]
fn conversion_instance_constraint_fix_repairs_the_instance_context() {
    let source = format!("{SOURCE}\ntype EditBox a = EditBoxed a | EditEmpty\ninstance ErrorFrom (EditBox a) Text =\n    errorFrom (x: EditBox a) -> Text = match x\n        EditBoxed value -> toText value\n        EditEmpty -> \"empty\"\n");
    let (_dir, root, _) = fixture(&source);
    let mut state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
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
    assert!(
        state.type_errors.iter().any(|(_, e)| e.code() == "T055"),
        "{:?}",
        state.type_errors
    );
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let fixes = ridge_lsp::index::collect_signature_fixes(
        &index.line_indices,
        &index.module_uris,
        &index.module_text,
        &state.typed,
        &state.type_errors,
    );
    let fix = fixes
        .iter()
        .find(|fix| fix.code == "T055" && fix.new_text == " where ToText a")
        .unwrap();
    assert!(fix.title.contains("this instance"));
    let byte = ridge_lexer::LineIndex::new(&source)
        .utf16_to_byte(fix.edit_range.start.line, fix.edit_range.start.character)
        as usize;
    let mut repaired = source;
    repaired.insert_str(byte, &fix.new_text);
    let module = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    state.recompile(module, &repaired);
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);

    // Existing where syntax accepts variables. A fixed destination has no
    // mechanical where edit; changing the helper's public type needs a choice.
    let concrete_destination = format!("{repaired}\npub fn fixed (r: Result Int e) -> Result Int Text =\n    let n = r ?\n    Ok n\n");
    state.recompile(module, &concrete_destination);
    assert!(
        state.type_errors.iter().any(|(_, e)| e.code() == "T055"),
        "{:?}",
        state.type_errors
    );
    let index = WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
    let fixes = ridge_lsp::index::collect_signature_fixes(
        &index.line_indices,
        &index.module_uris,
        &index.module_text,
        &state.typed,
        &state.type_errors,
    );
    assert!(!fixes.iter().any(|fix| fix.code == "T055"));
}

#[test]
fn conversion_has_hover_completion_definition_signature_and_implementation() {
    let (_dir, root, uri) = fixture(SOURCE);
    let mut state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    assert!(
        state.resolved.errors.is_empty(),
        "{:?}",
        state.resolved.errors
    );
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    assert_method_surfaces(&index, &uri);
    assert_class_surfaces(&index, &uri);

    let module = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    let broken = SOURCE.replace("-> Result Int Text", "-> Result Int Int");
    state.recompile(module, &broken);
    assert!(
        state
            .type_errors
            .iter()
            .any(|(_, error)| error.code() == "T029"),
        "{:?}",
        state.type_errors
    );
    state.recompile(module, SOURCE);
    assert!(state.resolved.errors.is_empty());
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);

    let generic = format!(
        "{SOURCE}\npub fn lift (r: Result Int e) -> Result Int f =\n    let n = r ?\n    Ok n\n"
    );
    state.recompile(module, &generic);
    assert!(
        state
            .type_errors
            .iter()
            .any(|(_, error)| error.code() == "T055"),
        "{:?}",
        state.type_errors
    );
    let index = WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
    let fixes = ridge_lsp::index::collect_signature_fixes(
        &index.line_indices,
        &index.module_uris,
        &index.module_text,
        &state.typed,
        &state.type_errors,
    );
    assert!(
        fixes
            .iter()
            .any(|fix| fix.new_text == " where ErrorFrom e f"),
        "{:?}",
        fixes.iter().map(|f| &f.new_text).collect::<Vec<_>>()
    );
    let repaired = generic.replace("-> Result Int f =", "-> Result Int f where ErrorFrom e f =");
    state.recompile(module, &repaired);
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

fn assert_method_surfaces(index: &WorkspaceIndex, uri: &Url) {
    let at = SOURCE.rfind("errorFrom e").unwrap();
    let (line, col) = position(SOURCE, at + 2);
    let (hover, _) = index.hover_at(uri, line, col).unwrap();
    assert!(hover.contains("errorFrom (x: e) -> f"), "{hover}");
    assert!(hover.contains("matching instance"), "{hover}");
    let definition = index.definition_at(uri, line, col).unwrap();
    let declaration = index.declaration_at(uri, line, col).unwrap();
    assert_eq!(definition, declaration);
    let text = std::fs::read_to_string(definition.uri.to_file_path().unwrap()).unwrap();
    assert!(text.contains("class ErrorFrom e f"));
    assert!(text
        .lines()
        .nth(definition.range.start.line as usize)
        .unwrap()
        .contains("errorFrom"));
    let completions = index.completions_at(uri, line, col);
    let completion = completions
        .iter()
        .find(|item| item.label == "errorFrom")
        .unwrap();
    let card = index
        .resolve_completion(completion.data.as_ref().unwrap())
        .unwrap();
    assert!(card.0.contains("errorFrom (x: e) -> f"));
    let (call_line, call_col) = position(SOURCE, at + "errorFrom ".len());
    let signature = index
        .signature_help_at(uri, call_line, call_col, false)
        .unwrap();
    assert!(signature.signatures[0]
        .label
        .contains("errorFrom (x: e) -> f"));
    assert_eq!(index.implementations_at(uri, line, col).unwrap().len(), 1);
    assert!(!index
        .document_highlights_at(uri, line, col)
        .unwrap()
        .is_empty());
    assert!(index.prepare_rename_at(uri, line, col).is_none());
    let references = index
        .references_at(uri, line, col, true, &Cancel::new())
        .unwrap();
    assert!(references.contains(&definition));
    assert!(references.iter().any(|location| location.uri == *uri));
    let (method_line, method_col) = position(SOURCE, at);
    let mut token_line = 0;
    let mut token_col = 0;
    let mut method_token = None;
    for token in index.semantic_tokens(uri).unwrap().data {
        token_line += token.delta_line;
        token_col = if token.delta_line == 0 {
            token_col + token.delta_start
        } else {
            token.delta_start
        };
        if (token_line, token_col) == (method_line, method_col) {
            method_token = Some(token);
        }
    }
    let token = method_token.unwrap();
    assert_eq!(token.token_type, 4); // method, from the advertised token legend
    assert_ne!(token.token_modifiers_bitset & (1 << 2), 0); // defaultLibrary
}

fn assert_class_surfaces(index: &WorkspaceIndex, uri: &Url) {
    let class_at = SOURCE.find("ErrorFrom").unwrap();
    let (class_line, class_col) = position(SOURCE, class_at + 1);
    assert!(index
        .hover_at(uri, class_line, class_col)
        .unwrap()
        .0
        .contains("class ErrorFrom e f"));
    assert!(index.definition_at(uri, class_line, class_col).is_some());
    assert!(index
        .prepare_rename_at(uri, class_line, class_col)
        .is_none());
    assert!(index
        .completions_at(uri, class_line, class_col)
        .iter()
        .any(|item| item.label == "ErrorFrom"));
    let hierarchy = index
        .prepare_type_hierarchy_at(uri, class_line, class_col)
        .unwrap();
    let class = &hierarchy[0];
    assert_eq!(class.name, "ErrorFrom");
    assert!(index
        .type_supertypes(class.data.as_ref().unwrap())
        .unwrap()
        .is_empty());
    let instances = index.type_subtypes(class.data.as_ref().unwrap()).unwrap();
    assert_eq!(instances.len(), 1);
    assert_eq!(
        index
            .type_supertypes(instances[0].data.as_ref().unwrap())
            .unwrap()[0]
            .name,
        "ErrorFrom"
    );
}

fn fixture(source: &str) -> (tempfile::TempDir, std::path::PathBuf, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"errors\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), source).unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let uri = Url::from_file_path(root.join("app/src/Main.ridge")).unwrap();
    (dir, root, uri)
}
