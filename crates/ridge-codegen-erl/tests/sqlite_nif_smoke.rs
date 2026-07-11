//! Smoke test for the SQLite native bridge (`runtime/native/sqlite_nif.c` +
//! `runtime/ridge_sqlite.erl`).
//!
//! Gated on `--features beam-runtime`. It compiles the NIF from the vendored
//! amalgamation, loads it on a live BEAM, and exercises the whole native
//! surface end to end: open an in-memory database, run mixed-type inserts,
//! read the rows back, check a failing statement's error, and confirm a closed
//! handle is rejected. The row equality is asserted inside the BEAM so the
//! Rust side only has to look for `rows_match=true`, immune to how `~p` wraps.
//!
//! Compiling the amalgamation needs a C compiler (MSVC on Windows, cc on
//! Unix). Both the beam-e2e CI job and a normal dev box have one; if none is
//! found the test skips loudly rather than failing, so an environment without
//! a C toolchain is not blocked.

#![cfg(feature = "beam-runtime")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::path::{Path, PathBuf};
use std::process::Command;

const NATIVE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/runtime/native");
const RIDGE_SQLITE_ERL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/runtime/ridge_sqlite.erl");
const TMPDIR: &str = env!("CARGO_TARGET_TMPDIR");

/// The smoke module. Kept in Erlang so equality is judged by the BEAM and only
/// compact tokens cross back to the Rust assertions. It covers both levels of
/// the bridge: the raw NIF surface, and the ridge_sqlite glue (SqlValue
/// mapping, transactions, migrations, and error classification).
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

/// The shared-object base path (no extension) that `erlang:load_nif` expects.
fn nif_base() -> PathBuf {
    Path::new(TMPDIR).join("ridge_sqlite")
}

fn shared_object() -> PathBuf {
    if cfg!(windows) {
        nif_base().with_extension("dll")
    } else {
        nif_base().with_extension("so")
    }
}

/// `<otp-root>/erts-<ver>/include`, home of `erl_nif.h`.
fn erts_include_dir(erl: &Path) -> PathBuf {
    let out = Command::new(erl)
        .args([
            "-noshell",
            "-eval",
            "io:format(\"~ts\",[filename:join([code:root_dir(),\"erts-\"++erlang:system_info(version),\"include\"])])",
            "-s",
            "init",
            "stop",
        ])
        .output()
        .expect("run erl to resolve the erts include dir");
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    PathBuf::from(dir)
}

/// True when the compiled object is present and newer than our own C source, so
/// a repeat run reuses it and only an edit to `sqlite_nif.c` forces a rebuild.
/// The vendored `sqlite3.c` is pinned and never changes, so it is not consulted.
fn up_to_date() -> bool {
    let so = shared_object();
    let src = Path::new(NATIVE_DIR).join("sqlite_nif.c");
    match (std::fs::metadata(&so), std::fs::metadata(&src)) {
        (Ok(a), Ok(b)) => match (a.modified(), b.modified()) {
            (Ok(am), Ok(bm)) => am >= bm,
            _ => false,
        },
        _ => false,
    }
}

