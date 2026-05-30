//! Integration tests for `erlc::probe`, `erlc::compile_core`, the
//! runtime/output-layout helpers, and the end-to-end `codegen_workspace` path.
//!
//! Tests that depend on `erlc` being present on PATH are gated with `which`
//! and skip cleanly otherwise — CI runners without OTP installed see them as
//! passing-skips, not failures.
//!
//! Tests that require a real OTP installation are additionally gated behind
//! `#[cfg_attr(not(feature = "beam-runtime"), ignore = "requires OTP installation; run with --features beam-runtime")]`; run with
//! `cargo test --features beam-runtime` to enable them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_codegen_erl::{
    codegen_workspace, erlc, output_layout, runtime, BuildProfile, CodegenError, CodegenOptions,
};
use ridge_ir::{
    IrConst, IrExpr, IrItem, IrLit, IrNodeId, LoweredModule, LoweredWorkspace, ModuleId, NodeId,
    Span, Type,
};
use rustc_hash::FxHashMap;
use std::fs;
use tempfile::tempdir;

// ── helpers shared across new tests ──────────────────────────────────────────

const fn sp() -> Span {
    Span::point(0)
}

const fn lit_int(n: i64) -> IrExpr {
    IrExpr::Lit {
        id: IrNodeId(0),
        value: IrLit::Int(n),
        span: sp(),
    }
}

fn make_const(name: &str, is_pub: bool, value: IrExpr) -> IrConst {
    IrConst {
        name: name.into(),
        ty: Type::Error,
        value,
        origin: NodeId(0),
        span: sp(),
        is_pub,
    }
}

fn make_lowered_module(id: u32, items: Vec<IrItem>) -> LoweredModule {
    LoweredModule::new(ModuleId(id), items, vec![], FxHashMap::default())
}

#[test]
fn probe_below_min_version_rejects() {
    assert!(erlc::validate(25).is_err());
    match erlc::validate(25).unwrap_err() {
        CodegenError::ErlcVersionTooOld { found, minimum } => {
            assert_eq!(found, "OTP 25");
            assert_eq!(minimum, "OTP 26");
        }
        _ => panic!("expected ErlcVersionTooOld"),
    }
    assert!(erlc::validate(26).is_ok());
    assert!(erlc::validate(27).is_ok());
}

#[test]
fn probe_succeeds_when_erlc_on_path() {
    if which::which("erlc").is_err() {
        eprintln!("erlc not on PATH — skipping probe_succeeds_when_erlc_on_path");
        return;
    }
    let info = erlc::probe(None).expect("erlc on PATH should probe successfully");
    assert!(info.version >= erlc::MIN_OTP_VERSION);
    assert!(info.path.exists());
}

#[test]
fn install_runtime_is_idempotent() {
    let dir = tempdir().unwrap();
    let out_root = dir.path();
    output_layout::ensure_out_dirs(out_root).expect("ensure_out_dirs");
    let info1 = runtime::install_runtime(out_root).expect("first install");
    let mtime1 = fs::metadata(&info1.erl_path).unwrap().modified().unwrap();
    // Sleep a tick so any spurious rewrite would change mtime.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let info2 = runtime::install_runtime(out_root).expect("second install");
    let mtime2 = fs::metadata(&info2.erl_path).unwrap().modified().unwrap();
    assert_eq!(info1.erl_path, info2.erl_path);
    assert_eq!(mtime1, mtime2, "idempotent install must not rewrite");
}

#[test]
fn output_dir_creation_creates_subdirs() {
    let dir = tempdir().unwrap();
    let out_root = dir.path();
    output_layout::ensure_out_dirs(out_root).expect("ensure_out_dirs");
    assert!(out_root.join("core").is_dir());
    assert!(out_root.join("beam").is_dir());
    assert!(out_root.join("runtime").is_dir());
    // Idempotent: second call must succeed.
    output_layout::ensure_out_dirs(out_root).expect("idempotent ensure");
}

#[test]
fn resolve_out_root_uses_profile_subdir() {
    let debug = output_layout::resolve_out_root(BuildProfile::Debug);
    let release = output_layout::resolve_out_root(BuildProfile::Release);
    assert!(debug.ends_with("debug"));
    assert!(release.ends_with("release"));
    assert_ne!(debug, release);
}

// ── T10 new tests ─────────────────────────────────────────────────────────────

