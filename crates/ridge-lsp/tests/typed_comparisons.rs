//! Comparison evidence remains typed through unsaved editor edits.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions};
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::Url;

#[test]
fn intrinsic_comparison_contracts_reject_unsaved_redeclarations() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("reserved-comparison-contracts");
        let valid = "-- 🧭 café\npub fn same () -> Bool = 1 == 1\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        std::fs::write(&file, &valid).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for name in ["Eq", "Ord"] {
            let invalid = format!(
                "-- 🧭 🧭 café\nclass {name} a =\n    wrong (value: a) -> Int\npub fn same () -> Bool = 1 == 1\n"
            )
            .replace('\n', ending);
            state.recompile(module, &invalid);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(state.resolved.parse_errors.is_empty());
            assert!(
                state
                    .resolved
                    .errors
                    .iter()
                    .any(|(_, error)| error.code() == "R028"),
                "{:?}",
                state.resolved.errors
            );
            state.recompile(module, &valid);
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
            assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
        }
    }
}

fn editor_workspace(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(root.join("app/src")).unwrap();
    std::fs::write(
        root.join("ridge.toml"),
        format!("[workspace]\nname=\"{name}\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    (dir, root)
}

#[test]
fn outer_join_row_contexts_follow_unsaved_entity_edits() {
    for ending in ["\n", "\r\n"] {
        for shape in ["LeftJoin", "RightJoin", "FullJoin"] {
            let (_dir, root) = editor_workspace("outer-join-row-contexts");
            std::fs::write(
                root.join("app/ridge.toml"),
                "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"db\"]\n",
            ).unwrap();
            let valid = format!(
                "import std.data (DbError, MemAdapter)\nimport std.sql (Row)\nimport std.repo as Repo\nimport std.repo ({shape})\n-- 🧭 café\ntype User = {{ age: Int }} deriving (Row)\ntype Post = {{ author: Int }} deriving (Row)\npub fn db probe (query: {shape} User Post MemAdapter) -> Result Bool DbError = Repo.exists query\n"
            ).replace('\n', ending);
            let file = root.join("app/src/Main.ridge");
            std::fs::write(&file, &valid).unwrap();
            let mut state =
                check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true))
                    .unwrap();
            let module = state
                .resolved
                .graph
                .modules
                .iter()
                .find(|module| module.file_path.ends_with("Main.ridge"))
                .unwrap()
                .id;
            for (source, invalid) in [
                (valid.clone(), false),
                (valid.replace(" deriving (Row)", ""), false),
                (
                    valid
                        .replace(
                            "type User = { age: Int } deriving (Row)",
                            "opaque type User = { age: Int }",
                        )
                        .replace("🧭 café", "🧭 🧭 café"),
                    true,
                ),
                (valid.clone(), false),
                (
                    valid.replace(
                        "type Post = { author: Int } deriving (Row)",
                        "opaque type Post = { author: Int }",
                    ),
                    true,
                ),
                (valid.clone(), false),
            ] {
                state.recompile(module, &source);
                assert!(state.resolved.lex_errors.is_empty());
                assert!(state.resolved.parse_errors.is_empty());
                assert!(
                    state.resolved.errors.is_empty(),
                    "{:?}",
                    state.resolved.errors
                );
                if invalid {
                    assert!(
                        state
                            .type_errors
                            .iter()
                            .any(|(_, error)| error.code() == "T029"),
                        "{:?}",
                        state.type_errors
                    );
                } else {
                    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                }
            }
        }
    }
}

#[test]
fn inferred_inner_comparisons_follow_unsaved_field_specializations() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("inner-field-comparison-editor");
        let valid = "pub fn exercise () -> Bool =\n    fn same record -> Bool = record.value == record.value\n    let label = \"🧭 café\"\n    let first = same ({ value = 1 })\n    same ({ value = 2 }) && first\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        std::fs::write(&file, &valid).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (source, invalid) in [
            (valid.clone(), false),
            (
                valid
                    .replace("value = 2", "value = 2.0")
                    .replace("🧭 café", "🧭 🧭 café"),
                true,
            ),
            (valid.clone(), false),
            (valid.replace("value = 2", "value = true"), false),
        ] {
            state.recompile(module, &source);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(state.resolved.parse_errors.is_empty());
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            if invalid {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(_, error)| error.code() == "T029"
                            && error.render(&state.typed.tycons).contains("Float")),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let (hover, _) = index.hover_at(&uri, 1, 7).unwrap();
                assert!(hover.contains("Eq"), "{hover}");
                assert!(
                    hover.contains("function") && !hover.contains("Unit"),
                    "{hover}"
                );
                let help = index.signature_help_at(&uri, 4, 10, true).unwrap();
                assert!(
                    help.signatures[0].label.contains("Eq"),
                    "{:?}",
                    help.signatures
                );
                let definition = index.definition_at(&uri, 4, 5).unwrap();
                assert_eq!(definition.uri, uri);
                assert_eq!(definition.range.start.line, 1);
                let function = ridge_lsp::index::SEMANTIC_TOKEN_TYPES
                    .iter()
                    .position(|kind| *kind == tower_lsp::lsp_types::SemanticTokenType::FUNCTION)
                    .unwrap();
                let (mut line, mut column) = (0, 0);
                let classified = index
                    .semantic_tokens(&uri)
                    .unwrap()
                    .data
                    .iter()
                    .any(|token| {
                        line += token.delta_line;
                        column = if token.delta_line == 0 {
                            column + token.delta_start
                        } else {
                            token.delta_start
                        };
                        line == 1 && column == 7 && token.token_type as usize == function
                    });
                assert!(classified, "inner helper must be classified as a function");
            }
        }
    }
}

