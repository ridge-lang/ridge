//! Projection errors reach the editor at the annotation and disappear on repair.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{
    check_workspace_incremental, collect_diagnostics, CheckOptions, IncrementalState,
};
use ridge_lsp::{diagnostics::to_lsp_diagnostic, index::WorkspaceIndex};
use ridge_resolve::ModuleId;
use tower_lsp::lsp_types::{NumberOrString, Url};

fn expected_position(source: &str, offset: usize) -> (u32, u32) {
    let prefix = &source[..offset];
    (
        u32::try_from(prefix.bytes().filter(|&byte| byte == b'\n').count()).unwrap(),
        u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

#[test]
fn projection_diagnostics_repair_incrementally_with_utf16_ranges() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"projections\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    let source = "pub fn same (x: Ret (fn Int -> Text)) -> Text = x\n";
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let path = root.join("app/src/Main.ridge");
    std::fs::write(&path, source).unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    let mut state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    assert!(state.resolved.parse_errors.is_empty());
    assert!(
        state.resolved.errors.is_empty(),
        "{:?}",
        state.resolved.errors
    );
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path == path)
        .unwrap()
        .id;

    for (projection, code) in [
        ("Rows Int", "T029"),
        ("InsertShape Int", "T029"),
        ("Ret Int", "T061"),
    ] {
        // The preceding string makes byte and UTF-16 columns observably different.
        let broken = format!(
            "pub fn bad () -> Int =\n    let f = (\"café 🧭\", fn (x: {projection}) -> 1)\n    1\n"
        );
        state.recompile(mid, &broken);
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
            &state.disc_resolve_errors,
            &state.resolved,
            &state.type_errors,
            &state.lower_errors,
            &state.source_cache(),
            &state.typed.tycons,
        );
        let diagnostic = diagnostics
            .iter()
            .map(|d| to_lsp_diagnostic(d, &uri, Some(&broken)))
            .find(|d| d.code == Some(NumberOrString::String(code.into())))
            .expect("projection diagnostic in editor");
        let offset = broken.find(projection).unwrap();
        let start = expected_position(&broken, offset);
        let end = expected_position(&broken, offset + projection.len());
        assert_eq!(
            (
                diagnostic.range.start.line,
                diagnostic.range.start.character
            ),
            start
        );
        assert_eq!(
            (diagnostic.range.end.line, diagnostic.range.end.character),
            end
        );
        assert!(!diagnostic.message.contains("TyConId"), "{diagnostic:?}");

        state.recompile(mid, source);
        assert!(state.resolved.parse_errors.is_empty());
        assert!(state.resolved.errors.is_empty());
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let offset = u32::try_from(source.rfind('x').unwrap()).unwrap();
        let (line, col) = ridge_lexer::LineIndex::new(source).byte_to_utf16(offset);
        let (hover, _) = index
            .hover_at(&uri, line, col)
            .expect("repaired expression hover");
        assert!(hover.contains("Text"), "{hover}");
    }

    assert_field_specialization(&mut state, mid, &uri, source);
}

fn assert_field_specialization(
    state: &mut IncrementalState,
    mid: ModuleId,
    uri: &Url,
    source: &str,
) {
    let record =
        "pub type Box a = { value: InsertShape a }\npub fn bad (box: Box Int) -> Int = box.value\n";
    state.recompile(mid, record);
    assert!(state.resolved.parse_errors.is_empty());
    assert!(state.resolved.errors.is_empty());
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
        .map(|d| to_lsp_diagnostic(d, uri, Some(record)))
        .find(|d| d.message.contains("HasSchema Int"))
        .expect("field specialization reports its schema requirement");
    let start = record.find("box.value").unwrap();
    assert_eq!(
        (error.range.start.line, error.range.start.character),
        expected_position(record, start)
    );
    assert_eq!(
        (error.range.end.line, error.range.end.character),
        expected_position(record, start + "box.value".len())
    );
    state.recompile(mid, source);
    assert!(state.resolved.parse_errors.is_empty());
    assert!(state.resolved.errors.is_empty());
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

const SCHEMA_PRODUCER: &str = r#"import std.schema (HasSchema, EntitySchema, schema)
import std.sql (Row, SqlValue)
import std.map as Map
pub type Box a = { value: a }
pub type User = { name: Text }
instance HasSchema (Box a) where Row a =
    schemaOf (_witness: Option (Box a)) -> EntitySchema (Box a) = schema "Box" "boxes"
    toInsertRow (_shape: InsertShape (Box a)) -> Map Text SqlValue = Map.empty
pub fn through (x: InsertShape (Box a)) -> Box a = x
pub fn relay (x: Box a) -> Box a = through x
"#;
const SCHEMA_VALID: &str = "import app.producer (Box, User, relay)\npub fn use () -> Box User = relay (Box { value = User { name = \"Ada\" } })\n";
const SCHEMA_INVALID: &str = "import app.producer (Box, User, relay)\npub fn use () -> Box Int = relay (Box { value = 42 })\n";

struct SchemaWorkspace {
    _dir: tempfile::TempDir,
    state: IncrementalState,
    producer: ModuleId,
    consumer: ModuleId,
    uri: Url,
}

impl SchemaWorkspace {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
        std::fs::write(
            dir.path().join("ridge.toml"),
            "[workspace]\nname=\"projections\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app/ridge.toml"),
            "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("app/src/producer.ridge"), SCHEMA_PRODUCER).unwrap();
        std::fs::write(dir.path().join("app/src/consumer.ridge"), SCHEMA_VALID).unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let path = root.join("app/src/consumer.ridge");
        let uri = Url::from_file_path(&path).unwrap();
        let state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let consumer = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path == path)
            .unwrap()
            .id;
        let producer = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("producer.ridge"))
            .unwrap()
            .id;
        let fixture = Self {
            _dir: dir,
            state,
            producer,
            consumer,
            uri,
        };
        fixture.assert_clean();
        fixture
    }

    fn assert_clean(&self) {
        assert!(self.state.resolved.parse_errors.is_empty());
        assert!(
            self.state.resolved.errors.is_empty(),
            "{:?}",
            self.state.resolved.errors
        );
        assert!(
            self.state.type_errors.is_empty(),
            "{:?}",
            self.state.type_errors
        );
        assert!(
            self.state.lower_errors.is_empty(),
            "{:?}",
            self.state.lower_errors
        );
    }

    fn has_row_error(&self) -> bool {
        self.state.type_errors.iter().any(|(m, error)| {
            *m == self.consumer
                && matches!(error,
                    ridge_typecheck::TypeError::NoInstance { class, .. } if class == "Row"
                )
        })
    }
}

