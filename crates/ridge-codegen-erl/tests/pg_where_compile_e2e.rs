//! Verifies the Postgres WHERE compiler (`ridge_pg:compile_where/2`) renders the
//! `QLike`, `QIn`, and correlated `QExists` predicate nodes to parameterised SQL.
//!
//! This is the renderer the last `cw`-path read takes — `groupSummarize` — and it
//! is distinct from the plan renderer (`query.ridge`'s `planToSql`) that every other
//! read now uses. The two must stay in lockstep on every `QExpr` node, so this locks
//! the `cw` path the way `query_plan_sql_e2e` locks the plan path.
//!
//! Method: compile the bundled `ridge_pg.erl` with `erlc +export_all` so the
//! internal `compile_where/2` is reachable, then `erl -eval` it on a few `QExpr`
//! trees and assert the SQL fragment and bind count. Skips cleanly when
//! `erlc`/`erl` are not on PATH.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::tempdir;

const ERL_TIMEOUT_SECS: u64 = 30;

// Render a handful of WHERE trees, each as `<key>=<sql>|<bind-count>`.
const EVAL: &str = r#"
F = fun(Tree) -> {Frag, Binds} = ridge_pg:compile_where(Tree, <<"users">>), io:format("~s|~w~n", [Frag, length(Binds)]) end,
io:format("like="), F({'QLike', {'QCol', <<"name">>}, {'QLitText', <<"%a%">>}}),
io:format("exists="), F({'QExists', <<"posts">>, {'QEq', {'QColR', <<"author">>}, {'QCol', <<"id">>}}}),
io:format("in="), F({'QIn', {'QCol', <<"age">>}, [{'QLitInt', 18}, {'QLitInt', 30}]}),
io:format("inempty="), F({'QIn', {'QCol', <<"age">>}, []}),
io:format("andmix="), F({'QAnd', {'QLike', {'QCol', <<"name">>}, {'QLitText', <<"%a%">>}}, {'QGe', {'QCol', <<"age">>}, {'QLitInt', 18}}}),
io:format("arithmul="), F({'QGt', {'QMul', {'QCol', <<"age">>}, {'QLitInt', 2}}, {'QLitInt', 50}}),
io:format("arithcol="), F({'QGt', {'QAdd', {'QCol', <<"age">>}, {'QCol', <<"id">>}}, {'QLitInt', 20}}),
io:format("arithmod="), F({'QEq', {'QMod', {'QCol', <<"age">>}, {'QLitInt', 2}}, {'QLitInt', 0}}),
io:format("casepred="), F({'QCase', {'QGe', {'QCol', <<"age">>}, {'QLitInt', 18}}, {'QLitBool', true}, {'QLitBool', false}}),
halt().
"#;

fn run_erl_capture(beam_dir: &std::path::Path, eval: &str) -> (String, String, i32) {
    let erl_path = which::which("erl").expect("erl on PATH");
    let mut child = Command::new(&erl_path)
        .arg("-noinput")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(eval)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn erl");

    let start = Instant::now();
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
fn compile_where_renders_like_and_in() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erlc/erl not on PATH — skipping compile_where_renders_like_and_in");
        return;
    }

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/ridge_pg.erl");
    assert!(src.exists(), "ridge_pg.erl source at {src:?}");

    let td = tempdir().expect("tempdir");
    let beam_dir = td.path();

    // `+export_all` exposes the internal `compile_where/2` without widening the
    // shipped module's API.
    let erlc_path = which::which("erlc").expect("erlc on PATH");
    let status = Command::new(&erlc_path)
        .arg("+export_all")
        .arg("-o")
        .arg(beam_dir)
        .arg(&src)
        .status()
        .expect("run erlc");
    assert!(status.success(), "erlc failed to compile ridge_pg.erl");

    let (stdout, stderr, code) = run_erl_capture(beam_dir, EVAL);
    assert_eq!(
        code, 0,
        "erl exited with {code}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    for (probe, want) in [
        (
            r#"like="name" LIKE $1|1"#,
            "QLike renders a parameterised LIKE, one bind",
        ),
        (
            r#"exists=EXISTS (SELECT 1 FROM "posts" AS sub WHERE sub."author" = "users"."id")|0"#,
            "a correlated QExists renders a NOT-yet-negated EXISTS subquery, the inner row qualified by `sub` and the outer row by its table, no binds",
        ),
        (
            r#"in="age" IN ($1, $2)|2"#,
            "QIn renders one placeholder per element",
        ),
        (
            "inempty=FALSE|0",
            "an empty IN set renders as the constant FALSE, no binds",
        ),
        (
            r#"andmix=("name" LIKE $1 AND "age" >= $2)|2"#,
            "LIKE combines with a comparison under AND, binds in order",
        ),
        (
            r#"arithmul=("age" * $1) > $2|2"#,
            "a column times a literal renders as a parenthesised operand, binds in order",
        ),
        (
            r#"arithcol=("age" + "id") > $1|1"#,
            "a column plus a column binds nothing of its own",
        ),
        (
            r#"arithmod=("age" % $1) = $2|2"#,
            "modulo renders with the divisor bound as a placeholder",
        ),
        (
            r#"casepred=CASE WHEN "age" >= $1 THEN TRUE ELSE FALSE END|1"#,
            "a CASE used as a WHERE predicate renders CASE WHEN … END, binding its condition literal",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "expected `{probe}` ({want})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