#[test]
fn inner_comparison_promise_fix_applies_to_the_inner_declaration() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("inner-comparison-promise-fix");
        let source = "pub fn exercise () -> Bool =\n    let label = \"🧭 café\"\n    fn same (left: a) (right: a) -> Bool = left == right\n    same 1 1\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        std::fs::write(&file, &source).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        assert!(state
            .type_errors
            .iter()
            .any(|(_, error)| error.code() == "T055"));
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
            .find(|fix| fix.uri == uri && fix.code == "T055")
            .unwrap();
        assert_eq!(fix.edit_range.start.line, 2);
        assert_eq!(fix.new_text, " where Eq a");
        let lines = ridge_lexer::LineIndex::new(&source);
        let at =
            lines.utf16_to_byte(fix.edit_range.start.line, fix.edit_range.start.character) as usize;
        let mut repaired = source.clone();
        repaired.insert_str(at, &fix.new_text);
        state.recompile(module, &repaired);
        assert!(state.resolved.parse_errors.is_empty());
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
        state.recompile(module, &source);
        assert!(state
            .type_errors
            .iter()
            .any(|(_, error)| error.code() == "T055"));
    }
}

#[test]
fn shared_fundep_results_follow_unsaved_context_and_reject_conflicts() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("shared-fundep-context-editor");
        let source = "type InputBox a = { value: a }\ntype OutputBox a = { value: a }\nclass Move q p | q -> p =\n    transfer (value: q) -> p\ninstance Move (InputBox a) (OutputBox a) =\n    transfer (value: InputBox a) -> OutputBox a = OutputBox { value = value.value }\npub fn check () -> OutputBox (List Int) =\n    let label = \"🧭 café\"\n    transfer (InputBox { value = [] })\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        std::fs::write(&file, &source).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        let text = source
            .replace("List Int", "List Text")
            .replace("🧭 café", "🧭 🧭 café");
        for (buffer, invalid) in [
            (source.clone(), false),
            (text.clone(), false),
            (text.replace("value = []", "value = [1]"), true),
            (source, false),
        ] {
            state.recompile(module, &buffer);
            assert!(state.resolved.parse_errors.is_empty());
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            if invalid {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(_, error)| error.code() == "T001"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let (hover, _) = index.hover_at(&uri, 8, 5).unwrap();
                assert!(hover.contains("where Move q p"), "{hover}");
                let help = index.signature_help_at(&uri, 8, 13, true).unwrap();
                assert!(
                    help.signatures[0].label.contains("where Move q p"),
                    "{:?}",
                    help.signatures
                );
            }
        }
    }
}

#[test]
fn comparison_tokens_share_method_navigation_and_preserve_fixed_syntax() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("comparison-token-editor");
        let source = "pub type Key = { id: Int }\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Bool = true\ninstance Ord Key =\n    compare (left: Key) (right: Key) -> Ordering = Equal\npub fn check (left: Key) (right: Key) -> Bool =\n    let label = \"🧭 café\"\n    (left == right) && (left != right) && (left < right) && (left <= right) && (left > right) && (left >= right)\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        std::fs::write(&file, &source).unwrap();
        let state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
        let source = source.replace(
            "(left == right)",
            "(\"🧭 café\" == \"🧭 café\") && (left == right)",
        );
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        let mut state = state;
        state.recompile(module, &source);
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        let index = WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
        let line = source.lines().nth(7).unwrap();
        for (op, class, method_line, count) in [
            ("==", "Eq", 2, 3),
            ("!=", "Eq", 2, 3),
            ("<", "Ord", 4, 4),
            ("<=", "Ord", 4, 4),
            (">", "Ord", 4, 4),
            (">=", "Ord", 4, 4),
        ] {
            let byte = line.rfind(&format!(" {op} ")).unwrap() + 1;
            let col = u32::try_from(line[..byte].encode_utf16().count()).unwrap();
            let (hover, span) = index.hover_at(&uri, 7, col).unwrap();
            assert!(hover.contains(class), "{hover}");
            assert_eq!(span.end - span.start, u32::try_from(op.len()).unwrap());
            let implementations = index.implementations_at(&uri, 7, col).unwrap();
            assert_eq!(implementations.len(), 1);
            assert_eq!(implementations[0].range.start.line, method_line);
            let definition = index.definition_at(&uri, 7, col).unwrap();
            assert_eq!(Some(definition.clone()), index.declaration_at(&uri, 7, col));
            let declaration =
                std::fs::read_to_string(definition.uri.to_file_path().unwrap()).unwrap();
            assert!(declaration.contains(&format!("class {class} a")));
            let refs = index
                .references_at(&uri, 7, col, false, &ridge_lsp::cancel::Cancel::new())
                .unwrap();
            assert_eq!(refs.len(), count, "{op}: {refs:?}");
            assert_eq!(
                index.document_highlights_at(&uri, 7, col).unwrap().len(),
                count
            );
            assert!(index.prepare_rename_at(&uri, 7, col).is_none());
        }
        let tokens = index.semantic_tokens(&uri).unwrap();
        let operator = ridge_lsp::index::SEMANTIC_TOKEN_TYPES
            .iter()
            .position(|ty| *ty == tower_lsp::lsp_types::SemanticTokenType::OPERATOR)
            .unwrap();
        assert_eq!(
            tokens
                .data
                .iter()
                .filter(|token| token.token_type as usize == operator)
                .count(),
            7
        );
    }
}

