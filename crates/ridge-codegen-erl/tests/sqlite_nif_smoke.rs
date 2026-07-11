//! End-to-end test for the SQLite bridge through the real install path.
//!
//! Gated on `--features beam-runtime`. Under that feature `build.rs` has already
//! compiled the NIF from the vendored amalgamation and baked it into the crate,
//! so this test drives the production runtime installer: `install_runtime` +
//! `compile_runtime` write the baked NIF beside `ridge_sqlite.beam`, and the
//! module's `-on_load` finds it there — the same path a real `ridge run` takes.
//! It then exercises the whole bridge on a live BEAM: the raw native surface,
//! and the `ridge_sqlite` glue (`SqlValue` mapping, transactions, migrations,
//! and error classification). Equality is judged inside the BEAM so only
//! compact tokens cross back to the Rust assertions.

#![cfg(feature = "beam-runtime")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

use ridge_codegen_erl::escript::package_escript_from_beam_dir;
use ridge_codegen_erl::runtime::{compile_runtime, install_runtime};
use std::path::Path;
use std::process::Command;

const TMPDIR: &str = env!("CARGO_TARGET_TMPDIR");

/// The smoke module. Kept in Erlang so equality is judged by the BEAM and only
/// compact tokens cross back. It covers both levels of the bridge: the raw NIF
/// surface, and the glue (SqlValue mapping, transactions, migrations, errors).
const SMOKE_ERL: &str = r#"-module(sqlite_smoke).
-export([run/0]).

run() ->
    nif_level(),
    glue_level(),
    halt(0).

%% The raw native surface: mixed-type round-trip, typed error, closed guard.
nif_level() ->
    {ok, Conn} = ridge_sqlite:nif_open(<<":memory:">>),
    {ok, _} = ridge_sqlite:nif_exec(Conn,
        <<"CREATE TABLE t (id INTEGER, name TEXT, score REAL, data BLOB, note TEXT)">>, []),
    {ok, 1} = ridge_sqlite:nif_exec(Conn,
        <<"INSERT INTO t VALUES (?,?,?,?,?)">>,
        [{int, 1}, {text, <<"ada">>}, {float, 9.5}, {blob, <<1, 2, 3>>}, null]),
    {ok, Cols, Rows} = ridge_sqlite:nif_query(Conn,
        <<"SELECT id,name,score,data,note FROM t">>, []),
    Ver = ridge_sqlite:nif_libversion(),
    {error, {sqlite_error, ErrCode, _}} =
        ridge_sqlite:nif_exec(Conn, <<"SELECT * FROM nope">>, []),
    ok = ridge_sqlite:nif_close(Conn),
    AfterClose = ridge_sqlite:nif_exec(Conn, <<"SELECT 1">>, []),
    ExpCols = [<<"id">>, <<"name">>, <<"score">>, <<"data">>, <<"note">>],
    ExpRows = [[{int, 1}, {text, <<"ada">>}, {float, 9.5}, {blob, <<1, 2, 3>>}, null]],
    io:format("nif_ver=~ts~n", [Ver]),
    io:format("nif_cols=~p~n", [Cols =:= ExpCols]),
    io:format("nif_rows=~p~n", [Rows =:= ExpRows]),
    io:format("nif_errcode=~p~n", [ErrCode]),
    io:format("nif_after_close=~p~n", [AfterClose]).

