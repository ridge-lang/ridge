//! Editor-diagnostics guarantee for the "did you mean" syntax guidance.
//!
//! `ridge-lsp` seeds its state through `check_standalone_incremental` and turns
//! the resulting parse errors into LSP messages verbatim. So the promise that an
//! editor shows the helpful `P033`/`P034`/`P035` guidance — instead of a bare
//! "unexpected token" — for `let … in`, an `if` match guard, or `{ record with
//! … }` reduces to one fact: this pipeline emits those codes for those forms.
//! This test pins it so the LSP counterpart cannot silently regress, and pins
//! the negative direction too: the canonical Ridge spellings emit none of them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;

use ridge_driver::check_standalone_incremental;
use tempfile::tempdir;

#[test]
fn explicit_call_migration_preserves_raw_text_and_rechecks_the_program() {
    let source = "-- café 🧭\nimport std.actor as A\nactor Counter =\n    on ping = ()\n    on take (_: Unit) = ()\npub fn time exercise (h: Handle Counter) -> Unit =\n    h ! ping -- keep this comment\n    h ! take ()\n    let _ = A.send h (ping)\n    let _ = A.tryAsk h ping 1000\n";
    let expected = "-- café 🧭\nimport std.actor as A\nactor Counter =\n    on ping = ()\n    on take (_: Unit) = ()\npub fn time exercise (h: Handle Counter) -> Unit =\n    h ! ping () -- keep this comment\n    h ! take ()\n    let _ = A.send h ((ping ()))\n    let _ = A.tryAsk h (ping ()) 1000\n";
    for ending in ["\n", "\r\n", "\r"] {
        let source = source.replace('\n', ending);
        let expected = expected.replace('\n', ending);
        let dir = tempdir().unwrap();
        let file = dir.path().join("migration.ridge");
        fs::write(&file, &source).unwrap();
        let mut state = check_standalone_incremental(std::slice::from_ref(&file));
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
            state
                .type_errors
                .iter()
                .all(|(_, error)| error.code() == "T063"),
            "{:?}",
            state.type_errors
        );
        assert_eq!(state.type_errors.len(), 3);
        let output = ridge_driver::migrate_message_calls(&source, &state.typed.modules[0]).unwrap();
        assert_eq!(output, expected);
        let changed_signature = source.replace("on ping =", "on ping (_: Unit) =");
        assert!(
            ridge_driver::migrate_message_calls(&changed_signature, &state.typed.modules[0])
                .is_err()
        );
        let duplicate = state.typed.modules[0].message_call_sites[0].clone();
        state.typed.modules[0].message_call_sites.push(duplicate);
        assert!(ridge_driver::migrate_message_calls(&source, &state.typed.modules[0]).is_err());
        fs::write(&file, &output).unwrap();
        let migrated = check_standalone_incremental(&[file]);
        assert!(
            migrated.type_errors.is_empty(),
            "{:?}",
            migrated.type_errors
        );
        assert!(migrated.resolved.parse_errors.is_empty());
        assert!(migrated.resolved.errors.is_empty());
        assert_eq!(
            ridge_driver::migrate_message_calls(&output, &migrated.typed.modules[0]).unwrap(),
            output
        );
    }
}

#[test]
fn explicit_call_migration_rejects_an_unresolved_receiver() {
    let source = "pub fn unresolved h -> Unit = h ! ping\n";
    let dir = tempdir().unwrap();
    let file = dir.path().join("unknown.ridge");
    fs::write(&file, source).unwrap();
    let state = check_standalone_incremental(&[file]);
    assert!(
        state.resolved.parse_errors.is_empty(),
        "{:?}",
        state.resolved.parse_errors
    );
    assert!(state.typed.modules[0]
        .message_call_sites
        .iter()
        .any(|site| site.parameter_count.is_none()));
    assert_eq!(state.type_errors.len(), 1, "{:?}", state.type_errors);
    assert!(matches!(
        &state.type_errors[0].1,
        ridge_typecheck::TypeError::MissingInvocationMarker {
            needs_receiver_annotation: true,
            ..
        }
    ));
    assert!(ridge_driver::migrate_message_calls(source, &state.typed.modules[0]).is_err());
}

/// The parse-error codes the driver pipeline produces for `src`.
fn parse_codes(src: &str) -> Vec<String> {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("guidance.ridge");
    fs::write(&path, src).expect("write source");

    let state = check_standalone_incremental(&[path]);
    state
        .resolved
        .parse_errors
        .iter()
        .map(|(_, e)| e.code().to_owned())
        .collect()
}

#[test]
fn let_in_surfaces_p033() {
    let src = "\
fn f (x: Int) -> Int =
    let y = x + 1 in y * 2
";
    assert!(
        parse_codes(src).iter().any(|c| c == "P033"),
        "the editor should show the let-layout guidance (P033)"
    );
}

#[test]
fn if_match_guard_surfaces_p034() {
    let src = "\
fn f (x: Int) -> Int =
    match x
        n if n > 0 -> 1
        _ -> 0
";
    assert!(
        parse_codes(src).iter().any(|c| c == "P034"),
        "the editor should show the `when` guard guidance (P034)"
    );
}

#[test]
fn record_update_braces_surfaces_p035() {
    let src = "\
type Point = { x: Int, y: Int }

fn move0 (p: Point) -> Point =
    { p with x = 0 }
";
    assert!(
        parse_codes(src).iter().any(|c| c == "P035"),
        "the editor should show the record-update guidance (P035)"
    );
}

#[test]
fn canonical_spellings_emit_no_guidance_codes() {
    let src = "\
type Point = { x: Int, y: Int }

fn f (x: Int) -> Int =
    let y = x + 1
    match y
        n when n > 0 -> 1
        _ -> 0

fn move0 (p: Point) -> Point =
    p with { x = 0 }
";
    let codes = parse_codes(src);
    assert!(
        !codes
            .iter()
            .any(|c| c == "P033" || c == "P034" || c == "P035"),
        "canonical Ridge syntax must not trip the guidance diagnostics, got: {codes:?}"
    );
}
