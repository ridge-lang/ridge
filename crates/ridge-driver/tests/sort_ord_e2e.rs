//! End-to-end checks that `List.sort` / `List.sortBy` order by a type's `Ord`
//! instance rather than the BEAM's native term order.
//!
//! The motivating case: a union with a derived `Ord` must sort by its declared
//! variant order, not by how its tagged-tuple / atom representation happens to
//! compare natively. Primitives keep native ordering (their `Ord` *is* the term
//! order), and sorting records by a field key works through the primitive `Ord`.
//!
//! Each `pub fn` returns an `Int` asserted from a single BEAM boot. Gated on
//! `beam-runtime` plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

const SOURCE: &str = r#"
import std.list as List

type Ev = Val Int | Tick
    deriving (Eq, Ord, ToText)

-- Sorting a union honours the declared variant order (Val before Tick), and the
-- payload breaks ties. Native term order would put the bare atom 'Tick' before
-- the {'Val', N} tuples, which is the bug this guards against. Returns the first
-- element's payload (1 when correctly `Val 1` leads), or a negative sentinel.
pub fn union_decl_order () -> Int =
    let s = List.sortBy (fn e -> e) [Tick, Val 5, Val 1]
    match s
        []          -> 0 - 1
        [first, ..] ->
            match first
                Val n -> n
                Tick  -> 0 - 2

-- Primitive element sort keeps ascending numeric order.
pub fn int_sort_asc () -> Int =
    match List.sort [30, 10, 20]
        []          -> 0 - 1
        [first, ..] -> first

-- Sorting records by an Int field key (the most common `sortBy`), through the
-- primitive Ord of the key.
pub fn sortby_int_field () -> Int =
    let xs = [{ id = 3 }, { id = 1 }, { id = 2 }]
    let s = List.sortBy (fn r -> r.id) xs
    match s
        []          -> 0 - 1
        [first, ..] -> first.id
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"sort-ord-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn sort_orders_by_ord_instance() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping sort_orders_by_ord_instance");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-sort-ord-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-sort-ord-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|stem| stem.starts_with("ridge_module_"))
        .expect("a user module")
        .to_owned();

    let expr = format!(
        "F=fun(N)->io:format(\"~s=~p~n\",[N,{module}:N()])end, \
         lists:foreach(F,['union_decl_order','int_sort_asc','sortby_int_field']), halt()."
    );
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    for (name, want) in [
        ("union_decl_order", 1), // Val 1 leads, proving declared order beats native
        ("int_sort_asc", 10),
        ("sortby_int_field", 1),
    ] {
        let needle = format!("{name}={want}");
        assert!(
            stdout.contains(&needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
