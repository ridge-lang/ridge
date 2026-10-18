//! Actor arguments survive incremental checking and editor position indexing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, collect_diagnostics, CheckOptions};
use ridge_lsp::{diagnostics::to_lsp_diagnostic, index::WorkspaceIndex};
use tower_lsp::lsp_types::{NumberOrString, Url};

const SOURCE: &str = r#"import std.actor (AskError)
actor Worker =
    on value () -> Int = 7

actor Store =
    on tag (label: Text) -> Int = 4
    on transform (f: fn Int -> Int) -> Int = f 4
    on register (worker: Handle Worker) -> Int = 4

actor Labelled =
    state label: Text
    init (text: Text) =
        label <- text
    on value () -> Text = label

actor Transform =
    state apply: fn Int -> Int
    init (f: fn Int -> Int) =
        apply <- f
    on value () -> Int = apply 4

actor Parent =
    state worker: Handle Worker
    init (child: Handle Worker) =
        worker <- child
    on value () -> Handle Worker = worker

pub fn time spawn exercise () -> Result Int AskError =
    let store = spawn Store
    let raw = store ?> tag r"café 🧭" ?
    let mapped = store ?> transform fn x -> x + 1
    let nested = store ?> register spawn Worker
    let labelled = spawn Labelled r"café 🧭"
    let transformed = spawn Transform fn x -> x + 1
    let parent = spawn Parent spawn Worker
    let _ = raw
    let _ = nested
    let _ = labelled
    let _ = transformed
    let _ = parent
    match mapped
        Ok n -> Ok n
        Err e -> Err e
"#;

#[test]
fn actor_arguments_recheck_with_types_and_precise_timeout_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"arguments\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/ridge.toml"), "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"time\",\"spawn\"]\n").unwrap();
    let source = SOURCE.to_owned();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("app/src/Main.ridge"), &source).unwrap();
    let uri = Url::from_file_path(root.join("app/src/Main.ridge")).unwrap();
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
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    for name in ["mapped", "nested"] {
        let start = source.find(&format!("let {name}")).unwrap();
        let offset = start + source[start..].find("?>").unwrap();
        let (line, col) =
            ridge_lexer::LineIndex::new(&source).byte_to_utf16(u32::try_from(offset).unwrap());
        let (hover, _) = index
            .hover_at(&uri, line, col)
            .expect("typed actor argument result");
        assert!(hover.contains("Result Int AskError"), "{hover}");
    }
    for (expression, expected) in [
        ("spawn Labelled", "Labelled"),
        ("spawn Transform", "Transform"),
        ("spawn Parent", "Parent"),
    ] {
        let offset = source.find(expression).unwrap();
        let (line, col) =
            ridge_lexer::LineIndex::new(&source).byte_to_utf16(u32::try_from(offset).unwrap());
        let (hover, _) = index.hover_at(&uri, line, col).expect("typed spawn result");
        assert!(hover.contains(&format!("Handle {expected}")), "{hover}");
    }
    let offset = source.find("x + 1").unwrap();
    let (line, col) =
        ridge_lexer::LineIndex::new(&source).byte_to_utf16(u32::try_from(offset).unwrap());
    let definition = index
        .definition_at(&uri, line, col)
        .expect("lambda parameter definition");
    assert_eq!(definition.uri, uri);
    assert_eq!(definition.range.start.line, line);
    let wrong = source.replace(
        "r\"café 🧭\" ?",
        "r\"café 🧭\" timeout if true then 1 else 2 ?",
    );
    state.recompile(mid, &wrong);
    let diagnostics = collect_diagnostics(
        &state.disc_resolve_errors,
        &state.resolved,
        &state.type_errors,
        &state.lower_errors,
        &state.source_cache(),
        &state.typed.tycons,
    );
    let error = diagnostics
        .iter()
        .map(|d| to_lsp_diagnostic(d, &uri, Some(&wrong)))
        .find(|d| d.code == Some(NumberOrString::String("P001".into())))
        .expect("timeout diagnostic");
    assert!(error.message.contains("parenthes"), "{error:?}");
    let offset = wrong.find("if true").unwrap();
    let (line, col) =
        ridge_lexer::LineIndex::new(&wrong).byte_to_utf16(u32::try_from(offset).unwrap());
    assert_eq!(
        (error.range.start.line, error.range.start.character),
        (line, col)
    );
    state.recompile(mid, &source);
    assert!(state.resolved.parse_errors.is_empty());
    assert!(state.resolved.errors.is_empty());
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
}
