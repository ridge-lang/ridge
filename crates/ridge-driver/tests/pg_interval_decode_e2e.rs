//! Unit check for the Postgres interval text parser, without a database.
//!
//! Postgres delivers `interval` (type OID 1186) as its `postgres`-style text over the
//! wire, and the adapter folds that text into the whole-millisecond `SqlInterval`
//! value so a `Duration` column round-trips. That parser is the only wire-decode step
//! with real logic — a sub-second fraction to read, hours that may exceed 24, a sign,
//! and the calendar words (`day`/`mon`/`year`) a foreign client can write — so it is
//! worth pinning directly rather than only through the live-database round-trip in
//! `data_pg_interval_e2e`.
//!
//! `ridge_pg.erl` is a self-contained protocol client, so this compiles it in
//! isolation with `erlc` and calls the exported `pg_interval_to_ms/1` on a battery of
//! the shapes Postgres emits. No connection is opened. Gated on the `beam-runtime`
//! feature and a `which` guard for `erl`/`erlc`; it needs no `RIDGE_TEST_PG_URL`, so it
//! runs on every machine with OTP installed.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

#[test]
fn pg_interval_text_parses_to_milliseconds() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping pg_interval_text_parses_to_milliseconds");
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
        .prefix("ridge-pg-interval-decode-")
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

    // Each case prints `<label>=<ms>`. The cases cover: a sub-second fraction, a
    // Ridge-written whole-second span, a span whose hours exceed 24 (Ridge writes a
    // day-plus duration as `25:00:00` and up), a negative span, a bare calendar day
    // and month (a foreign client's `interval`, read with a 24-hour day and 30-day
    // month), a mixed calendar-plus-time value, and a negative mixed value.
    let expr = r#"
        M = fun(B) -> ridge_pg:pg_interval_to_ms(B) end,
        io:format("frac=~p~n",     [M(<<"00:00:01.5">>)]),
        io:format("ninety=~p~n",   [M(<<"00:01:30">>)]),
        io:format("bighours=~p~n", [M(<<"25:00:00">>)]),
        io:format("neg=~p~n",      [M(<<"-00:00:01.5">>)]),
        io:format("day=~p~n",      [M(<<"1 day">>)]),
        io:format("daytime=~p~n",  [M(<<"1 day 02:03:04">>)]),
        io:format("mon=~p~n",      [M(<<"1 mon">>)]),
        io:format("mixed=~p~n",    [M(<<"1 year 2 mons 3 days 04:05:06.789">>)]),
        io:format("negmixed=~p~n", [M(<<"-1 mons +3 days -00:00:01">>)]),
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

    for (probe, why) in [
        ("frac=1500", "a `.5` sub-second fraction reads as 500 ms"),
        (
            "ninety=90000",
            "a Ridge-written 90-second span (`00:01:30`) folds to 90000 ms",
        ),
        (
            "bighours=90000000",
            "hours past 24 (`25:00:00`) are read as-is, not justified into days",
        ),
        ("neg=-1500", "a leading `-` negates the whole time part"),
        ("day=86400000", "a bare `1 day` is read with a 24-hour day"),
        (
            "daytime=93784000",
            "a `1 day 02:03:04` sums the day and the time part",
        ),
        (
            "mon=2592000000",
            "a bare `1 mon` is read with a 30-day month",
        ),
        (
            "mixed=36561906789",
            "a year/mon/day/time value sums with a 12-month year, 30-day month, 24-hour day",
        ),
        (
            "negmixed=-2332801000",
            "each field carries its own sign in a mixed negative value",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