#[test]
fn hash_contract_has_source_backed_completion_and_signature() {
    let (_dir, root) = editor_workspace("hash-class-editor");
    let source =
        "pub fn feed (value: a) (hasher: Hasher) -> Hasher where Hash a = hash value hasher\n";
    let file = root.join("app/src/Main.ridge");
    let uri = Url::from_file_path(&file).unwrap();
    std::fs::write(&file, source).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let class_col = u32::try_from(source.find("Hash a").unwrap()).unwrap();
    assert!(
        state.resolved.parse_errors.is_empty(),
        "{:?}",
        state.resolved.parse_errors
    );
    let method_col = u32::try_from(source.find("hash value").unwrap()).unwrap();
    for col in [class_col, method_col] {
        let (hover, _) = index.hover_at(&uri, 0, col).unwrap();
        assert!(hover.contains("hash") || hover.contains("Hash"), "{hover}");
        assert!(index.definition_at(&uri, 0, col).is_some());
    }
    let completion = index
        .completions_at(&uri, 0, method_col + 2)
        .into_iter()
        .find(|c| c.label == "hash")
        .unwrap();
    let (detail, _) = index
        .resolve_completion(completion.data.as_ref().unwrap())
        .unwrap();
    assert!(detail.contains("Hasher"), "{detail}");
    let signature = index
        .signature_help_at(&uri, 0, method_col + 6, false)
        .unwrap();
    assert!(signature.signatures[0].label.contains("Hasher"));
    let class_completion = index
        .completions_at(&uri, 0, class_col + 2)
        .into_iter()
        .find(|c| c.label == "Hash")
        .unwrap();
    let (detail, _) = index
        .resolve_completion(class_completion.data.as_ref().unwrap())
        .unwrap();
    assert!(detail.contains("where Eq a"), "{detail}");
    let hierarchy = index.prepare_type_hierarchy_at(&uri, 0, class_col).unwrap();
    let supers = index
        .type_supertypes(hierarchy[0].data.as_ref().unwrap())
        .unwrap();
    assert_eq!(
        supers
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["Eq"]
    );
    let children = index
        .type_subtypes(supers[0].data.as_ref().unwrap())
        .unwrap();
    assert!(children.iter().any(|item| item.name == "Hash"));
    assert!(children.iter().any(|item| item.name == "Ord"));
}

#[test]
fn quoted_float_comparison_has_sql_hover_without_class_navigation() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("comparison-sql-editor");
        let source = "type Reading = { value: Float }\nfn predicate (q: Quote (Reading -> Bool)) -> Bool = true\npub fn check () -> Bool = predicate (fn row -> row.value >= 1.0)\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        std::fs::write(&file, &source).unwrap();
        let state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let col = u32::try_from(source.lines().nth(2).unwrap().find(">=").unwrap()).unwrap();
        let (hover, _) = index.hover_at(&uri, 2, col).unwrap();
        assert!(hover.contains("SQL comparison"), "{hover}");
        assert!(index.implementations_at(&uri, 2, col).is_none());
        assert!(index
            .references_at(&uri, 2, col, false, &ridge_lsp::cancel::Cancel::new())
            .is_none());
        assert!(index.prepare_rename_at(&uri, 2, col).is_none());
    }
}

#[test]
fn operators_keep_inferred_contracts_through_unsaved_edits() {
    for ending in ["\n", "\r\n"] {
        for (operator, class) in [("==", "Eq"), ("<", "Ord")] {
            let (_dir, root) = editor_workspace("comparison-operators-editor");
            let valid = format!(
            "-- café 🧭\nfn check left right = left {operator} right\npub fn exercise () -> Bool = check 1 1\n"
        ).replace('\n', ending);
            let file = root.join("app/src/Main.ridge");
            let uri = Url::from_file_path(&file).unwrap();
            std::fs::write(&file, &valid).unwrap();
            let mut state =
                check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true))
                    .unwrap();
            let module = state
                .resolved
                .graph
                .modules
                .iter()
                .find(|m| m.file_path.ends_with("Main.ridge"))
                .unwrap()
                .id;
            for (source, invalid) in [
                (valid.clone(), false),
                (valid.replace("check 1 1", "check 1.0 1.0"), true),
                (
                    valid.replace("check 1 1", "check (fn () -> 1) (fn () -> 1)"),
                    true,
                ),
                (valid, false),
            ] {
                state.recompile(module, &source);
                assert!(state.resolved.lex_errors.is_empty());
                assert!(
                    state.resolved.errors.is_empty(),
                    "{:?}",
                    state.resolved.errors
                );
                assert!(
                    state.resolved.parse_errors.is_empty(),
                    "{:?}",
                    state.resolved.parse_errors
                );
                if invalid {
                    assert!(
                        state.type_errors.iter().any(|(_, e)| e.code() == "T029"),
                        "{:?}",
                        state.type_errors
                    );
                    if source.contains("1.0") {
                        let expected = if class == "Eq" {
                            "Float.equal"
                        } else {
                            "Float.less"
                        };
                        assert!(state.type_errors.iter().any(|(_, error)| error
                            .render(&state.typed.tycons)
                            .contains(expected)));
                    }
                } else {
                    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                    let index = WorkspaceIndex::build(
                        0,
                        &state.typed,
                        &state.resolved,
                        &state.source_cache(),
                    );
                    let (hover, _) = index.hover_at(&uri, 1, 4).unwrap();
                    assert!(hover.contains(class), "{hover}");
                }
            }
        }
    }
}

