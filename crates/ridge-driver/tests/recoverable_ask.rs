//! The ask's Result contract is checked independently of runtime execution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const ACTOR: &str = "actor Counter =\n    on get () -> Int = 7\n\n";

#[test]
fn a_bare_reply_annotation_is_rejected_with_migration_advice() {
    let source = format!("{ACTOR}pub fn time read (h: Handle Counter) -> Int = h ?> get\n");
    let ws = common::make_workspace("Ask", &source);
    let artifacts =
        compile_workspace(CompileOptions::new(ws.path).with_emit(EmitArtefacts::Core)).unwrap();
    let mismatch = artifacts
        .diagnostics
        .iter()
        .find(|d| d.code == "T001")
        .unwrap();
    assert!(
        mismatch.primary_message.contains("Result Int AskError"),
        "{mismatch:?}"
    );
    assert!(
        mismatch.primary_message.contains("match `Ok`/`Err`"),
        "{mismatch:?}"
    );
    assert!(artifacts.core_files.is_empty());
}

#[test]
fn ask_error_identity_does_not_require_an_import_or_follow_a_local_namesake() {
    let source = format!("{ACTOR}type AskError = LocalErrorA | LocalErrorB\npub fn time read (h: Handle Counter) = h ?> get\npub fn time label (h: Handle Counter) -> Text = match h ?> get\n    Ok _ -> \"ok\"\n    Err error -> toText error\n");
    let ws = common::make_workspace("Ask", &source);
    let state = ridge_driver::check_workspace_incremental(
        ridge_driver::CheckOptions::new(ws.path.clone()).with_retain_indices(true),
    )
    .unwrap();
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(
        state.resolved.errors.is_empty(),
        "{:?}",
        state.resolved.errors
    );
    let module = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Ask.ridge"))
        .unwrap()
        .id;
    let scheme = state.typed.module_schemes[module.0 as usize]
        .get("read")
        .unwrap();
    let ridge_ir::Type::Fn { ret, .. } = &scheme.ty else {
        panic!("not a function")
    };
    let ridge_ir::Type::Con(result, args) = ret.as_ref() else {
        panic!("not a Result")
    };
    assert_eq!(*result, state.typed.builtins.result);
    let ridge_ir::Type::Con(error, _) = &args[1] else {
        panic!("not a nominal error")
    };
    assert_eq!(Some(error), state.typed.stdlib_tycons.get("AskError"));
    assert_ne!(
        Some(error),
        state.typed.module_tycon_names[module.0 as usize].get("AskError")
    );
    let artifacts =
        compile_workspace(CompileOptions::new(ws.path).with_emit(EmitArtefacts::Core)).unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let core = artifacts
        .core_files
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(core.contains("'actor_error_to_text'"), "{core}");
}

#[test]
fn ask_propagation_emits_the_recoverable_operation() {
    let source = format!("import std.actor (AskError)\n{ACTOR}pub fn time read (h: Handle Counter) -> Result Int AskError =\n    let value = h ?> get timeout never ?\n    Ok (value + 1)\n");
    let ws = common::make_workspace("Ask", &source);
    let artifacts =
        compile_workspace(CompileOptions::new(ws.path).with_emit(EmitArtefacts::Core)).unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let core = artifacts
        .core_files
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(core.contains("'try_ask'"), "{core}");
    assert!(!core.contains(":'ask'"), "{core}");
}
