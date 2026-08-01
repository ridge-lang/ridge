//! Anti-regression pin for the `Error` wire shape in the BEAM runtime.
//!
//! `Error` is the builtin record `{ code: Text, message: Text }`, which codegen
//! lowers to an atom-keyed map — field access `e.code` compiles to
//! `maps:get(code, _)`. Until the 2026-08 unification, several `ridge_rt.erl`
//! producers (`fs_*`, `read_line`, `proc_run`, `decimal`, `uuid`, `bytes`,
//! `date`, `time`, `crypto.base64`, `json_decode`, …) returned the error
//! payload as a tagged tuple `{error_record, Code, Message}` instead, so any
//! caller that read `e.code` or `e.message` crashed with `badmap` at runtime
//! (same family as the `http_listen`/`http_get` fixes before it). All
//! producers now route through `mk_error/2`.
//!
//! This test pins the invariant at the source level: no producer site may
//! reintroduce `{error, {error_record, …}`. Historical comments that mention
//! the old shape do not match the pinned pattern.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

#[test]
fn runtime_has_no_error_record_tuple_producers() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/ridge_rt.erl");
    let src = std::fs::read_to_string(&runtime)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", runtime.display()));
    assert!(
        !src.contains("{error, {error_record,"),
        "ridge_rt.erl must not produce the old `{{error, {{error_record, …}}}}` \
         tuple shape — route the error through mk_error/2 (map shape) instead"
    );
}

#[test]
fn runtime_routes_error_producers_through_mk_error() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/ridge_rt.erl");
    let src = std::fs::read_to_string(&runtime)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", runtime.display()));
    // The fs/proc/io error channels were the tuple-shape holdouts; they must
    // call mk_error/2 now.
    for needle in [
        "fs_error(R) ->",
        "mk_error(<<\"spawn_error\">>",
        "mk_error(<<\"eof\">>",
        "mk_error(<<\"decode_error\">>",
    ] {
        assert!(
            src.contains(needle),
            "expected `{needle}` in ridge_rt.erl — the Error wire-shape \
             unification routes every producer through mk_error/2"
        );
    }
}