#[test]
fn collection_update_keeps_key_constraints_and_function_values_in_the_editor() {
    let (_dir, root) = editor_workspace("collection-update-editor");
    let valid = "import std.map as Map\nfn modify key f items = Map.update key f items\npub fn exercise () -> Map Int (fn () -> Int) =\n    modify 7 (fn _ -> fn () -> 9) Map.empty\n";
    let file = root.join("app/src/Main.ridge");
    let uri = Url::from_file_path(&file).unwrap();
    std::fs::write(&file, valid).unwrap();
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
    let invalid = valid
        .replace("Map Int", "Map Float")
        .replace("modify 7 ", "modify 7.0 ");
    for (source, reject) in [(valid, false), (invalid.as_str(), true), (valid, false)] {
        state.recompile(module, source);
        assert!(state.resolved.lex_errors.is_empty());
        assert!(state.resolved.parse_errors.is_empty());
        assert!(state.resolved.errors.is_empty());
        if reject {
            assert!(
                state
                    .type_errors
                    .iter()
                    .any(|(_, error)| error.code() == "T029"),
                "{:?}",
                state.type_errors
            );
        } else {
            assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
            assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            let index =
                WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
            let (hover, _) = index.hover_at(&uri, 1, 4).unwrap();
            assert!(
                hover.contains("Hash") && hover.contains("Option"),
                "{hover}"
            );
        }
    }
}

#[test]
fn collection_primitive_requirements_survive_incremental_edits() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("collection-primitive-editor");
        let valid = "-- café 🧭\n@primitive\nfn keyMethods () -> (fn a Hasher -> Hasher, fn a a -> Bool) where Hash a, Eq a\npub fn use () -> (fn a Hasher -> Hasher, fn a a -> Bool) where Hash a = keyMethods ()\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        std::fs::write(&file, &valid).unwrap();
        let mut options = CheckOptions::new(root).with_retain_indices(true);
        options.is_stdlib = true;
        let mut state = check_workspace_incremental(options).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (source, invalid) in [
            (valid.clone(), false),
            (
                valid.replace(" where Hash a = keyMethods", " = keyMethods"),
                true,
            ),
            (valid, false),
        ] {
            state.recompile(module, &source);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(state.resolved.parse_errors.is_empty());
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            if invalid {
                assert!(
                    state.type_errors.iter().any(|(_, e)| e.code() == "T055"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let prefix = &source[..source.find("keyMethods ()").unwrap()];
                let line = u32::try_from(prefix.matches('\n').count()).unwrap();
                let column =
                    u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count())
                        .unwrap();
                let (hover, _) = index.hover_at(&uri, line, column).unwrap();
                assert!(
                    hover.contains("Hash") && hover.contains("Eq") && hover.contains("Hasher"),
                    "{hover}"
                );
            }
        }
    }
}

#[test]
fn collection_value_equality_follows_unsaved_edits() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("collection-value-editor");
        let valid = "-- café 🧭\nimport std.map as Map\nfn same (left: Map k v) (right: Map k v) -> Bool where Hash k, Eq v = eq left right\npub fn exercise () -> Bool =\n    let value = 7\n    same (Map.fromList [(1, value)]) (Map.fromList [(1, value)])\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
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
        for (source, invalid) in [
            (valid.clone(), false),
            (valid.replace("value = 7", "value = 7.0"), true),
            (valid.replace("value = 7", "value = fn () -> 7"), true),
            (valid, false),
        ] {
            state.recompile(module, &source);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(state.resolved.parse_errors.is_empty());
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            if invalid {
                assert!(
                    state.type_errors.iter().any(|(_, e)| e.code() == "T029"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let prefix = &source[..source.find("same (left").unwrap()];
                let line = u32::try_from(prefix.matches('\n').count()).unwrap();
                let column =
                    u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count())
                        .unwrap();
                let (hover, _) = index.hover_at(&uri, line, column).unwrap();
                assert!(
                    hover.contains("Hash") && hover.contains("Eq") && hover.contains("Map"),
                    "{hover}"
                );
            }
        }
    }
}

#[test]
fn collection_key_evidence_follows_unsaved_edits_and_generic_hover() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("collection-key-editor");
        let valid = "-- café 🧭\nimport std.map as Map\nimport std.set as Set\nimport std.list as List\nfn lookup key items = Map.get key items\npub fn exercise () -> Bool =\n    let key = 7\n    let items = Map.fromList [(key, fn () -> 1)]\n    let group = List.groupBy (fn x -> x) [key]\n    Set.contains key (Set.fromList [key]) && Map.contains key items && Map.contains key group\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
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
        for (source, invalid) in [
            (valid.clone(), false),
            (valid.replace("key = 7", "key = 7.0"), true),
            (valid.replace("key = 7", "key = fn () -> 7"), true),
            (valid, false),
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
            if invalid {
                assert!(
                    state.type_errors.iter().any(|(_, e)| e.code() == "T029"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let prefix = &source[..source.find("lookup key").unwrap()];
                let line = u32::try_from(prefix.matches('\n').count()).unwrap();
                let column =
                    u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count())
                        .unwrap();
                let (hover, _) = index.hover_at(&uri, line, column).unwrap();
                assert!(
                    hover.contains("Hash") && hover.contains("Map") && hover.contains("Option"),
                    "{hover}"
                );
            }
        }
    }
}