/// `compile_core` invokes `erlc +from_core` on a valid minimal Core Erlang file
/// and produces a `.beam` file.
///
/// Gated on `beam-runtime` feature (real OTP required) AND `which::which` guard
/// (belt-and-braces skip if erlc is somehow absent even with the feature).
#[test]
#[cfg_attr(
    not(feature = "beam-runtime"),
    ignore = "requires OTP installation; run with --features beam-runtime"
)]
fn compile_core_invokes_erlc_on_valid_input() {
    if which::which("erlc").is_err() {
        eprintln!("erlc not on PATH — skipping compile_core_invokes_erlc_on_valid_input");
        return;
    }

    let dir = tempdir().unwrap();
    let out_root = dir.path();
    output_layout::ensure_out_dirs(out_root).expect("ensure_out_dirs");
    runtime::install_runtime(out_root).expect("install_runtime");

    // Write a trivial valid Core Erlang module.
    let core_src = "module 'tt' []\n  attributes []\nend\n";
    let core_path = output_layout::core_file_path(out_root, "tt");
    fs::write(&core_path, core_src).expect("write .core");

    let info = erlc::probe(None).expect("probe");
    let beam_out = output_layout::beam_dir(out_root);
    let rt_dir = output_layout::runtime_dir(out_root);

    let artifact = erlc::compile_core(
        &info.path,
        &core_path,
        &beam_out,
        &rt_dir,
        BuildProfile::Debug,
    )
    .expect("compile_core should succeed on valid input");

    assert!(
        artifact.beam_path.exists(),
        "expected .beam at {:?}",
        artifact.beam_path
    );
}

/// `compile_core` returns `E004 ErlcRejectedInput` when `erlc` exits non-zero
/// (i.e. the `.core` file contains parse errors).
///
/// Gated on `beam-runtime` feature and `which` guard.
#[test]
#[cfg_attr(
    not(feature = "beam-runtime"),
    ignore = "requires OTP installation; run with --features beam-runtime"
)]
fn compile_core_returns_e004_on_subprocess_exit_failure() {
    if which::which("erlc").is_err() {
        eprintln!(
            "erlc not on PATH — skipping compile_core_returns_e004_on_subprocess_exit_failure"
        );
        return;
    }

    let dir = tempdir().unwrap();
    let out_root = dir.path();
    output_layout::ensure_out_dirs(out_root).expect("ensure_out_dirs");
    runtime::install_runtime(out_root).expect("install_runtime");

    // Deliberately malformed Core Erlang — erlc will reject it.
    let garbage = b"this is not valid core erlang at all @@@@";
    let core_path = output_layout::core_file_path(out_root, "bad_module");
    fs::write(&core_path, garbage).expect("write garbage .core");

    let info = erlc::probe(None).expect("probe");
    let beam_out = output_layout::beam_dir(out_root);
    let rt_dir = output_layout::runtime_dir(out_root);

    let err = erlc::compile_core(
        &info.path,
        &core_path,
        &beam_out,
        &rt_dir,
        BuildProfile::Debug,
    )
    .expect_err("expected E004 on garbage input");

    match err {
        CodegenError::ErlcRejectedInput {
            exit_code, stderr, ..
        } => {
            assert_ne!(exit_code, 0, "exit code must be non-zero");
            assert!(!stderr.is_empty(), "stderr must contain error text");
        }
        other => panic!("expected ErlcRejectedInput, got {other:?}"),
    }
}

/// The Layer B bench runner installs, compiles, discovers `bench_*/0` exports
/// in a module, times them, and prints one machine-readable JSON line each.
///
/// Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.
#[test]
#[cfg_attr(
    not(feature = "beam-runtime"),
    ignore = "requires OTP installation; run with --features beam-runtime"
)]
fn bench_runner_times_and_reports_bench_functions() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping bench_runner_times_and_reports_bench_functions");
        return;
    }

    let dir = tempdir().unwrap();
    let out_root = dir.path();
    output_layout::ensure_out_dirs(out_root).expect("ensure_out_dirs");

    // Install + compile the bench runner.
    runtime::install_bench_runner(out_root).expect("install_bench_runner");
    let info = erlc::probe(None).expect("probe");
    runtime::compile_bench_runner(&info.path, out_root).expect("compile_bench_runner");

    // A hand-written bench module: one trivial body and one that does enough
    // work to clear the clock resolution, so we can assert a real timing.
    let beam_dir = output_layout::beam_dir(out_root);
    let demo_erl = out_root.join("bench_demo.erl");
    fs::write(
        &demo_erl,
        "-module(bench_demo).\n\
         -export([bench_noop/0, bench_listwork/0]).\n\
         bench_noop() -> ok.\n\
         bench_listwork() -> lists:sum(lists:seq(1, 100000)).\n",
    )
    .expect("write bench_demo.erl");
    let status = std::process::Command::new(&info.path)
        .arg("-o")
        .arg(&beam_dir)
        .arg(&demo_erl)
        .status()
        .expect("erlc bench_demo");
    assert!(status.success(), "erlc must compile bench_demo");

    // Run every bench in a single BEAM boot.
    let output = std::process::Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-s")
        .arg("ridge_bench_runner")
        .arg("run")
        .arg("bench_demo")
        .arg("-s")
        .arg("init")
        .arg("stop")
        .output()
        .expect("run bench runner");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("\"bench\":\"bench_noop\""),
        "missing bench_noop result line:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("\"bench\":\"bench_listwork\""),
        "missing bench_listwork result line:\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("\"median_ns\":") && stdout.contains("\"p99_ns\":"),
        "result lines must carry median_ns and p99_ns:\n{stdout}"
    );
    // The substantial body must register a non-zero median (proves timing works,
    // not just that lines are printed).
    let listwork_line = stdout
        .lines()
        .find(|l| l.contains("bench_listwork"))
        .expect("bench_listwork line present");
    assert!(
        !listwork_line.contains("\"median_ns\":0,"),
        "a 100k-element body must measure above clock resolution:\n{listwork_line}"
    );
}

