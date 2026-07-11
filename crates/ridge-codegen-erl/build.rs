//! Build script: bake the SQLite NIF into the crate.
//!
//! SQLite reaches the BEAM through a native function (runtime/native/sqlite_nif.c
//! over the vendored amalgamation). Compiling C on a user's machine at `ridge
//! run` time would put a C toolchain in the critical path, so instead the object
//! is built here — once, when Ridge itself is built — and embedded into the
//! compiler binary via `include_bytes!`. `ridge run`/`ridge build` then just
//! write it to disk; no compiler is ever needed to run a Ridge program.
//!
//! This only happens under the `beam-runtime` feature (the same gate the live
//! OTP tests use). A plain `cargo build` skips it entirely, so the ordinary
//! build needs neither a C compiler nor an `erl_nif.h`.

// A build script fails the build by panicking, so the usual restrictions on
// `expect`/`panic` do not apply here.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=runtime/native/sqlite_nif.c");
    println!("cargo:rerun-if-changed=runtime/native/sqlite3.c");
    println!("cargo:rerun-if-changed=runtime/native/sqlite3.h");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var_os("CARGO_FEATURE_BEAM_RUNTIME").is_none() {
        return;
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let native = manifest.join("runtime").join("native");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let out_bin = out_dir.join("ridge_sqlite_nif.bin");
    let nif_c = native.join("sqlite_nif.c");
    let sqlite_c = native.join("sqlite3.c");

    let include = erts_include_dir();
    assert!(
        include.join("erl_nif.h").exists(),
        "erl_nif.h not found under {} — the beam-runtime feature needs an OTP with dev headers",
        include.display()
    );

    let compiler = cc::Build::new().get_compiler();
    let mut cmd = compiler.to_command();
    if compiler.is_like_msvc() {
        cmd.arg("/nologo")
            .arg("/LD")
            .arg("/O2")
            .arg("/std:c11")
            .arg("/DSQLITE_THREADSAFE=1")
            .arg(format!("/I{}", include.display()))
            .arg(&nif_c)
            .arg(&sqlite_c)
            .arg(format!("/Fe:{}", out_bin.display()))
            // Keep intermediate .obj files out of the source tree.
            .current_dir(&out_dir);
    } else {
        cmd.arg("-shared")
            .arg("-fPIC")
            .arg("-O2")
            .arg("-std=c11")
            .arg("-DSQLITE_THREADSAFE=1")
            .arg("-I")
            .arg(&include)
            .arg(&nif_c)
            .arg(&sqlite_c)
            .arg("-o")
            .arg(&out_bin)
            .arg("-lpthread")
            .arg("-lm");
        if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
            cmd.arg("-ldl");
        }
    }

    let status = cmd.status().expect("run the C compiler for the SQLite NIF");
    assert!(
        status.success(),
        "the C compiler failed to build the SQLite NIF"
    );
    assert!(
        out_bin.exists(),
        "the SQLite NIF object was not produced at {}",
        out_bin.display()
    );
}

/// `<otp-root>/erts-<ver>/include`, home of `erl_nif.h`.
fn erts_include_dir() -> PathBuf {
    let out = Command::new("erl")
        .args([
            "-noshell",
            "-eval",
            "io:format(\"~ts\",[filename:join([code:root_dir(),\"erts-\"++erlang:system_info(version),\"include\"])])",
            "-s",
            "init",
            "stop",
        ])
        .output()
        .expect("run erl to resolve the erts include dir (needed to build the SQLite NIF)");
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        !dir.is_empty(),
        "could not resolve the erts include dir from erl"
    );
    PathBuf::from(dir)
}
