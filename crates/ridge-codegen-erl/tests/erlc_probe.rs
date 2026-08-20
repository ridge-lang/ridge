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
        // The contract under test: garbage input is classified as E004 with a
        // non-zero exit. Whether erlc routes the diagnostic to stderr (vs stdout,
        // or emits nothing for an input rejected this early) varies across OTP
        // releases, so the stderr content is not asserted.
        CodegenError::ErlcRejectedInput { exit_code, .. } => {
            assert_ne!(exit_code, 0, "exit code must be non-zero");
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

/// A crashing benchmark is reported in Ridge's words, and does not take the
/// rest of the run with it.
///
/// The bench runner is the one place the prefix is built with `io_lib:format`
/// instead of passed as a binary, so it is the call site whose shape differs
/// from the other three — and it was the only crash clause of the four that
/// nothing ran. The second half is the claim the runner's own comment makes
/// and nothing checked: one crashing benchmark must not abort the others.
#[test]
#[cfg_attr(
    not(feature = "beam-runtime"),
    ignore = "requires OTP installation; run with --features beam-runtime"
)]
fn a_crashing_benchmark_is_named_and_does_not_stop_the_run() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping a_crashing_benchmark_is_named_...");
        return;
    }

    let dir = tempdir().unwrap();
    let out_root = dir.path();
    output_layout::ensure_out_dirs(out_root).expect("ensure_out_dirs");
    runtime::install_runtime(out_root).expect("install_runtime");
    runtime::install_bench_runner(out_root).expect("install_bench_runner");
    let info = erlc::probe(None).expect("probe");
    runtime::compile_runtime(&info.path, out_root).expect("compile_runtime");
    runtime::compile_bench_runner(&info.path, out_root).expect("compile_bench_runner");

    // Two details make this module match what Ridge emits rather than what
    // Erlang would write by hand. The divisor comes from a function, so erlc
    // reports no warning about an expression it can already see will fail. And
    // the division is a qualified call: written as `1 / zero()` it compiles to
    // an inline instruction whose stacktrace names only the calling function,
    // and `arith_failure/1` would correctly fall back to its hedge — the test
    // would then be asserting Ridge's wording against Erlang's shape. Ridge's
    // `/` on `Int` lowers to an external `erlang:div/2`, which leaves the BIF
    // frame that carries the arguments and lets the fault be named.
    let beam_dir = output_layout::beam_dir(out_root);
    let demo_erl = out_root.join("bench_crash_demo.erl");
    fs::write(
        &demo_erl,
        "-module(bench_crash_demo).\n\
         -export([bench_boom/0, bench_fine/0]).\n\
         bench_boom() -> erlang:'div'(1, zero()).\n\
         bench_fine() -> lists:sum(lists:seq(1, 1000)).\n\
         zero() -> 0.\n",
    )
    .expect("write bench_crash_demo.erl");
    let status = std::process::Command::new(&info.path)
        .arg("-o")
        .arg(&beam_dir)
        .arg(&demo_erl)
        .status()
        .expect("erlc bench_crash_demo");
    assert!(status.success(), "erlc must compile bench_crash_demo");

    let output = std::process::Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-s")
        .arg("ridge_bench_runner")
        .arg("run")
        .arg("bench_crash_demo")
        .arg("-s")
        .arg("init")
        .arg("stop")
        .output()
        .expect("run bench runner");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let ctx = format!("stdout:\n{stdout}\nstderr:\n{stderr}");

    // The crash keeps this command's framing and gains Ridge's words.
    assert!(
        stderr.contains("bench bench_boom crashed: divided by zero"),
        "{ctx}"
    );
    assert!(
        stderr.contains("RIDGE_BACKTRACE"),
        "the way to the Erlang must still be offered; {ctx}"
    );
    assert!(
        !stderr.contains("badarith"),
        "the raw OTP reason should be behind the variable; {ctx}"
    );

    // The marker line is the machine contract, and it is on stdout, separate
    // from anything a person reads. Changing the human half must not move it.
    assert!(
        stdout.contains("{\"bench\":\"bench_boom\",\"error\":true}"),
        "{ctx}"
    );

    // And the run went on.
    assert!(
        stdout.contains("\"bench\":\"bench_fine\"") && stdout.contains("\"median_ns\":"),
        "a crashing benchmark must not abort the ones after it; {ctx}"
    );
}