#[test]
fn float_predicate_types_and_errors_follow_unsaved_argument_edits() {
    for ending in ["\n", "\r\n"] {
        for name in ["equal", "less", "lessOrEqual", "greater", "greaterOrEqual"] {
            let dir = tempfile::tempdir().unwrap();
            let root = ridge_manifest::canonicalize(dir.path()).unwrap();
            std::fs::create_dir_all(root.join("app/src")).unwrap();
            std::fs::write(
                root.join("ridge.toml"),
                "[workspace]\nname=\"comparison-editor\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
            )
            .unwrap();
            std::fs::write(
                root.join("app/ridge.toml"),
                "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
            )
            .unwrap();
            let file = root.join("app/src/Main.ridge");
            let uri = Url::from_file_path(&file).unwrap();
            let valid = format!(
                "-- café 🧭\nimport std.float as F\npub fn exercise () -> Bool =\n    let predicate = F.{name}\n    predicate 0.0 (-0.0)\n"
            )
            .replace('\n', ending);
            std::fs::write(&file, &valid).unwrap();
            let mut state =
                check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true))
                    .unwrap();
            let module = state
                .resolved
                .graph
                .modules
                .iter()
                .find(|m| m.file_path.ends_with("Main.ridge"))
                .unwrap()
                .id;
            for (source, invalid) in [
                (valid.clone(), false),
                (valid.replace("predicate 0.0", "predicate 0"), true),
                (valid.clone(), false),
            ] {
                state.recompile(module, &source);
                assert!(state.resolved.lex_errors.is_empty());
                assert!(state.resolved.parse_errors.is_empty());
                assert!(state.resolved.errors.is_empty());
                if invalid {
                    assert!(state
                        .type_errors
                        .iter()
                        .any(|(_, error)| error.code() == "T002"));
                } else {
                    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                    let index = WorkspaceIndex::build(
                        0,
                        &state.typed,
                        &state.resolved,
                        &state.source_cache(),
                    );
                    let offset = source.find("predicate =").unwrap();
                    let prefix = &source[..offset];
                    let line = u32::try_from(prefix.matches('\n').count()).unwrap();
                    let column =
                        u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count())
                            .unwrap();
                    let (hover, _) = index.hover_at(&uri, line, column).unwrap();
                    assert_eq!(hover.matches("Float").count(), 2, "{hover}");
                    assert!(hover.contains("Bool"), "{hover}");
                }
            }
        }
    }
}

#[test]
fn structural_component_requirements_follow_unsaved_editor_edits() {
    for ending in ["\n", "\r\n"] {
        for expression in [
            "eq (left, 7) (right, 7)",
            "eq ({ key = left }) ({ key = right })",
            "compare (left, 7) (right, 7) == Equal",
            "compare ({ key = left }) ({ key = right }) == Equal",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = ridge_manifest::canonicalize(dir.path()).unwrap();
            std::fs::create_dir_all(root.join("app/src")).unwrap();
            std::fs::write(
                root.join("ridge.toml"),
                "[workspace]\nname=\"structural-editor\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
            )
            .unwrap();
            std::fs::write(
                root.join("app/ridge.toml"),
                "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
            )
            .unwrap();
            let file = root.join("app/src/Main.ridge");
            let uri = Url::from_file_path(&file).unwrap();
            let valid = format!("-- café 🧭\npub fn exercise () -> Bool =\n    let left = 1\n    let right = 1\n    {expression}\n").replace('\n', ending);
            let invalid = valid
                .replace("left = 1", "left = 1.0")
                .replace("right = 1", "right = 1.0");
            std::fs::write(&file, &valid).unwrap();
            let mut state =
                check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true))
                    .unwrap();
            let module = state
                .resolved
                .graph
                .modules
                .iter()
                .find(|m| m.file_path.ends_with("Main.ridge"))
                .unwrap()
                .id;
            for (source, invalid) in [
                (valid.clone(), false),
                (invalid, true),
                (valid.clone(), false),
            ] {
                state.recompile(module, &source);
                assert!(state.resolved.lex_errors.is_empty());
                assert!(state.resolved.parse_errors.is_empty());
                assert!(state.resolved.errors.is_empty());
                if invalid {
                    assert!(
                        state
                            .type_errors
                            .iter()
                            .any(|(_, error)| error.code() == "T029"),
                        "{expression}: {:?}",
                        state.type_errors
                    );
                } else {
                    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                    let index = WorkspaceIndex::build(
                        0,
                        &state.typed,
                        &state.resolved,
                        &state.source_cache(),
                    );
                    let (hover, _) = index.hover_at(&uri, 1, 7).unwrap();
                    assert!(hover.contains("Bool"), "{hover}");
                }
            }
        }
    }
}

#[test]
fn same_named_comparison_types_keep_nominal_identity_during_editor_edits() {
    for ending in ["\n", "\r\n"] {
        let (dir, root) = editor_workspace("nominal-editor");
        for name in ["A", "B"] {
            let prefix = if name == "A" {
                "pub type Entity = { id: Int } deriving (Schema)\n"
            } else {
                "import app.A as A\n"
            };
            std::fs::write(root.join(format!("app/src/{name}.ridge")),
                format!("{prefix}pub type Key = {{ id: Int }}\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Bool = left.id == right.id\npub fn key (id: Int) -> Key = Key {{ id = id }}\n")
                    .replace('\n', ending)).unwrap();
        }
        let source = "-- café 🧭\nimport app.A as A\nimport app.B as B\npub fn exercise () -> Bool = eq (A.key 1) (A.key 2) && eq (B.key 1) (B.key 2)\n"
            .replace('\n', ending);
        std::fs::write(root.join("app/src/Main.ridge"), &source).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (text, invalid) in [
            (source.clone(), false),
            (source.replacen("(A.key 2)", "(B.key 2)", 1), true),
            (source.clone(), false),
        ] {
            state.recompile(module, &text);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(state.resolved.parse_errors.is_empty());
            assert!(state.resolved.errors.is_empty());
            if invalid {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(_, error)| error.code() == "T002"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            }
        }
        let producer = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("B.ridge"))
            .unwrap()
            .id;
        let valid = std::fs::read_to_string(dir.path().join("app/src/B.ridge")).unwrap();
        let added = "\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Bool = true\n"
            .replace('\n', ending);
        let duplicate = format!("{valid}{added}");
        let helper = "\nfn helper () -> Int = 1\n".replace('\n', ending);
        let duplicate_with_body = format!("{duplicate}{helper}");
        for (text, invalid) in [
            (valid.clone(), false),
            (duplicate, true),
            (duplicate_with_body.clone(), true),
            (
                duplicate_with_body.replace("-> Int = 1", "-> Int = 2"),
                true,
            ),
            (valid, false),
        ] {
            state.recompile(producer, &text);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(state.resolved.parse_errors.is_empty());
            if invalid {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(owner, error)| *owner == producer && error.code() == "T032"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.resolved.errors.is_empty());
            }
        }
    }
}