%% The adapter glue: rich SqlValue mapping, raw decode contract, unique-error
%% classification, nested transactions, all/get_rows, migrations, close.
glue_level() ->
    {ok, #{id := Id}} = ridge_sqlite:sqlite_connect(<<":memory:">>, 5000, <<>>, 1),
    {ok, _} = ridge_sqlite:sqlite_raw_exec(Id,
        <<"CREATE TABLE u (id INTEGER PRIMARY KEY, email TEXT UNIQUE, active INTEGER, ts TEXT, amt TEXT)">>, []),
    %% rich params: bool -> int 0/1, instant -> ISO text, decimal -> exact text
    {ok, 1} = ridge_sqlite:sqlite_raw_exec(Id,
        <<"INSERT INTO u (id,email,active,ts,amt) VALUES (?,?,?,?,?)">>,
        [{'SqlInt', 1}, {'SqlText', <<"a@x">>}, {'SqlBool', true},
         {'SqlInstant', 0}, {'SqlDecimal', <<"9.99">>}]),
    {ok, [Row]} = ridge_sqlite:sqlite_raw_query(Id,
        <<"SELECT id,email,active,ts,amt FROM u WHERE id = ?">>, [{'SqlInt', 1}]),
    io:format("glue_active=~p~n", [maps:get(<<"active">>, Row)]),
    io:format("glue_ts=~p~n", [maps:get(<<"ts">>, Row)]),
    io:format("glue_amt=~p~n", [maps:get(<<"amt">>, Row)]),
    %% a duplicate email is a unique violation, mapped to the shared SQLSTATE
    UErr = ridge_sqlite:sqlite_raw_exec(Id,
        <<"INSERT INTO u (id,email) VALUES (2,'a@x')">>, []),
    io:format("glue_unique=~p~n", [err_code(UErr)]),
    %% nested transaction: roll back the savepoint, commit the outer
    {ok, ok} = ridge_sqlite:sqlite_begin(Id),
    {ok, 1} = ridge_sqlite:sqlite_raw_exec(Id, <<"INSERT INTO u (id,email) VALUES (3,'c@x')">>, []),
    {ok, ok} = ridge_sqlite:sqlite_begin(Id),
    {ok, 1} = ridge_sqlite:sqlite_raw_exec(Id, <<"INSERT INTO u (id,email) VALUES (4,'d@x')">>, []),
    {ok, ok} = ridge_sqlite:sqlite_rollback(Id),
    {ok, ok} = ridge_sqlite:sqlite_commit(Id),
    {ok, AllRows} = ridge_sqlite:sqlite_all(Id, <<"u">>),
    Ids = lists:sort([N || R <- AllRows, begin {'SqlInt', N} = maps:get(<<"id">>, R), true end]),
    io:format("glue_ids=~p~n", [Ids]),
    {ok, GRows} = ridge_sqlite:sqlite_get_rows(Id, <<"u">>, <<"id">>, {'SqlInt', 3}),
    io:format("glue_get=~p~n", [length(GRows)]),
    %% bytes round-trip through the hex codec, and an array encoded to JSON text
    {ok, _} = ridge_sqlite:sqlite_raw_exec(Id, <<"CREATE TABLE b (raw BLOB, arr TEXT)">>, []),
    {ok, 1} = ridge_sqlite:sqlite_raw_exec(Id, <<"INSERT INTO b VALUES (?,?)">>,
        [{'SqlBytes', <<"deadbeef">>},
         {'SqlArray', [{'SqlInt', 1}, {'SqlInt', 2}, {'SqlText', <<"x">>}]}]),
    {ok, [BRow]} = ridge_sqlite:sqlite_raw_query(Id, <<"SELECT raw,arr FROM b">>, []),
    io:format("glue_bytes=~p~n", [maps:get(<<"raw">>, BRow)]),
    io:format("glue_array=~p~n", [maps:get(<<"arr">>, BRow)]),
    %% migrations bookkeeping
    {ok, []} = ridge_sqlite:sqlite_migrations_applied(Id),
    {ok, ok} = ridge_sqlite:sqlite_record_migration(Id, <<"0001_init">>),
    {ok, ok} = ridge_sqlite:sqlite_record_migration(Id, <<"0002_more">>),
    {ok, Applied} = ridge_sqlite:sqlite_migrations_applied(Id),
    io:format("glue_migs=~p~n", [Applied]),
    {ok, ok} = ridge_sqlite:sqlite_unrecord_migration(Id, <<"0002_more">>),
    {ok, Applied2} = ridge_sqlite:sqlite_migrations_applied(Id),
    io:format("glue_migs2=~p~n", [Applied2]),
    {ok, ok} = ridge_sqlite:sqlite_close(Id),
    io:format("glue_closed=~p~n", [err_code(ridge_sqlite:sqlite_raw_exec(Id, <<"SELECT 1">>, []))]).

err_code({error, #{code := C}}) -> C;
err_code(Other) -> Other.
"#;

/// A tiny program that opens SQLite and reads a value back, used to prove a
/// packaged escript carries and loads the native object with nothing beside it.
const ESCRIPT_MAIN_ERL: &str = r#"-module(escript_main).
-export([main/1]).

main(_) ->
    {ok, #{id := Id}} = ridge_sqlite:sqlite_connect(<<":memory:">>, 0, <<>>, 1),
    {ok, _} = ridge_sqlite:sqlite_raw_exec(Id, <<"CREATE TABLE t (n INTEGER)">>, []),
    {ok, 1} = ridge_sqlite:sqlite_raw_exec(Id, <<"INSERT INTO t VALUES (?)">>, [{'SqlInt', 42}]),
    {ok, [Row]} = ridge_sqlite:sqlite_raw_query(Id, <<"SELECT n FROM t">>, []),
    io:format("escript_val=~p~n", [maps:get(<<"n">>, Row)]),
    ok = ridge_sqlite:sqlite_close(Id).
"#;

/// `erlc <src>` into `out_dir`.
fn erlc(erlc_path: &Path, src: &Path, out_dir: &Path) {
    let status = Command::new(erlc_path)
        .arg("-o")
        .arg(out_dir)
        .arg(src)
        .status()
        .expect("run erlc");
    assert!(status.success(), "erlc failed for {}", src.display());
}

#[test]
fn sqlite_bridge_end_to_end() {
    let erl = which::which("erl")
        .expect("erl not found on PATH — install OTP or drop --features beam-runtime");
    let erlc_path = which::which("erlc").expect("erlc not found on PATH");

    // A fresh output root so the runtime is installed and compiled from scratch,
    // exactly as a first build would. `compile_runtime` writes the baked NIF
    // beside `ridge_sqlite.beam` under the `beam-runtime` feature.
    let out_root = Path::new(TMPDIR).join("sqlite_bridge_e2e");
    let _ = std::fs::remove_dir_all(&out_root);
    let beam_dir = out_root.join("beam");
    // The driver creates the beam dir during codegen before installing the
    // runtime; calling the installer in isolation, the test does it here.
    std::fs::create_dir_all(&beam_dir).expect("create beam dir");
    install_runtime(&out_root).expect("install the runtime sources");
    compile_runtime(&erlc_path, &out_root).expect("compile the runtime and write the NIF");

    assert!(
        beam_dir.join("ridge_sqlite.beam").exists(),
        "ridge_sqlite.beam was not compiled"
    );

    // Compile the smoke driver into the same beam dir.
    let smoke_src = out_root.join("sqlite_smoke.erl");
    std::fs::write(&smoke_src, SMOKE_ERL).expect("write smoke module");
    erlc(&erlc_path, &smoke_src, &beam_dir);

    // No RIDGE_SQLITE_NIF override: the module's -on_load resolves the object
    // beside its own beam, which is where compile_runtime placed it.
    let out = Command::new(&erl)
        .args([
            "-noinput",
            "-pa",
            beam_dir.to_str().expect("beam dir path"),
            "-s",
            "sqlite_smoke",
            "run",
            "-s",
            "init",
            "stop",
        ])
        .output()
        .expect("run the smoke module on the BEAM");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("stdout:\n{stdout}\nstderr:\n{stderr}");

    // Raw native surface.
    assert!(
        stdout.contains("nif_ver=3.45.3"),
        "version mismatch: {combined}"
    );
    assert!(
        stdout.contains("nif_cols=true"),
        "columns wrong: {combined}"
    );
    assert!(stdout.contains("nif_rows=true"), "rows wrong: {combined}");
    assert!(
        stdout.contains("nif_errcode=1"),
        "error code wrong: {combined}"
    );
    assert!(
        stdout.contains("nif_after_close={error,closed}"),
        "closed handle not rejected: {combined}"
    );

    // Adapter glue: rich params store correctly and read back raw by storage.
    assert!(
        stdout.contains("glue_active={'SqlInt',1}"),
        "bool did not store as integer 1: {combined}"
    );
    assert!(
        stdout.contains("glue_ts={'SqlText',<<\"1970-01-01T00:00:00"),
        "instant did not store as ISO text: {combined}"
    );
    assert!(
        stdout.contains("glue_amt={'SqlText',<<\"9.99\">>}"),
        "decimal did not store as exact text: {combined}"
    );
    // A unique violation maps to the shared SQLSTATE, so it reads like Postgres.
    assert!(
        stdout.contains("glue_unique=<<\"db.error.23505\">>"),
        "unique violation not classified: {combined}"
    );
    // Savepoint rolled back id 4, outer commit kept id 3; id 2 failed the unique.
    assert!(
        stdout.contains("glue_ids=[1,3]"),
        "transaction wrong: {combined}"
    );
    assert!(stdout.contains("glue_get=1"), "get_rows wrong: {combined}");
    assert!(
        stdout.contains("glue_bytes={'SqlBytes',<<\"deadbeef\">>}"),
        "bytes did not round-trip through hex: {combined}"
    );
    assert!(
        stdout.contains("glue_array={'SqlText',<<\"[1,2,"),
        "array did not encode to JSON text: {combined}"
    );
    assert!(
        stdout.contains("glue_migs=[<<\"0001_init\">>,<<\"0002_more\">>]"),
        "migrations not recorded in order: {combined}"
    );
    assert!(
        stdout.contains("glue_migs2=[<<\"0001_init\">>]"),
        "migration not unrecorded: {combined}"
    );
    assert!(
        stdout.contains("glue_closed=<<\"db.conn.closed\">>"),
        "closed glue handle not rejected: {combined}"
    );
}

#[test]
fn sqlite_escript_self_contained() {
    let erlc_path = which::which("erlc").expect("erlc not found on PATH");
    let Ok(escript_exe) = which::which("escript") else {
        eprintln!("SKIP sqlite_escript_self_contained: escript not on PATH");
        return;
    };

    let out_root = Path::new(TMPDIR).join("sqlite_escript");
    let _ = std::fs::remove_dir_all(&out_root);
    let beam_dir = out_root.join("beam");
    std::fs::create_dir_all(&beam_dir).expect("create beam dir");
    install_runtime(&out_root).expect("install the runtime sources");
    compile_runtime(&erlc_path, &out_root).expect("compile the runtime and write the NIF");

    // The program that uses SQLite, compiled into the beam dir.
    let entry_src = out_root.join("escript_main.erl");
    std::fs::write(&entry_src, ESCRIPT_MAIN_ERL).expect("write entry module");
    erlc(&erlc_path, &entry_src, &beam_dir);

    // Package the escript. Entry name equals the module so no shim is generated
    // and escript dispatches straight to `escript_main:main/1`. Under
    // `beam-runtime` this embeds the native object as an archive entry, so the
    // escript is a single self-contained file.
    let bytes = package_escript_from_beam_dir(&beam_dir, "escript_main", "escript_main")
        .expect("package the escript");
    let script = out_root.join("escript_main.escript");
    std::fs::write(&script, &bytes).expect("write the escript");

    // Run it from a clean directory with no loose object beside it and no
    // override set, so the module has to extract the embedded object to load it.
    let run_dir = out_root.join("run");
    std::fs::create_dir_all(&run_dir).expect("create run dir");
    let out = Command::new(&escript_exe)
        .arg(&script)
        .current_dir(&run_dir)
        .env_remove("RIDGE_SQLITE_NIF")
        .output()
        .expect("run the escript");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("escript_val={'SqlInt',42}"),
        "the self-contained escript did not run SQLite:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