/// What `exit_reason_to_ridge/1` puts inside a `Crashed`, across every shape.
///
/// Driven from Erlang rather than from Ridge source because the interesting
/// cases include reasons no Ridge program can raise on purpose — an actor
/// killed from outside, or a term a library chose — and those are exactly the
/// ones where the answer used to be an Erlang dump in a `Text` field.
///
/// Both directions are asserted. A reason Ridge has words for must lose its
/// stack; a reason Ridge does not have words for must keep its term, framed so
/// a reader can tell which of the two they are looking at. Getting only the
/// first half right would read as a success and be a guess in Ridge's voice.
#[test]
#[cfg_attr(
    not(feature = "beam-runtime"),
    ignore = "requires OTP installation; run with --features beam-runtime"
)]
fn a_crashed_payload_says_what_ridge_knows_and_frames_what_it_does_not() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping exit_reason_to_ridge probe");
        return;
    }

    let dir = tempdir().unwrap();
    let out_root = dir.path();
    output_layout::ensure_out_dirs(out_root).expect("ensure_out_dirs");
    runtime::install_runtime(out_root).expect("install_runtime");
    let info = erlc::probe(None).expect("probe");
    runtime::compile_runtime(&info.path, out_root).expect("compile_runtime");

    let beam_dir = output_layout::beam_dir(out_root);
    let probe_erl = out_root.join("reason_probe.erl");
    // Each line carries the answer alone. Echoing the reason as well would put
    // `gen_server` in stdout whatever the runtime did with it, and the
    // assertion that no stacktrace reaches a `Text` field would then be
    // reading a line the runtime never produced.
    fs::write(
        &probe_erl,
        "-module(reason_probe).\n\
         -export([run/0]).\n\
         run() ->\n\
             Stack = [{worker, handle_call, 3, [{file, \"worker.erl\"}, {line, 5}]},\n\
                      {gen_server, try_handle_call, 4, [{file, \"gen_server.erl\"}, {line, 2470}]}],\n\
             show(\"normal\", normal),\n\
             show(\"shutdown\", shutdown),\n\
             show(\"noproc\", noproc),\n\
             show(\"killed\", killed),\n\
             show(\"ask\", ridge_ask_noproc),\n\
             show(\"range\", {{ridge_int_out_of_range, <<\"Int.add\">>, 99}, Stack}),\n\
             show(\"arith\", {badarith, [{erlang, 'div', [1, 0], []} | Stack]}),\n\
             show(\"tagged\", {my_own_tag, [1, 2, 3]}).\n\
         show(Label, R) ->\n\
             case ridge_rt:exit_reason_to_ridge(R) of\n\
                 {'Crashed', Text} -> io:format(\"~s|Crashed|~ts~n\", [Label, Text]);\n\
                 Ordered           -> io:format(\"~s|~p~n\", [Label, Ordered])\n\
             end.\n",
    )
    .expect("write reason_probe.erl");
    let status = std::process::Command::new(&info.path)
        .arg("-o")
        .arg(&beam_dir)
        .arg(&probe_erl)
        .status()
        .expect("erlc reason_probe");
    assert!(status.success(), "erlc must compile reason_probe");

    let output = std::process::Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-s")
        .arg("reason_probe")
        .arg("run")
        .arg("-s")
        .arg("init")
        .arg("stop")
        .output()
        .expect("run reason_probe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let ctx = format!("stdout:\n{stdout}\nstderr:\n{stderr}");

    // The probe has to be able to fail: if the lines never arrived, every
    // `contains` below would be unsatisfied for a reason that has nothing to
    // do with what is under test.
    assert_eq!(
        stdout.lines().filter(|l| l.contains('|')).count(),
        8,
        "the probe must report all eight reasons; {ctx}"
    );

    // An ordered stop is not a crash and never was.
    assert!(stdout.contains("normal|'Normal'"), "{ctx}");
    assert!(stdout.contains("shutdown|'Shutdown'"), "{ctx}");
    assert!(stdout.contains("noproc|'NotRunning'"), "{ctx}");

    // What Ridge has words for arrives as those words.
    assert!(
        stdout.contains("ask|Crashed|asked an actor that is no longer running"),
        "{ctx}"
    );
    assert!(
        stdout.contains("range|Crashed|`Int.add` produced 99, which is outside the range of `Int`"),
        "{ctx}"
    );
    assert!(stdout.contains("arith|Crashed|divided by zero"), "{ctx}");

    // And the stacktrace it arrived with does not come along. This is the
    // assertion the whole test exists for: the payload is a `Text` field, and
    // it used to hold gen_server's own frames.
    assert!(
        !stdout.contains("gen_server"),
        "a stacktrace reached a Text field; {ctx}"
    );

    // What Ridge has no words for keeps its term, and says as much.
    assert!(
        stdout.contains("killed|Crashed|the actor stopped: killed"),
        "{ctx}"
    );
    assert!(
        stdout.contains("tagged|Crashed|the actor stopped: {my_own_tag,[1,2,3]}"),
        "a reason that merely looks like a stacktrace must keep both halves; {ctx}"
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
