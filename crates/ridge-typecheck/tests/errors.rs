//! T17 — per-`T###` fixture harness for `ridge-typecheck` (plan §10 T17, §9.2).
//!
//! Mirrors Phase 3's `crates/ridge-resolve/tests/errors.rs`.  Each fixture file
//! under `tests/fixtures/typecheck/*.ridge` declares one or more
//! `-- expect: T###` directives.  [`all_fixtures_pass`] iterates the directory,
//! builds a synthetic single-module workspace per fixture, runs the full
//! resolve+typecheck pipeline, and asserts every expected code appears.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};
use tempfile::TempDir;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/typecheck");

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// Wrap a source string in a one-module synthetic workspace with FQN
/// `demo.<stem>`.
fn build_single_module_workspace(stem: &str, src: &str) -> TempDir {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/ridge.toml",
        "[project]\n\
         name = \"demo\"\n\
         version = \"0.1.0\"\n\
         kind = \"library\"\n\
         \n\
         [project.exports]\n\
         public = [\"**\"]\n",
    );
    write_file(td.path(), &format!("apps/demo/src/{stem}.ridge"), src);
    td
}

/// Run the full resolve+typecheck pipeline over the workspace at `td.path()`.
/// Returns the combined vector of T### errors (module attribution stripped —
/// tests care about the error code, not the source module).
fn run_typecheck_pipeline(td: &TempDir) -> Vec<TypeError> {
    let disc = discover_workspace(td.path());
    let Some(ws_graph) = disc.graph else {
        return Vec::new();
    };
    let resolved = resolve_workspace(ws_graph);
    // We deliberately ignore R-errors here — we're testing T-errors only.
    let result = typecheck_workspace(&resolved);
    result.errors.into_iter().map(|(_, e)| e).collect()
}

fn run_typecheck_on_source(stem: &str, src: &str) -> Vec<TypeError> {
    let td = build_single_module_workspace(stem, src);
    run_typecheck_pipeline(&td)
}

// ── `-- expect:` directive parser ─────────────────────────────────────────────

#[derive(Debug)]
struct ExpectLine {
    code: String,
}

fn parse_expects(src: &str) -> Vec<ExpectLine> {
    let mut out = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("--") {
            break;
        }
        let after_dashes = trimmed.trim_start_matches('-').trim();
        let Some(rest) = after_dashes.strip_prefix("expect:") else {
            continue;
        };
        let mut tokens = rest.split_whitespace();
        let Some(code) = tokens.next() else { continue };
        out.push(ExpectLine {
            code: code.to_uppercase(),
        });
    }
    out
}

// ── Fixture-driven test ───────────────────────────────────────────────────────

/// Iterate every `tests/fixtures/typecheck/*.ridge` file, run the typecheck
/// pipeline, and assert every `-- expect: T###` directive is satisfied.
///
/// `DoD` §9.2: ≥ 25 single-file fixtures; every reachable T### code must have
/// at least one fixture.
#[test]
fn all_fixtures_pass() {
    let dir = PathBuf::from(FIXTURE_DIR);
    assert!(
        dir.is_dir(),
        "fixture directory does not exist: {}",
        dir.display()
    );

    let mut entries: Vec<_> = fs::read_dir(&dir)
        .expect("read fixture dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "ridge"))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    let mut failures: Vec<String> = Vec::new();
    let mut count = 0usize;

    for entry in entries {
        let path = entry.path();
        let stem = path
            .file_stem()
            .expect("fixture stem")
            .to_string_lossy()
            .into_owned();
        let file_name = path
            .file_name()
            .expect("fixture filename")
            .to_string_lossy()
            .into_owned();

        let src = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));

        let expects = parse_expects(&src);
        if expects.is_empty() {
            failures.push(format!("{file_name}: no `-- expect:` directive"));
            continue;
        }
        count += 1;

        let errors = run_typecheck_on_source(&stem, &src);
        let actual_codes: Vec<&str> = errors.iter().map(TypeError::code).collect();

        for exp in &expects {
            let found = errors.iter().any(|e| e.code() == exp.code);
            if !found {
                failures.push(format!(
                    "{file_name}: expected {} but got codes {:?}",
                    exp.code, actual_codes
                ));
            }
        }
    }

    assert!(
        count >= 25,
        "DoD requires at least 25 single-file fixtures; got {count}"
    );
    assert!(
        failures.is_empty(),
        "fixture failures:\n  {}",
        failures.join("\n  ")
    );
}