/// Locate `vcvarsall.bat` through `vswhere`, mirroring how the `cc` crate finds
/// MSVC. Returns None when no suitable Visual Studio install is present.
#[cfg(windows)]
fn find_vcvarsall() -> Option<PathBuf> {
    let pf86 = std::env::var("ProgramFiles(x86)")
        .unwrap_or_else(|_| "C:\\Program Files (x86)".to_string());
    let vswhere = PathBuf::from(pf86)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    if !vswhere.exists() {
        return None;
    }
    let out = Command::new(&vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .ok()?;
    let install = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if install.is_empty() {
        return None;
    }
    let vcvars = PathBuf::from(install)
        .join("VC")
        .join("Auxiliary")
        .join("Build")
        .join("vcvarsall.bat");
    vcvars.exists().then_some(vcvars)
}

/// Compile the NIF with MSVC via a generated batch file that enters the x64
/// developer environment. Returns false (with a printed reason) when the
/// toolchain is unavailable.
#[cfg(windows)]
fn compile_nif(include: &Path) -> bool {
    let Some(vcvars) = find_vcvarsall() else {
        eprintln!("SKIP sqlite_nif_smoke: no MSVC toolchain found via vswhere");
        return false;
    };
    let nif_c = Path::new(NATIVE_DIR).join("sqlite_nif.c");
    let sqlite_c = Path::new(NATIVE_DIR).join("sqlite3.c");
    let bat = Path::new(TMPDIR).join("build_ridge_sqlite.bat");
    let script = format!(
        "@echo off\r\n\
         call \"{vcvars}\" x64 >nul\r\n\
         cl /nologo /LD /O2 /std:c11 /DSQLITE_THREADSAFE=1 /I\"{inc}\" \
         \"{nif}\" \"{sqlite}\" /Fe:\"{out}\"\r\n\
         exit /b %ERRORLEVEL%\r\n",
        vcvars = vcvars.display(),
        inc = include.display(),
        nif = nif_c.display(),
        sqlite = sqlite_c.display(),
        out = shared_object().display(),
    );
    std::fs::write(&bat, script).expect("write build batch");
    let status = Command::new("cmd")
        .args(["/c", &bat.to_string_lossy()])
        .current_dir(TMPDIR) // keep intermediate .obj/.lib out of the repo
        .status()
        .expect("run cl via cmd");
    assert!(status.success(), "MSVC failed to build the SQLite NIF");
    true
}

/// Compile the NIF with the system C compiler (`$CC` or `cc`).
#[cfg(unix)]
fn compile_nif(include: &Path) -> bool {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let nif_c = Path::new(NATIVE_DIR).join("sqlite_nif.c");
    let sqlite_c = Path::new(NATIVE_DIR).join("sqlite3.c");
    let mut cmd = Command::new(&cc);
    cmd.arg("-shared")
        .arg("-fPIC")
        .arg("-O2")
        .arg("-std=c11")
        .arg("-DSQLITE_THREADSAFE=1")
        .arg("-I")
        .arg(include)
        .arg(&nif_c)
        .arg(&sqlite_c)
        .arg("-o")
        .arg(shared_object())
        .arg("-lpthread")
        .arg("-lm");
    if !cfg!(target_os = "macos") {
        cmd.arg("-ldl");
    }
    match cmd.status() {
        Ok(status) => {
            assert!(status.success(), "{cc} failed to build the SQLite NIF");
            true
        }
        Err(e) => {
            eprintln!("SKIP sqlite_nif_smoke: C compiler {cc} not runnable: {e}");
            false
        }
    }
}

/// `erlc <src>` into `TMPDIR`.
fn erlc(erlc_path: &Path, src: &Path) {
    let status = Command::new(erlc_path)
        .arg("-o")
        .arg(TMPDIR)
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
    let include = erts_include_dir(&erl);

    // An OTP install stripped of its dev headers can't build a NIF. Skip loudly
    // rather than fail the whole run, same as a missing compiler.
    if !include.join("erl_nif.h").exists() {
        eprintln!(
            "SKIP sqlite_nif_smoke: erl_nif.h not found under {} (OTP without dev headers)",
            include.display()
        );
        return;
    }

    if !up_to_date() && !compile_nif(&include) {
        return; // no C toolchain: skip loudly (message already printed)
    }
    assert!(shared_object().exists(), "NIF object was not produced");

    // Compile the glue module and the smoke driver next to the NIF object.
    erlc(&erlc_path, Path::new(RIDGE_SQLITE_ERL));
    let smoke_src = Path::new(TMPDIR).join("sqlite_smoke.erl");
    std::fs::write(&smoke_src, SMOKE_ERL).expect("write smoke module");
    erlc(&erlc_path, &smoke_src);

    // erlang:load_nif takes a slash path; hand it the base with no extension.
    let nif_env = nif_base().to_string_lossy().replace('\\', "/");
    let out = Command::new(&erl)
        .env("RIDGE_SQLITE_NIF", &nif_env)
        .args([
            "-noinput",
            "-pa",
            TMPDIR,
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