#[test]
fn independent_container_requirements_survive_generic_editor_edits() {
    for header in [
        "fn both left right",
        "fn both (left: a) right",
        "fn both (left: a) (right: b) -> (JsonValue, JsonValue) where Encode a, Encode b",
    ] {
        for ending in ["\n", "\r\n"] {
            let dir = tempfile::tempdir().unwrap();
            let root = ridge_manifest::canonicalize(dir.path()).unwrap();
            std::fs::create_dir_all(root.join("app/src")).unwrap();
            std::fs::write(
                root.join("ridge.toml"),
                "[workspace]\nname=\"evidence-editor\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
            )
            .unwrap();
            std::fs::write(
                root.join("app/ridge.toml"),
                "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
            )
            .unwrap();
            let valid = format!("-- café 🧭\nfn ready () = true\n{header} = (encode [left], encode [right])\npub fn exercise () = both 1 2\n").replace('\n', ending);
            let file = root.join("app/src/Main.ridge");
            let uri = Url::from_file_path(&file).unwrap();
            std::fs::write(&file, &valid).unwrap();
            let mut state =
                check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true))
                    .unwrap();
            let module = state
                .resolved
                .graph
                .modules
                .iter()
                .find(|module| module.file_path.ends_with("Main.ridge"))
                .unwrap()
                .id;
            for (source, invalid) in [
                (valid.clone(), false),
                (valid.replace("both 1 2", "both ready 2"), true),
                (valid.replace("both 1 2", "both 1 ready"), true),
                (valid.clone(), false),
            ] {
                state.recompile(module, &source);
                assert!(state.resolved.lex_errors.is_empty());
                assert!(state.resolved.parse_errors.is_empty());
                assert!(state.resolved.errors.is_empty());
                if invalid {
                    assert!(
                        state
                            .type_errors
                            .iter()
                            .any(|(_, error)| error.code() == "T029"),
                        "{:?}",
                        state.type_errors
                    );
                } else {
                    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                    let index = WorkspaceIndex::build(
                        0,
                        &state.typed,
                        &state.resolved,
                        &state.source_cache(),
                    );
                    let (hover, _) = index.hover_at(&uri, 2, 3).unwrap();
                    assert_eq!(hover.matches("Encode").count(), 2, "{hover}");
                    assert!(hover.contains("where Encode a, Encode b"), "{hover}");
                    let call = "pub fn exercise () = both 1 2";
                    let callee = u32::try_from(call.find("both").unwrap()).unwrap();
                    let (use_hover, _) = index.hover_at(&uri, 3, callee).unwrap();
                    assert!(
                        use_hover.contains("where Encode a, Encode b"),
                        "{use_hover}"
                    );
                    let help = index.signature_help_at(&uri, 3, callee + 6, true).unwrap();
                    let signature = &help.signatures[0];
                    assert!(
                        signature.label.contains("where Encode a, Encode b"),
                        "{signature:?}"
                    );
                    assert_eq!(signature.parameters.as_ref().unwrap().len(), 2);
                }
            }
        }
    }
}

#[test]
fn derived_field_requirements_follow_unsaved_calls_and_type_edits() {
    for ending in ["\n", "\r\n"] {
        let dir = tempfile::tempdir().unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("app/src")).unwrap();
        std::fs::write(
            root.join("ridge.toml"),
            "[workspace]\nname=\"derived-editor\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("app/ridge.toml"),
            "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
        )
        .unwrap();
        let valid = "-- café 🧭\ntype Box a = { value: a } deriving (Eq, Ord)\nfn ready () = true\nfn both left right = (eq (Box { value = left }) (Box { value = left }), compare (Box { value = right }) (Box { value = right }))\npub fn exercise () = both 1 2\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        std::fs::write(&file, &valid).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (text, invalid) in [
            (valid.clone(), None),
            (valid.replace("both 1 2", "both ready 2"), Some("Eq")),
            (valid.replace("both 1 2", "both 1 ready"), Some("Ord")),
            (valid.clone(), None),
            (valid.replace("value: a", "value: Float"), Some("Eq")),
            (valid.clone(), None),
        ] {
            state.recompile(module, &text);
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
            if let Some(class) = invalid {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(_, error)| error.code() == "T029"
                            && error.render(&state.typed.tycons).contains(class)),
                    "{text}\n{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let (hover, _) = index.hover_at(&uri, 3, 3).unwrap();
                assert!(hover.contains("Eq a") && hover.contains("Ord b"), "{hover}");
                let call = "pub fn exercise () = both 1 2";
                let column = u32::try_from(call.find("both").unwrap()).unwrap();
                let help = index.signature_help_at(&uri, 4, column + 6, false).unwrap();
                assert!(
                    help.signatures[0].label.contains("Eq a")
                        && help.signatures[0].label.contains("Ord b"),
                    "{help:?}"
                );
            }
        }
    }
}

#[test]
fn complete_function_heads_follow_unsaved_editor_arguments() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("function-head-editor");
        let template = r#"
import std.io as Io
type Input = Int
class Predicate a =
    evaluate (value: a) -> Bool
instance Predicate (fn Input -> Bool) =
    evaluate (value: fn Int -> Bool) -> Bool = value 1
type Carrier a = { value: a }
instance Eq (Carrier a) where Predicate a =
    eq (left: Carrier a) (right: Carrier a) -> Bool = evaluate left.value && evaluate right.value
