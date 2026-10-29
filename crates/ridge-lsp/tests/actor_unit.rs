//! Unit payloads and nullary markers remain distinct through editor edits.
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

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"actor-unit-editor\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    dir
}

#[test]
fn function_call_fixes_do_not_guess_unit_payloads_or_permissions() {
    for source in [
        "fn ready (_: Unit) -> Bool = true\npub fn choose () -> Int = if ready then 7 else 0\n",
        "fn ready -> Int = 1\npub fn choose () -> Int = if ready then 7 else 0\n",
        "import std.io as Io\nfn io ready -> Bool =\n    Io.println \"effect\"\n    true\npub fn choose () -> Int = if ready then 7 else 0\n",
        "pub fn unresolved h -> Unit = h ! ping\n",
    ] {
        let dir = workspace();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("app/src/Main.ridge"), source).unwrap();
        let state = check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        assert!(!state.type_errors.is_empty());
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        assert!(ridge_lsp::explicit_calls::collect_function_call_fixes(&index, &state.typed, &state.resolved, &state.type_errors).is_empty());
        assert!(ridge_lsp::explicit_calls::collect_explicit_call_fixes(&index, &state.typed).is_empty());
    }
}

#[test]
fn function_reference_fixes_preserve_aliases_and_callback_values() {
    let source = "fn ready -> Bool = true\ntype Callback = fn () -> Bool\nconst callback: Callback = ready\nfn consume (b: Bool) -> Int = if b then 1 else 0\npub fn choose () -> Int =\n    let alias = callback\n    if alias then 7 else 0\npub fn argument () -> Int = consume callback\npub fn reference () -> fn () -> Bool = callback\n";
    for ending in ["\n", "\r\n"] {
        let source = source.replace('\n', ending);
        let dir = workspace();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let file = root.join("app/src/Main.ridge");
        std::fs::write(&file, &source).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        assert_eq!(state.type_errors.len(), 2, "{:?}", state.type_errors);
        for (_, error) in &state.type_errors {
            assert!(error
                .render(&state.typed.tycons)
                .contains("a bare function name is a function value"));
        }
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let fixes = ridge_lsp::explicit_calls::collect_function_call_fixes(
            &index,
            &state.typed,
            &state.resolved,
            &state.type_errors,
        );
        assert_eq!(fixes.len(), 2, "{fixes:?}");
        let lines = ridge_lexer::LineIndex::new(&source);
        let mut edited = source.clone();
        let mut edits: Vec<_> = fixes
            .iter()
            .map(|fix| {
                (
                    lines.utf16_to_byte(fix.edit_range.start.line, fix.edit_range.start.character)
                        as usize,
                    lines.utf16_to_byte(fix.edit_range.end.line, fix.edit_range.end.character)
                        as usize,
                    &fix.new_text,
                )
            })
            .collect();
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.0));
        for (start, end, text) in edits {
            edited.replace_range(start..end, text);
        }
        assert!(edited.contains("if (alias ()) then 7 else 0"));
        assert!(edited.contains("consume (callback ())"));
        assert!(edited.contains("-> fn () -> Bool = callback"));
        state.recompile(state.typed.modules[0].id, &edited);
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    }
}