/// Regression: an actor whose state field is `Handle <ActorB>` where
/// `<ActorB>` is declared LATER in the same source file must typecheck
/// without errors.  Before the two-pass `collect_user_tycons` refactor,
/// `ActorB` was not yet in the user-tycon name map when pass 2 resolved
/// `Handle ActorB`, so the field type fell through to a fresh `Type::Var`
/// and any `state.fieldB ! msg` later raised `T020 send on non-actor`
/// with the polymorphic stub type embedded in the message.
#[test]
fn forward_actor_type_reference_typechecks_cleanly() {
    let src = "\
actor Caller =\n\
    state target: Handle Receiver\n\
\n\
    init (r: Handle Receiver) =\n\
        target <- r\n\
\n\
    on poke =\n\
        target ! ping\n\
\n\
actor Receiver =\n\
    state count: Int = 0\n\
\n\
    on ping =\n\
        count <- count + 1\n\
";
    let errors = run_typecheck_on_source("forward_actor", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        !codes.contains(&"T020"),
        "forward-referenced actor handle must NOT raise T020; got: {codes:?}"
    );
    assert!(
        !codes.contains(&"T999"),
        "forward-referenced actor handle must NOT raise T999; got: {codes:?}"
    );
}

// ── Non-parametric type alias transparency ────────────────────────────────────

/// `type Bag = List Int` declares a non-parametric alias.  At use sites
/// (parameter annotations, return types) the alias must unify with the body
/// it stands for: `b: Bag` is interchangeable with `b: List Int` and a call
/// to `List.length b` must typecheck.
///
/// Before the wrap-as-`Type::Alias` fix in `ast_type_to_ridge_type`, the
/// alias interned as its own opaque `Type::Con(bag_id, [])` and never
/// unified with `List Int`, surfacing a confusing
/// `T001 expected #6 (?0), got #15` at every alias use site.
#[test]
fn non_parametric_alias_unifies_with_body() {
    let src = "import std.list as List\n\
type Bag = List Int\n\
\n\
pub fn lengthBag (b: Bag) -> Int = List.length b\n\
";
    let errors = run_typecheck_on_source("alias_bag", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "non-parametric alias `Bag = List Int` must typecheck cleanly; got: {codes:?}"
    );
}

/// A non-parametric alias for a parametric container (`Map`) must also
/// unify transparently with the body.  This is the exact dx-test paper-cut
/// from `mini-sql`, where `type Row = Map Text Text` had to be inlined.
#[test]
fn non_parametric_map_alias_unifies_with_body() {
    let src = "import std.map as Map\n\
type Row = Map Text Text\n\
\n\
pub fn empty () -> Row = Map.empty\n\
";
    let errors = run_typecheck_on_source("alias_row", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "non-parametric alias `Row = Map Text Text` must typecheck cleanly; got: {codes:?}"
    );
}

/// Multi-step alias chains: `type A = List Int; type B = A` must let
/// `B` unify with `List Int` even though the second alias references the
/// first.  Pass 2 builds `B`'s body before `ctx.tycon_decls` has been
/// synced from the arena, so without the dedicated chain-resolution pass
/// `B` lands as `Type::Con(A, [])` — an opaque dead end that no caller
/// can unify with `List Int`.
#[test]
fn multistep_alias_chain_unifies_with_terminal_body() {
    let src = "import std.list as List\n\
type IntList = List Int\n\
type Numbers = IntList\n\
\n\
pub fn lengthNumbers (ns: Numbers) -> Int = List.length ns\n\
";
    let errors = run_typecheck_on_source("alias_chain", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "multi-step alias chain `Numbers -> IntList -> List Int` must typecheck \
         cleanly; got: {codes:?}"
    );
}

/// Three-step chain (`A -> B -> C -> Map Text Text`) is the same fix
/// generalised: the dedicated pass recurses through every alias hop until
/// it lands on a non-alias body.
#[test]
fn three_step_alias_chain_unifies_with_terminal_body() {
    let src = "import std.map as Map\n\
type RowA = Map Text Text\n\
type RowB = RowA\n\
type RowC = RowB\n\
\n\
pub fn empty () -> RowC = Map.empty\n\
";
    let errors = run_typecheck_on_source("alias_chain3", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "three-step alias chain must typecheck cleanly; got: {codes:?}"
    );
}

