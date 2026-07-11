//! Unit check for the Postgres timestamp text parser, without a database.
//!
//! Postgres delivers `timestamp`/`timestamptz` (type OIDs 1114/1184) as ISO text
//! over the wire, and the adapter parses that text into the epoch-microsecond
//! `SqlInstant` value so a `Timestamp` column round-trips. That parser is the only
//! wire-decode step with real logic — a zone offset to normalise, a naive value to
//! read as UTC, a trimmed fraction to pad — so it is worth pinning directly rather
//! than only through the live-database round-trip in `data_pg_timestamp_e2e`.
//!
//! `ridge_pg.erl` is a self-contained protocol client, so this compiles it in
//! isolation with `erlc` and calls the exported `pg_timestamp_to_micros/1` on a
//! battery of the shapes Postgres emits. No connection is opened. Gated on the
//! `beam-runtime` feature and a `which` guard for `erl`/`erlc`; it needs no
//! `RIDGE_TEST_PG_URL`, so it runs on every machine with OTP installed.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

#[test]
fn pg_timestamp_text_parses_to_epoch_micros() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping pg_timestamp_text_parses_to_epoch_micros");
        return;
    }

    let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ridge-codegen-erl")
        .join("runtime")
        .join("ridge_pg.erl");
    assert!(
        runtime.exists(),
        "ridge_pg.erl not found at {}",
        runtime.display()
    );

    let out = tempfile::Builder::new()
        .prefix("ridge-pg-ts-decode-")
        .tempdir()
        .expect("temp dir");

    let compile = Command::new("erlc")
        .arg("-o")
        .arg(out.path())
        .arg(&runtime)
        .output()
        .expect("run erlc");
    assert!(
        compile.status.success(),
        "erlc failed to compile ridge_pg.erl:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // Each case prints `<label>=<micros>`. The cases cover: a whole-second UTC
    // instant, microsecond precision, a fraction Postgres trimmed of trailing
    // zeros (padded back out), a negative zone offset normalised to UTC, a naive
    // timestamp read as UTC, a fractional-hour offset, and a pre-epoch instant
    // (negative micros).
    let expr = r#"
        M = fun(B) -> ridge_pg:pg_timestamp_to_micros(B) end,
        io:format("utc=~p~n",   [M(<<"2026-07-06 18:09:05+00">>)]),
        io:format("micros=~p~n",[M(<<"2026-07-06 18:09:05.789012+00">>)]),
        io:format("trim=~p~n",  [M(<<"2026-07-06 18:09:05.5+00">>)]),
        io:format("neg=~p~n",   [M(<<"2026-07-06 13:09:05-05">>)]),
        io:format("naive=~p~n", [M(<<"2026-07-06 18:09:05">>)]),
        io:format("half=~p~n",  [M(<<"2026-07-06 18:39:05+00:30">>)]),
        io:format("epoch=~p~n", [M(<<"1969-12-31 23:59:59+00">>)]),
        halt().
    "#;

    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(out.path())
        .arg("-eval")
        .arg(expr)
        .output()
        .expect("run erl");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // 2026-07-06T18:09:05Z is 1_783_361_345 seconds after the epoch; the first
    // three, the negative-offset, the naive, and the half-hour-offset cases all
    // denote that same instant (with their own fraction).
    for (probe, why) in [
        ("utc=1783361345000000", "a whole-second UTC instant"),
        (
            "micros=1783361345789012",
            "microsecond precision is preserved",
        ),
        (
            "trim=1783361345500000",
            "a fraction Postgres trimmed to `.5` pads back to 500000 micros",
        ),
        (
            "neg=1783361345000000",
            "a -05 offset normalises to the same UTC instant",
        ),
        (
            "naive=1783361345000000",
            "a timestamp without an offset is read as UTC",
        ),
        (
            "half=1783361345000000",
            "a +00:30 fractional-hour offset normalises to UTC",
        ),
        (
            "epoch=-1000000",
            "a pre-epoch instant decodes to negative micros",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