type Outer a = { wrapped: Carrier a } deriving (Eq)
fn pureValue (value: Int) -> Bool = value == 1
fn wrongParameter (value: Bool) -> Bool = value
fn wrongReturn (value: Int) -> Int = value
fn io effect (value: Int) -> Bool =
    Io.println "effect"
    value == 1
pub fn exercise () -> Bool = eq (Outer { wrapped = Carrier { value = TARGET } }) (Outer { wrapped = Carrier { value = TARGET } })
"#.replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        std::fs::write(&file, template.replace("TARGET", "pureValue")).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for target in [
            "pureValue",
            "effect",
            "pureValue",
            "wrongParameter",
            "wrongReturn",
            "pureValue",
        ] {
            state.recompile(module, &template.replace("TARGET", target));
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
            if target == "pureValue" {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            } else {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(_, error)| error.code() == "T029"),
                    "{target}: {:?}",
                    state.type_errors
                );
            }
        }
    }
}

#[test]
fn derived_comparisons_follow_private_alias_edits() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("derived-alias-editor");
        let valid = "type Field a = a\ntype Box a = { value: Field a } deriving (Eq, Ord)\npub fn exercise () -> Bool = eq (Box { value = 1 }) (Box { value = 1 })\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        std::fs::write(&file, &valid).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (text, invalid) in [
            (valid.clone(), false),
            (valid.replace("Field a = a", "Field a = Float"), true),
            (valid, false),
        ] {
            state.recompile(module, &text);
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
            if invalid {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(_, error)| error.code() == "T029"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            }
        }
    }
}

#[test]
fn implicit_row_ownership_follows_unsaved_opaque_edits() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("opaque-row-editor");
        let plain = "import std.sql (Row)\npub type Age = { value: Int }\n".replace('\n', ending);
        let opaque = plain.replace("pub type", "pub opaque type");
        let explicit = opaque.replace("{ value: Int }", "{ value: Int } deriving (Row)");
        std::fs::write(root.join("app/src/A.ridge"), &plain).unwrap();
        let source = r#"
import app.A (Age)
import std.sql as Sql
import std.map as Map
pub fn exercise () -> Result Age Error = Sql.fromRow (Map.fromList [("value", Sql.toSql (-1))])
"#
        .replace('\n', ending);
        std::fs::write(root.join("app/src/Main.ridge"), source).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let producer = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("A.ridge"))
            .unwrap()
            .id;
        let consumer = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (text, invalid) in [
            (plain.clone(), false),
            (opaque.clone(), true),
            (explicit, false),
            (opaque, true),
            (plain, false),
        ] {
            state.recompile(producer, &text);
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
            if invalid {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(module, error)| *module == consumer && error.code() == "T029"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            }
        }
    }
}

#[test]
fn derived_query_contexts_follow_unsaved_predicate_arity() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("derived-query-editor");
        let valid = r"
import std.repo (Query, Joined, Refinable)
import std.data (MemAdapter)
type Person = { id: Int }
type Carrier q p = { receiver: q, predicate: p }
instance Eq (Carrier q p) where Refinable q p =
    eq (left: Carrier q p) (right: Carrier q p) -> Bool = true
type Outer = { value: Carrier (Joined (Query Person MemAdapter) Person MemAdapter) (fn Person Person -> Bool) } deriving (Eq)
pub fn exercise () -> Bool = true
".replace('\n', ending);
        std::fs::write(root.join("app/src/Main.ridge"), &valid).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (source, invalid) in [
            (valid.clone(), false),
            (
                valid.replace("fn Person Person -> Bool", "fn Person -> Bool"),
                true,
            ),
            (valid, false),
        ] {
            state.recompile(module, &source);
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
            if invalid {
                assert!(
                    !state.type_errors.is_empty(),
                    "a two-row context cannot accept a one-argument predicate"
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            }
        }
    }
}

#[test]
fn decimal_value_evidence_follows_unsaved_numeric_edits() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("decimal-evidence-editor");
        let valid =
            "fn same left right = eq left right\npub fn exercise () -> Bool = same 1.5m 1.50m\n"
                .replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        std::fs::write(&file, &valid).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (source, invalid) in [
            (valid.clone(), false),
            (valid.replace("1.5m 1.50m", "1.5 1.50"), true),
            (valid, false),
        ] {
            state.recompile(module, &source);
            assert!(state.resolved.parse_errors.is_empty());
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            if invalid {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(_, error)| error.code() == "T029"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let (hover, _) = index.hover_at(&uri, 0, 4).unwrap();
                assert!(hover.contains("Eq a"), "{hover}");
            }
        }
    }
}

#[test]
fn list_equality_requirements_follow_unsaved_value_edits() {
    for ending in ["\n", "\r\n"] {
        for expression in ["List.contains value [value]", "List.unique [value]"] {
            let (_dir, root) = editor_workspace("list-equality-editor");
            let valid = format!(
                "-- café 🧭\nfn inspect value = {expression}\npub fn exercise () = inspect 1.5m\n"
            )
            .replace('\n', ending);
            let file = root.join("app/src/Main.ridge");
            let uri = Url::from_file_path(&file).unwrap();
            std::fs::write(&file, &valid).unwrap();
            let mut state =
                check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true))
                    .unwrap();
            let module = state
                .resolved
                .graph
                .modules
                .iter()
                .find(|module| module.file_path.ends_with("Main.ridge"))
                .unwrap()
                .id;
            for (source, invalid) in [
                (valid.clone(), false),
                (valid.replace("inspect 1.5m", "inspect 1.5"), true),
                (valid, false),
            ] {
                state.recompile(module, &source);
                assert!(state.resolved.lex_errors.is_empty());
                assert!(state.resolved.parse_errors.is_empty());
                assert!(
                    state.resolved.errors.is_empty(),
                    "{:?}",
                    state.resolved.errors
                );
                if invalid {
                    assert!(
                        state
                            .type_errors
                            .iter()
                            .any(|(_, error)| error.code() == "T029"),
                        "{:?}",
                        state.type_errors
                    );
                } else {
                    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                    let index = WorkspaceIndex::build(
                        0,
                        &state.typed,
                        &state.resolved,
                        &state.source_cache(),
                    );
                    let (hover, _) = index.hover_at(&uri, 1, 4).unwrap();
                    assert!(hover.contains("Eq a"), "{hover}");
                }
            }
        }
    }
}