/// Parametric alias: `type Stack a = List a` plus a use of `Stack Int`
/// must unify with `List Int`.  Before this fix, `TyConKind::Alias` did
/// not carry the alias's own type-parameter vids, so the use site fell
/// through to `Type::Con(Stack, [Int])` — an opaque dead end that never
/// unified with the body.
#[test]
fn parametric_alias_unifies_with_body() {
    let src = "import std.list as List\n\
type Stack a = List a\n\
\n\
pub fn lengthStack (s: Stack Int) -> Int = List.length s\n\
";
    let errors = run_typecheck_on_source("alias_stack", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "parametric alias `Stack Int` must unify with `List Int`; got: {codes:?}"
    );
}

/// Two-parameter parametric alias (`type Pair a b = (a, b)`) — the
/// substitution path must thread both params through the body in order.
#[test]
fn two_parameter_alias_unifies_with_body() {
    let src = "\
type Pair a b = (a, b)\n\
\n\
pub fn fst (p: Pair Int Text) -> Int =\n\
    let (a, _) = p\n\
    a\n\
";
    let errors = run_typecheck_on_source("alias_pair", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "two-parameter alias `Pair Int Text` must unify with `(Int, Text)`; got: {codes:?}"
    );
}

/// Parametric chain: `type Stack a = List a; type IntStack = Stack Int`
/// — the dedicated chain pass substitutes the inner alias's parameter
/// when chasing through, so `IntStack` lands directly on `List Int`.
#[test]
fn parametric_alias_chained_unifies_with_terminal_body() {
    let src = "import std.list as List\n\
type Stack a = List a\n\
type IntStack = Stack Int\n\
\n\
pub fn lengthIntStack (s: IntStack) -> Int = List.length s\n\
";
    let errors = run_typecheck_on_source("alias_intstack", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "parametric-then-instantiated alias chain must typecheck cleanly; got: {codes:?}"
    );
}

// ── Multi-parameter typeclasses (L7) ──────────────────────────────────────────

/// A two-parameter class with a concrete instance and a fully-determined call
/// site typechecks with no diagnostics: the constraint resolves against the
/// instance keyed by the `(Int, Bool)` head tuple.
#[test]
fn multi_param_class_and_instance_typecheck() {
    let src = "class Convert a b =\n    convert (x: a) -> b\n\ninstance Convert Int Bool =\n    convert (x: Int) -> Bool = true\n\nfn intToBool (n: Int) -> Bool = convert n\n";
    let errors = run_typecheck_on_source("mptc_happy", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "a 2-parameter class + matching instance + determined call must typecheck cleanly; got: {codes:?}"
    );
}

/// When a multi-parameter constraint leaves a head position undetermined, the
/// solver reports T030 — the user must annotate the open type. (Resolving it
/// automatically would require functional dependencies, deferred for now.)
#[test]
fn multi_param_undetermined_result_is_ambiguous() {
    let src = "class Convert a b =\n    convert (x: a) -> b\n\ninstance Convert Int Bool =\n    convert (x: Int) -> Bool = true\n\nfn amb (n: Int) -> Text =\n    let r = convert n\n    \"done\"\n";
    let errors = run_typecheck_on_source("mptc_ambiguous", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T030"),
        "an undetermined multi-parameter head position must be ambiguous (T030); got: {codes:?}"
    );
}

/// Two instances for the same head tuple `(Int, Bool)` violate coherence — T032,
/// the same single-value-per-key rule the instance registry enforces for
/// single-parameter classes, now over the head tuple.
#[test]
fn duplicate_multi_param_instance_is_overlapping() {
    let src = "class Convert a b =\n    convert (x: a) -> b\n\ninstance Convert Int Bool =\n    convert (x: Int) -> Bool = true\n\ninstance Convert Int Bool =\n    convert (x: Int) -> Bool = false\n";
    let errors = run_typecheck_on_source("mptc_overlap", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T032"),
        "two instances for the same head tuple must overlap (T032); got: {codes:?}"
    );
}

/// Distinct head tuples are distinct instances: `Convert Int Bool` and
/// `Convert Int Text` coexist without a coherence error.
#[test]
fn distinct_multi_param_head_tuples_coexist() {
    let src = "class Convert a b =\n    convert (x: a) -> b\n\ninstance Convert Int Bool =\n    convert (x: Int) -> Bool = true\n\ninstance Convert Int Text =\n    convert (x: Int) -> Text = \"n\"\n";
    let errors = run_typecheck_on_source("mptc_distinct", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        !codes.contains(&"T032"),
        "distinct head tuples must not collide; got: {codes:?}"
    );
}
