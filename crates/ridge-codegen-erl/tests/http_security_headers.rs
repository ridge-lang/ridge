//! Verifies that `ridge_rt:http_build_response/1` emits the default
//! Content-Security-Policy and Strict-Transport-Security headers on
//! every server response (T-N004 + T-N005 / spec Q-024).
//!
//! Method: install the runtime into a tempdir, compile it with `erlc`,
//! invoke `erl -eval` to call `http_build_response/1` with a small
//! sample input, capture stdout, and assert both headers are present.
//!
//! Skip pattern: if `erlc` or `erl` is not on PATH (e.g. CI runner
//! without OTP), the test prints an explicit skip notice and exits
//! cleanly rather than panicking.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_codegen_erl::{erlc, output_layout, runtime};
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::tempdir;

const ERL_TIMEOUT_SECS: u64 = 30;

const SAMPLE_EVAL: &str = "\
io:put_chars(ridge_rt:http_build_response(#{status => 200, body => <<\"ok\">>})), \
halt().";

fn run_erl_capture(beam_dir: &std::path::Path, eval: &str) -> (String, String, i32) {
    let erl_path = which::which("erl").expect("erl on PATH");
    let mut cmd = Command::new(&erl_path);
    cmd.arg("-noinput")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(eval)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn erl");
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(ERL_TIMEOUT_SECS);
    loop {
        if let Some(status) = child.try_wait().expect("try_wait erl") {
            use std::io::Read;
            let mut out = Vec::new();
            let mut err = Vec::new();
            if let Some(mut s) = child.stdout.take() {
                let _ = s.read_to_end(&mut out);
            }
            if let Some(mut s) = child.stderr.take() {
                let _ = s.read_to_end(&mut err);
            }
            return (
                String::from_utf8_lossy(&out).into_owned(),
                String::from_utf8_lossy(&err).into_owned(),
                status.code().unwrap_or(-1),
            );
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            panic!("erl exceeded {ERL_TIMEOUT_SECS}s timeout");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn http_build_response_emits_csp_and_hsts_defaults() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erlc/erl not on PATH — skipping http_build_response_emits_csp_and_hsts_defaults"
        );
        return;
    }

    // Install ridge_rt.erl + compile to .beam in a per-test tempdir.
    let td = tempdir().expect("tempdir");
    let out_root = td.path();
    output_layout::ensure_out_dirs(out_root).expect("ensure_out_dirs");
    runtime::install_runtime(out_root).expect("install_runtime");

    let info = erlc::probe(None).expect("erlc probe");
    let beam_path = runtime::compile_runtime(&info.path, out_root).expect("compile_runtime");
    assert!(beam_path.exists(), "ridge_rt.beam at {beam_path:?}");

    let beam_dir = output_layout::beam_dir(out_root);
    let (stdout, stderr, code) = run_erl_capture(&beam_dir, SAMPLE_EVAL);

    assert_eq!(
        code, 0,
        "erl exited with {code}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    assert!(
        stdout.contains("Content-Security-Policy: default-src 'self'"),
        "expected Content-Security-Policy default in response; got stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Strict-Transport-Security: max-age=31536000"),
        "expected Strict-Transport-Security default in response; got stdout:\n{stdout}"
    );
}