/// `compile_core` returns `E003 ErlcNotFound` when the erlc executable path
/// does not exist.  No real erlc needed — the binary simply doesn't exist.
#[test]
fn compile_core_returns_e003_when_erlc_path_missing() {
    let dir = tempdir().unwrap();
    let out_root = dir.path();
    output_layout::ensure_out_dirs(out_root).expect("ensure_out_dirs");

    // A non-existent erlc path.
    let fake_erlc = dir.path().join("not_erlc");
    // A dummy .core path (doesn't need to exist — spawn will fail first).
    let core_path = output_layout::core_file_path(out_root, "dummy");
    let beam_out = output_layout::beam_dir(out_root);
    let rt_dir = output_layout::runtime_dir(out_root);

    let err = erlc::compile_core(
        &fake_erlc,
        &core_path,
        &beam_out,
        &rt_dir,
        BuildProfile::Debug,
    )
    .expect_err("expected E003 when erlc path is missing");

    assert!(
        matches!(err, CodegenError::ErlcNotFound { .. }),
        "expected ErlcNotFound, got {err:?}"
    );
}

/// `codegen_workspace` writes `.core` files to disk for each module in the
/// workspace and populates `CodegenResult.modules` accordingly.
///
/// Does not invoke `erlc` (`invoke_erlc: false`).
#[test]
fn codegen_workspace_writes_core_files_to_disk() {
    let dir = tempdir().unwrap();

    let items = vec![IrItem::Const(make_const("PI", true, lit_int(3)))];
    let module = make_lowered_module(0, items);
    let ws = LoweredWorkspace::new(vec![Some(module)], 0);

    let mut opts = CodegenOptions::default();
    opts.out_root = dir.path().to_path_buf();
    opts.invoke_erlc = false;
    opts.install_runtime = false;

    let result = codegen_workspace(&ws, opts);

    assert!(
        result.errors.is_empty(),
        "expected no errors, got: {:?}",
        result.errors
    );

    let module_result = result.modules[0]
        .as_ref()
        .expect("module[0] should be Some after successful codegen");

    assert!(
        !module_result.core_path.as_os_str().is_empty(),
        "core_path must be non-empty"
    );
    assert!(
        module_result.core_path.exists(),
        "core file must exist on disk at {:?}",
        module_result.core_path
    );

    let core_text = fs::read_to_string(&module_result.core_path).expect("read core file");
    assert!(
        core_text.contains("module 'ridge_module_0' ["),
        "core file must declare the expected module atom; got:\n{core_text}"
    );
}

/// `codegen_workspace` returns `E005 OutputDirNotWritable` in `errors` and no
/// module results when the `out_root` cannot be created (e.g. it points at an
/// existing regular file so `create_dir_all` fails).
///
/// Skipped on Windows if pathological path semantics differ.
#[test]
#[cfg(not(windows))]
fn codegen_workspace_returns_e005_when_out_root_not_writable() {
    let dir = tempdir().unwrap();

    // Create a regular FILE at the out_root path so create_dir_all fails.
    let out_root = dir.path().join("not_a_dir");
    fs::write(&out_root, b"I am a file, not a directory").expect("write blocker file");

    let ws = LoweredWorkspace::empty(0, 0);
    let mut opts = CodegenOptions::default();
    opts.out_root = out_root;
    opts.invoke_erlc = false;
    opts.install_runtime = false;

    let result = codegen_workspace(&ws, opts);

    assert!(
        result.modules.iter().all(Option::is_none),
        "no modules should be produced on early return"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| matches!(e, CodegenError::OutputDirNotWritable { .. })),
        "expected at least one E005 OutputDirNotWritable error; got: {:?}",
        result.errors
    );
}
