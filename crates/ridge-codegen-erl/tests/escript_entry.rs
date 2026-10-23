//! Reject a module without an entry before invoking the external toolchain.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ridge_codegen_erl::core_ast::{CErlAtom, CErlModule};
use ridge_codegen_erl::escript::{emit_escript, EmitError};

#[test]
fn escript_requires_an_exported_main() {
    let library = CErlModule {
        name: CErlAtom("library".to_owned()),
        exports: vec![],
        attributes: vec![],
        fns: vec![],
    };
    assert!(matches!(
        emit_escript(&[library], "library"),
        Err(EmitError::EscriptNeedsEntry { member }) if member == "library"
    ));
}

#[cfg(feature = "beam-runtime")]
#[test]
fn beam_packaging_checks_missing_valid_and_invalid_entry_exports() {
    use ridge_codegen_erl::escript::package_escript_from_beam_dir;
    let directory = tempfile::tempdir().unwrap();
    let erlc = ridge_codegen_erl::erlc::probe(None).unwrap();
    for (name, arity) in [
        ("library", None),
        ("zero", Some(0)),
        ("one", Some(1)),
        ("two", Some(2)),
    ] {
        let args = match arity {
            None | Some(0) => "",
            Some(1) => "_A",
            _ => "_A, _B",
        };
        let function = if arity.is_some() { "main" } else { "value" };
        let on_load = if name == "zero" {
            "-on_load(init/0).\ninit() -> erlang:halt(77).\n"
        } else {
            ""
        };
        let source = format!(
            "-module({name}).\n-export([{function}/{}]).\n{on_load}{function}({args}) -> ok.\n",
            arity.unwrap_or(0)
        );
        let path = directory.path().join(format!("{name}.erl"));
        std::fs::write(&path, source).unwrap();
        assert!(std::process::Command::new(&erlc.path)
            .arg("-o")
            .arg(directory.path())
            .arg(path)
            .status()
            .unwrap()
            .success());
        let result = package_escript_from_beam_dir(directory.path(), name, name);
        match arity {
            None => assert!(matches!(result, Err(EmitError::EscriptNeedsEntry { .. }))),
            Some(2) => assert!(matches!(
                result,
                Err(EmitError::EscriptMainArityInvalid { arity: 2, .. })
            )),
            _ => assert!(result.unwrap().starts_with(b"#!/usr/bin/env escript\n")),
        }
    }
}