#[test]
fn explicit_message_fixes_apply_to_original_buffers_and_follow_signature_edits() {
    for ending in ["\n", "\r\n"] {
        let dir = workspace();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let file = root.join("app/src/Main.ridge");
        let source = "-- café 🧭\nimport std.actor as A\nactor Counter =\n    on ping = ()\n    on take (_: Unit) = ()\npub fn time exercise (h: Handle Counter) -> Unit =\n    h ! ping -- keep\n    let _ = h ?> ping\n    let _ = A.send h ping\n    let _ = A.tryAsk h ping 1000\n    h ! take ()\n".replace('\n', ending);
        std::fs::write(&file, &source).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state.typed.modules[0].id;
        assert!(
            state
                .type_errors
                .iter()
                .all(|(_, error)| error.code() == "T063"),
            "{:?}",
            state.type_errors
        );
        assert_eq!(state.type_errors.len(), 4);
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let fixes = ridge_lsp::explicit_calls::collect_explicit_call_fixes(&index, &state.typed);
        assert_eq!(
            fixes
                .iter()
                .map(|fix| fix.new_text.as_str())
                .collect::<Vec<_>>(),
            ["ping ()", "ping ()", "(ping ())", "(ping ())"]
        );
        let lines = ridge_lexer::LineIndex::new(&source);
        let mut edits: Vec<_> = fixes
            .iter()
            .map(|fix| {
                assert_eq!(fix.code, "T063");
                assert_eq!(fix.decl_range, fix.edit_range);
                (
                    lines.utf16_to_byte(fix.edit_range.start.line, fix.edit_range.start.character)
                        as usize,
                    lines.utf16_to_byte(fix.edit_range.end.line, fix.edit_range.end.character)
                        as usize,
                    fix.new_text.as_str(),
                )
            })
            .collect();
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.0));
        let mut edited = source.clone();
        for (start, end, text) in edits {
            assert_eq!(&edited[start..end], "ping");
            edited.replace_range(start..end, text);
        }
        assert!(edited.contains(&format!("ping () -- keep{ending}")));
        assert!(edited.contains("h ! take ()"));
        state.recompile(module, &edited);
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        assert!(state.resolved.parse_errors.is_empty());
        assert!(state.resolved.errors.is_empty());
        let index = WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
        assert!(
            ridge_lsp::explicit_calls::collect_explicit_call_fixes(&index, &state.typed).is_empty()
        );
        state.recompile(module, &source.replace("on ping =", "on ping (_: Unit) ="));
        assert!(state
            .type_errors
            .iter()
            .any(|(_, error)| error.code() == "T003"));
        let index = WorkspaceIndex::build(2, &state.typed, &state.resolved, &state.source_cache());
        assert!(
            ridge_lsp::explicit_calls::collect_explicit_call_fixes(&index, &state.typed).is_empty()
        );
    }
}

#[test]
fn actor_unit_payload_types_and_marker_facts_follow_edits() {
    for ending in ["\n", "\r\n"] {
        let dir = workspace();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let file = root.join("app/src/Main.ridge");
        let uri = Url::from_file_path(&file).unwrap();
        let valid = "-- café 🧭\nimport std.actor (AskError)\nactor Counter =\n    on value (payload: Unit) -> Int = 9\npub fn time fetch (h: Handle Counter) -> Result Int AskError = h ?> value ()\n".replace('\n', ending);
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
        let nullary = valid.replace("(payload: Unit)", "()");
        for (source, code, marker) in [
            (valid.clone(), None, false),
            (nullary, None, true),
            (valid.clone(), None, false),
            (
                valid.replace("h ?> value ()", "h ?> value"),
                Some("T003"),
                false,
            ),
            (
                valid.replace("h ?> value ()", "h ?> value 7"),
                Some("T001"),
                false,
            ),
            (
                valid.replace("h ?> value ()", "h ?> absent ()"),
                Some("T015"),
                false,
            ),
            (
                valid.replace("fn time fetch", "fn fetch"),
                Some("T014"),
                false,
            ),
            (valid.clone(), None, false),
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
                assert_eq!(
                    !state.typed.modules[module.0 as usize]
                        .nullary_message_sites
                        .is_empty(),
                    marker
                );
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let at = position(&source, source.find("fetch").unwrap());
                let (hover, _) = index.hover_at(&uri, at.line, at.character).unwrap();
                assert!(hover.contains("Int") && hover.contains("Result"), "{hover}");
                if !marker {
                    let at = position(&source, source.find("payload").unwrap());
                    let (hover, _) = index.hover_at(&uri, at.line, at.character).unwrap();
                    assert!(hover.contains("Unit"), "{hover}");
                }
            }
        }
    }
}