#[test]
fn instance_contracts_reject_unsaved_return_and_effect_changes() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("instance-contract-editor");
        let valid = r"-- café 🧭
import std.io as Io
type Key = { id: Int } deriving (Eq)
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hasher
pub fn emit (value: Key) (hasher: Hasher) -> Hasher = hash value hasher
"
        .replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        std::fs::write(&file, &valid).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (source, invalid) in [
            (valid.clone(), false),
            (valid.replace("(hasher: Hasher) -> Hasher = hasher", "(hasher: Int) -> Int = hasher"), true),
            (valid.clone(), false),
            (valid.replace("-> Hasher = hasher", &format!("-> Hasher ={ending}        Io.println \"unexpected effect\"{ending}        hasher")), true),
            (valid, false),
        ] {
            state.recompile(module, &source);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(state.resolved.parse_errors.is_empty(), "{:?}", state.resolved.parse_errors);
            assert!(state.resolved.errors.is_empty(), "{:?}", state.resolved.errors);
            if invalid {
                assert!(!state.type_errors.is_empty(), "must reject the edited method");
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            }
        }
    }
}

#[test]
fn compound_hash_editor_handles_missing_container_evidence() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("compound-hash-missing-editor");
        let valid = "fn ready () = true\nfn absent () -> Option Float = None\nfn result () -> Result Int Float = Ok 7\nfn emit value hasher = hash value hasher\npub fn exercise (hasher: Hasher) -> Hasher = emit [7] hasher\n".replace('\n', ending);
        std::fs::write(root.join("app/src/Main.ridge"), &valid).unwrap();
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
        for (source, invalid) in [
            (valid.clone(), false),
            (valid.replace("emit [7]", "emit [ready]"), true),
            (valid.clone(), false),
            (valid.replace("emit [7]", "emit (absent ())"), true),
            (valid.replace("emit [7]", "emit (result ())"), true),
            (valid, false),
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
            if invalid {
                assert!(
                    state.type_errors.iter().any(|(_, e)| e.code() == "T029"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
            }
            assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
        }
    }
}

#[test]
fn compound_hash_derives_follow_unsaved_fields_and_generic_calls() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("compound-hash-editor");
        let valid = "-- café 🧭\ntype Box a = { value: a } deriving (Eq, Hash)\nfn ready () = true\nfn emit value hasher = hash (Box { value = value }) hasher\npub fn exercise (hasher: Hasher) -> Hasher = emit 7 hasher\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
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
        for (source, invalid) in [
            (valid.clone(), false),
            (valid.replace("emit 7", "emit ready"), true),
            (valid.clone(), false),
            (valid.replace("value: a", "value: Float"), true),
            (valid, false),
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
            if invalid {
                assert!(
                    state.type_errors.iter().any(|(_, e)| e.code() == "T029"
                        && e.render(&state.typed.tycons).contains("Hash")),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let (hover, _) = index.hover_at(&uri, 3, 3).unwrap();
                assert!(hover.contains("Hash a"), "{hover}");
            }
        }
    }
}

#[test]
fn primitive_hash_evidence_follows_unsaved_literal_edits() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("primitive-hash-editor");
        let valid = "fn emit (value: a) (hasher: Hasher) -> Hasher where Hash a = hash value hasher\npub fn exercise (hasher: Hasher) -> Hasher = emit 7 hasher\n".replace('\n', ending);
        std::fs::write(root.join("app/src/Main.ridge"), &valid).unwrap();
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
        for (source, invalid) in [
            (valid.clone(), false),
            (valid.replace("emit 7", "emit 7.0"), true),
            (valid, false),
        ] {
            state.recompile(module, &source);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(state.resolved.parse_errors.is_empty());
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            if invalid {
                assert!(
                    state.type_errors.iter().any(|(_, e)| e.code() == "T029"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            }
        }
    }
}

#[test]
fn renderer_effect_edits_remove_and_restore_automatic_evidence() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace("renderer-effect-editor");
        let valid = r#"import app.Renderer (render)
type Printable = { value: Int }
pub fn toText (value: Printable) -> Text = "rendered"
pub fn exercise () -> Text = render (Printable { value = 7 })
"#
        .replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        std::fs::write(&file, &valid).unwrap();
        std::fs::write(
            root.join("app/src/Renderer.ridge"),
            "pub fn render (value: a) -> Text where ToText a = toText value\n",
        )
        .unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for (source, invalid) in [
            (valid.clone(), false),
            (valid.replace("pub fn toText", "pub fn io toText"), true),
            (valid, false),
        ] {
            state.recompile(module, &source);
            assert!(state.resolved.lex_errors.is_empty());
            assert!(state.resolved.parse_errors.is_empty());
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            if invalid {
                assert!(
                    state
                        .type_errors
                        .iter()
                        .any(|(_, error)| error.code() == "T029"),
                    "{:?}",
                    state.type_errors
                );
            } else {
                assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
                assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            }
        }
    }
}
