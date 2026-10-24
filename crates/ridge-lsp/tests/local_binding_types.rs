//! Declaration cards retain binding types without changing statement types.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_ast::{visit::Visit, Expr, Span};
use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::{Position, Range, Url};

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"bindings\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let path = root.join("lib/src/Main.ridge");
    std::fs::write(&path, source).unwrap();
    let uri = Url::from_file_path(path).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    clean(&state);
    (dir, state, uri)
}

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

fn position(source: &str, offset: usize) -> Position {
    let before = &source[..offset];
    Position::new(
        u32::try_from(before.bytes().filter(|b| *b == b'\n').count()).unwrap(),
        u32::try_from(before.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

fn card(
    state: &IncrementalState,
    uri: &Url,
    source: &str,
    offset: usize,
    name: &str,
    ty: &str,
    kind: &str,
) {
    let index = WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
    let pos = position(source, offset);
    let (markdown, span) = index.hover_at(uri, pos.line, pos.character).unwrap();
    assert!(
        markdown.contains(&format!("\n{name} : {ty}\n")),
        "{markdown}"
    );
    assert!(markdown.contains(&format!("*({kind})*")), "{markdown}");
    assert_eq!(
        index.span_to_range(uri, span).unwrap(),
        Range::new(pos, position(source, offset + name.len()))
    );
}

#[derive(Default)]
struct BindingStatements(Vec<Span>);

impl<'ast> Visit<'ast> for BindingStatements {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if matches!(expr, Expr::Let { .. } | Expr::Var { .. }) {
            self.0.push(expr.span());
        }
        ridge_ast::visit::walk_expr(self, expr);
    }
}

fn statements_remain_unit(state: &IncrementalState) {
    let mut seen = false;
    for module in &state.typed.modules {
        let mut statements = BindingStatements::default();
        statements.visit_module(&module.ast);
        for span in statements.0 {
            seen = true;
            let nodes = &state.resolved.modules[module.id.0 as usize].node_ids;
            let node = nodes.get(span, ridge_resolve::NodeKind::Expr).unwrap();
            let ty = module.node_types[node.0 as usize].as_ref().unwrap();
            assert_eq!(
                ridge_typecheck::render_type_with(ty, &state.typed.tycons),
                "Unit"
            );
        }
    }
    assert!(seen, "the fixture must exercise a binding statement");
}

#[test]
fn let_and_var_binder_cards_keep_value_types_and_statement_unit() {
    for ending in ["\n", "\r\n"] {
        for (declaration, ty, result, kind) in [
            ("let n = 42", "Int", "n", "local"),
            ("let n: Int = 42", "Int", "n", "local"),
            ("let n = \"hello\"", "Text", "n", "local"),
            ("var n = 42", "Int", "n", "mutable variable"),
            ("var n: Int = 42", "Int", "n", "mutable variable"),
            ("let n = 42", "Int", "()", "local"),
        ] {
            let ret = if result == "()" { "Unit" } else { ty };
            let source = format!("pub fn value () -> {ret} =\n    {declaration}\n    {result}\n")
                .replace('\n', ending);
            let (_dir, state, uri) = workspace(&source);
            card(
                &state,
                &uri,
                &source,
                source.find(" n").unwrap() + 1,
                "n",
                ty,
                kind,
            );
            if result == "n" {
                card(
                    &state,
                    &uri,
                    &source,
                    source.rfind('n').unwrap(),
                    "n",
                    ty,
                    kind,
                );
            }
            statements_remain_unit(&state);
        }
    }
}

#[test]
fn destructured_and_shadowed_binders_keep_distinct_types() {
    let source = "pub fn value () -> Int =\n    let (n, text) = (42, \"hello\")\n    fn inner () -> Text =\n        let n = \"inner\"\n        n\n    let chosen = inner ()\n    n\n";
    let (_dir, state, uri) = workspace(source);
    for (offset, name, ty) in [
        (source.find("(n,").unwrap() + 1, "n", "Int"),
        (source.find("text)").unwrap(), "text", "Text"),
        (source.find("chosen").unwrap(), "chosen", "Text"),
        (source.rfind("let n").unwrap() + 4, "n", "Text"),
        (source.find("        n\n").unwrap() + 8, "n", "Text"),
        (source.rfind('n').unwrap(), "n", "Int"),
    ] {
        card(&state, &uri, source, offset, name, ty, "local");
    }
    statements_remain_unit(&state);
}

#[test]
fn binder_types_and_type_definitions_follow_unicode_and_incremental_edits() {
    let source = "pub fn choose (label: Text) (n: Int) -> Int = n\npub fn value () -> Int =\n    let n = 42\n    choose \"caf\u{e9} \u{1f9ed}\" n\n";
    let (_dir, mut state, uri) = workspace(source);
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    for (text, ty) in [
        (source.to_owned(), "Int"),
        (
            source
                .replace("Int", "Text")
                .replace("42", "\"yes\"")
                .replace('\n', "\r\n"),
            "Text",
        ),
        (source.to_owned(), "Int"),
    ] {
        state.recompile(mid, &text);
        clean(&state);
        card(
            &state,
            &uri,
            &text,
            text.find("let n").unwrap() + 4,
            "n",
            ty,
            "local",
        );
        let offset = text.rfind('n').unwrap();
        assert_ne!(
            position(&text, offset).character as usize,
            text[..offset].rsplit('\n').next().unwrap().len()
        );
        card(&state, &uri, &text, offset, "n", ty, "local");
        statements_remain_unit(&state);
    }
    let record = "pub type Box = { value: Int }\npub fn value () -> Box =\n    let n = Box { value = 42 }\n    n\n";
    state.recompile(mid, record);
    clean(&state);
    let index = WorkspaceIndex::build(2, &state.typed, &state.resolved, &state.source_cache());
    let definition = index.type_definition_at(&uri, 2, 8).unwrap();
    assert_eq!(definition.uri, uri);
    assert_eq!(definition.range.start.line, 0);
    assert_eq!(index.type_definition_at(&uri, 3, 4).unwrap(), definition);
}

#[test]
fn generic_binders_and_parameter_cards_keep_their_types() {
    let source = "pub fn identity (x: a) -> a = x\npub fn generic (x: a) -> a =\n    let copy = x\n    copy\npub fn unused () -> Unit =\n    let f = identity\n    ()\npub fn parameter (x, y) -> Int = x\n";
    let (_dir, state, uri) = workspace(source);
    card(
        &state,
        &uri,
        source,
        source.find("let copy").unwrap() + 4,
        "copy",
        "a",
        "local",
    );
    card(
        &state,
        &uri,
        source,
        source.rfind("copy").unwrap(),
        "copy",
        "a",
        "local",
    );
    card(
        &state,
        &uri,
        source,
        source.find("parameter (x").unwrap() + 11,
        "x",
        "Int",
        "parameter",
    );
    card(
        &state,
        &uri,
        source,
        source.find("let f").unwrap() + 4,
        "f",
        "(a) -> a",
        "local",
    );
    statements_remain_unit(&state);
}