#[test]
fn imported_normalized_projection_requirements_survive_editor_repairs() {
    let mut fixture = SchemaWorkspace::new();
    for _ in 0..2 {
        fixture.state.recompile(fixture.consumer, SCHEMA_INVALID);
        assert!(fixture.state.resolved.parse_errors.is_empty());
        assert!(fixture.state.resolved.errors.is_empty());
        let state = &fixture.state;
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
            .map(|d| to_lsp_diagnostic(d, &fixture.uri, Some(SCHEMA_INVALID)))
            .find(|d| d.message.contains("Row Int"))
            .expect("imported projection checks its domain after an edit");
        let start = SCHEMA_INVALID.rfind("relay").unwrap();
        assert_eq!(
            (error.range.start.line, error.range.start.character),
            expected_position(SCHEMA_INVALID, start)
        );
        assert_eq!(
            (error.range.end.line, error.range.end.character),
            expected_position(SCHEMA_INVALID, start + "relay".len())
        );
        fixture.state.recompile(fixture.consumer, SCHEMA_VALID);
        fixture.assert_clean();
        let state = &fixture.state;
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let (line, col) = expected_position(SCHEMA_VALID, SCHEMA_VALID.rfind("relay").unwrap());
        let (hover, _) = index
            .hover_at(&fixture.uri, line, col)
            .expect("repaired imported generic hover");
        assert!(hover.contains("Box"), "{hover}");
    }
}

#[test]
fn producer_contract_edits_refresh_consumers_but_local_inference_ids_do_not() {
    let mut fixture = SchemaWorkspace::new();
    fixture.state.recompile(fixture.consumer, SCHEMA_INVALID);
    assert!(fixture.has_row_error());
    let without_requirement = SCHEMA_PRODUCER.replace("= through x", "= x");
    let affected = fixture
        .state
        .recompile(fixture.producer, &without_requirement);
    assert!(affected.contains(&fixture.consumer));
    fixture.assert_clean();
    let affected = fixture.state.recompile(fixture.producer, SCHEMA_PRODUCER);
    assert!(affected.contains(&fixture.consumer));
    assert!(fixture.has_row_error());
    fixture.state.recompile(fixture.consumer, SCHEMA_VALID);
    fixture.assert_clean();
    // A local function shifts later inference IDs without changing any export.
    let shifted = SCHEMA_PRODUCER.replace(
        "-> Box a = x",
        "-> Box a =\n    let spare = fn value -> value\n    x",
    );
    let affected = fixture.state.recompile(fixture.producer, &shifted);
    assert!(
        !affected.contains(&fixture.consumer),
        "alpha-equivalent contracts must not invalidate consumers"
    );
    fixture.assert_clean();
}

#[test]
fn recursive_projection_contracts_are_diagnosed_and_repaired_in_the_editor() {
    let mut fixture = SchemaWorkspace::new();
    let recursive = format!("{SCHEMA_PRODUCER}\npub fn first (x: Box a) -> Box a = second x\npub fn second (x: Box a) -> Box a =\n    let next = first\n    through x\n");
    fixture.state.recompile(fixture.producer, &recursive);
    assert!(fixture.state.resolved.parse_errors.is_empty());
    assert!(fixture.state.resolved.errors.is_empty());
    let state = &fixture.state;
    let uri =
        Url::from_file_path(&state.resolved.graph.modules[fixture.producer.0 as usize].file_path)
            .unwrap();
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
        .map(|d| to_lsp_diagnostic(d, &uri, Some(&recursive)))
        .find(|d| d.code == Some(NumberOrString::String("T055".into())))
        .expect("recursive declaration states its missing promise");
    assert!(error.message.contains("Row"), "{error:?}");
    assert_eq!(
        error.range.start.line,
        expected_position(&recursive, recursive.find("pub fn second").unwrap()).0
    );
    let repaired = recursive
        .replace(
            "(x: Box a) -> Box a = second x",
            "(x: InsertShape (Box a)) -> Box a = second x",
        )
        .replace(
            "pub fn second (x: Box a)",
            "pub fn second (x: InsertShape (Box a))",
        );
    fixture.state.recompile(fixture.producer, &repaired);
    fixture.assert_clean();
}
