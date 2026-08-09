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

// ── Constructor misuse: no T999 leaks ─────────────────────────────────────────

/// A name that resolves to a type but is used as a constructor (the symptom of
/// a single-variant union written without its leading `|`, which parses as an
/// alias) must report the user-facing `T044`, never an internal `T999`.
#[test]
fn type_used_as_constructor_reports_t044_not_t999() {
    let src = "type Box = Box Int\n\npub fn make () -> Box = Box 42\n";
    let codes: Vec<&str> = run_typecheck_on_source("box_alias", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.contains(&"T044"),
        "expected T044 for a type used as a constructor; got: {codes:?}"
    );
    assert!(
        !codes.contains(&"T999"),
        "a type-as-constructor mistake must NOT leak T999; got: {codes:?}"
    );
}

/// A genuinely unknown constructor is the resolver's job (`R010`); type-check
/// must absorb it silently rather than piling on a `T999`.
#[test]
fn unknown_constructor_does_not_leak_t999() {
    let src = "type Boxed = MkBox Int\n\npub fn make () -> Boxed = MkBox 42\n";
    let codes: Vec<&str> = run_typecheck_on_source("boxed_unknown", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        !codes.contains(&"T999"),
        "an unresolved constructor must NOT leak T999 (R010 covers it); got: {codes:?}"
    );
}

/// A mistyped stdlib symbol (`Io.printn`) is reported by the resolver as
/// `R014` with did-you-mean suggestions; the type checker must absorb the
/// same name silently rather than cascading into a `T999` "compiler bug".
#[test]
fn unknown_stdlib_symbol_does_not_leak_t999() {
    let src = "import std.io as Io\n\npub fn io hi () -> Unit = Io.printn \"hi\"\n";
    let codes: Vec<&str> = run_typecheck_on_source("io_printn", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        !codes.contains(&"T999"),
        "an R014-reported unknown stdlib symbol must NOT leak T999; got: {codes:?}"
    );
}

/// Matching (and constructing) a record-style union variant type-checks: the
/// variant's fields bind against its inline record schema, the match is
/// exhaustive, and no deferral diagnostic (`T044`) or internal error (`T999`)
/// is emitted.
#[test]
fn record_style_variant_pattern_type_checks() {
    let src = "type Msg = Ping | Move { dx: Int, dy: Int }\n\n\
               pub fn step (m: Msg) -> Int =\n\
               \x20   match m\n\
               \x20       Ping -> 0\n\
               \x20       Move { dx, dy } -> dx + dy\n";
    let codes: Vec<&str> = run_typecheck_on_source("record_variant", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.is_empty(),
        "a record-style variant pattern should type-check cleanly; got: {codes:?}"
    );
}

/// Constructing a record-style union variant type-checks: the field initialisers
/// are validated against the variant's inline record schema and the result is
/// the owner union type.
#[test]
fn record_style_variant_construction_type_checks() {
    let src = "type Event = Tick | Login { userId: Int, at: Int }\n\n\
               pub fn mk () -> Event = Login { userId = 7, at = 1000 }\n";
    let codes: Vec<&str> = run_typecheck_on_source("record_variant_ctor", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.is_empty(),
        "constructing a record-style variant should type-check cleanly; got: {codes:?}"
    );
}

/// Regression: an exhaustive match over record-payload variants must not flag a
/// sibling arm as redundant (`T017`). An earlier record-body pattern covers only
/// its own variant, not the whole union, so later variants stay reachable.
#[test]
fn record_variant_match_not_falsely_redundant() {
    let src = "type Event = Login { userId: Int } | Logout { userId: Int } | Tick\n\n\
               pub fn describe (e: Event) -> Int =\n\
               \x20   match e\n\
               \x20       Login { userId } -> userId\n\
               \x20       Logout { userId } -> userId\n\
               \x20       Tick -> 0\n";
    let codes: Vec<&str> = run_typecheck_on_source("event_exhaustive", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        !codes.contains(&"T016") && !codes.contains(&"T017"),
        "exhaustive record-variant match must not warn redundant/non-exhaustive; got: {codes:?}"
    );
}

/// A match that omits a variant is still non-exhaustive (`T016`), even when the
/// present arms are record-style variant patterns.
#[test]
fn record_variant_match_non_exhaustive_reports_t016() {
    let src = "type Event = Login { userId: Int } | Logout { userId: Int } | Tick\n\n\
               pub fn describe (e: Event) -> Int =\n\
               \x20   match e\n\
               \x20       Login { userId } -> userId\n\
               \x20       Tick -> 0\n";
    let codes: Vec<&str> = run_typecheck_on_source("event_missing", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.contains(&"T016"),
        "a match missing a record-payload variant must report T016; got: {codes:?}"
    );
}

/// A generic union with a record-payload variant type-checks: the field type `a`
/// unifies with the union's type parameter at construction and in patterns.
#[test]
fn generic_record_variant_type_checks() {
    let src = "type Box a = | Wrap { val: a }\n\n\
               pub fn unwrap (b: Box Int) -> Int =\n\
               \x20   match b\n\
               \x20       Wrap { val } -> val\n\n\
               pub fn make () -> Box Int = Wrap { val = 7 }\n";
    let codes: Vec<&str> = run_typecheck_on_source("box_generic", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.is_empty(),
        "a generic record-payload variant should type-check cleanly; got: {codes:?}"
    );
}

// ── Instances over function types (L8 / P1) ───────────────────────────────────

/// A class whose instance head is a FUNCTION TYPE (`instance Run (Int -> Int)`)
/// must resolve when a bare function is used where the class is required. The
/// constraint `Run (Int -> Int)` keys on the synthetic per-arity `Fn/1`
/// constructor; a regression would surface `T029 NoInstance` (the function type
/// fell through the dispatcher to the `_` wildcard) or an internal `T999`.
#[test]
fn function_type_instance_resolves_for_bare_fn() {
    let src = "\
class Run f =\n\
\x20   run (self: f) (x: Int) -> Int\n\
\n\
instance Run (Int -> Int) =\n\
\x20   run (g: Int -> Int) (x: Int) -> Int = g x\n\
\n\
pub fn callIt () -> Int =\n\
\x20   run (fn (x: Int) -> Int = x + 1) 41\n\
";
    let errors = run_typecheck_on_source("fn_instance", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        !codes.contains(&"T029"),
        "a function-type instance must resolve for a bare fn (no NoInstance); got: {codes:?}"
    );
    assert!(
        !codes.contains(&"T999"),
        "a function-type instance must NOT leak an internal T999; got: {codes:?}"
    );
    assert!(
        codes.is_empty(),
        "the function-type-instance program must typecheck cleanly; got: {codes:?}"
    );
}

/// A function-type instance head whose arity exceeds the reserved
/// `Fn/0..Fn/15` dispatch-key block is rejected at the declaration with
/// `T051`, not silently dropped (which previously surfaced as a confusing
/// `T029 NoInstance` at use sites, or nothing at all when unused).
#[test]
fn function_type_instance_head_arity_over_limit_is_t051() {
    let src = "\
class Run f =\n\
\x20   run (self: f) (x: Int) -> Int\n\
\n\
instance Run (fn a b c d e f g h i j k l m n o p -> Int) =\n\
\x20   run (g: fn a b c d e f g h i j k l m n o p -> Int) (x: Int) -> Int = x\n\
";
    let errors = run_typecheck_on_source("fn_instance_arity_over", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T051"),
        "a 16-ary function-type instance head must be rejected with T051; got: {codes:?}"
    );
    // The boundary just under the limit must still collect fine.
    let ok_src = "\
class Run f =\n\
\x20   run (self: f) (x: Int) -> Int\n\
\n\
instance Run (fn a b c d e f g h i j k l m n o -> Int) =\n\
\x20   run (g: fn a b c d e f g h i j k l m n o -> Int) (x: Int) -> Int = x\n\
";
    let ok_errors = run_typecheck_on_source("fn_instance_arity_at", ok_src);
    let ok_codes: Vec<&str> = ok_errors.iter().map(TypeError::code).collect();
    assert!(
        !ok_codes.contains(&"T051"),
        "a 15-ary function-type instance head is inside the dispatch-key block; got: {ok_codes:?}"
    );
}

/// A polymorphic, constrained consumer (`useRun … where Run a`) forwards its
/// retained `Run a` constraint; at the concrete call site the constraint pins
/// `a` to a function type and discharges to the `Fn/1` instance. Guards the
/// retained/forward path in addition to the direct one above.
#[test]
fn function_type_instance_resolves_through_constrained_consumer() {
    let src = "\
class Run f =\n\
\x20   run (self: f) (x: Int) -> Int\n\
\n\
instance Run (Int -> Int) =\n\
\x20   run (g: Int -> Int) (x: Int) -> Int = g x\n\
\n\
fn useRun (f: a) (n: Int) -> Int where Run a =\n\
\x20   run f n\n\
\n\
pub fn callIt () -> Int =\n\
\x20   useRun (fn (x: Int) -> Int = x + 1) 41\n\
";
    let errors = run_typecheck_on_source("fn_instance_fwd", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "constrained-consumer function instance must typecheck cleanly; got: {codes:?}"
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

// ── Quotation (L6) ────────────────────────────────────────────────────────────

/// A predicate over real columns, with a comparison and a boolean column joined
/// by `&&`, type-checks cleanly: the lambda is captured against `User`'s columns
/// rather than checked as an ordinary function.
#[test]
fn quoted_predicate_typechecks() {
    let src = "type User = { age: Int, active: Bool }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo () -> Bool = pred (fn u -> u.age >= 18 && u.active)\n";
    let errors = run_typecheck_on_source("quote_happy", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "a quoted predicate over real columns must typecheck cleanly; got: {codes:?}"
    );
}

/// Referencing a field that is not a column of the entity is a compile error
/// (T039), not wrong SQL at runtime.
#[test]
fn quoted_unknown_column_is_rejected() {
    let src = "type User = { age: Int, active: Bool }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo () -> Bool = pred (fn u -> u.salary >= 18)\n";
    let errors = run_typecheck_on_source("quote_unknown_col", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T039"),
        "an unknown column in a quoted predicate must be T039; got: {codes:?}"
    );
}

/// Comparing a column with a literal of a different type is rejected (T041).
#[test]
fn quoted_comparison_type_mismatch_is_rejected() {
    let src = "type User = { age: Int, active: Bool }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo () -> Bool = pred (fn u -> u.age >= \"old\")\n";
    let errors = run_typecheck_on_source("quote_cmp_mismatch", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T041"),
        "a mismatched comparison in a quoted predicate must be T041; got: {codes:?}"
    );
}

/// A quoted body that is not boolean — here a bare integer column — is rejected
/// (T040): a predicate must evaluate to a boolean.
#[test]
fn quoted_non_boolean_body_is_rejected() {
    let src = "type User = { age: Int, active: Bool }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo () -> Bool = pred (fn u -> u.age)\n";
    let errors = run_typecheck_on_source("quote_non_bool", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T040"),
        "a non-boolean quoted predicate body must be T040; got: {codes:?}"
    );
}

/// A quote may capture a base scalar from the enclosing scope; it lowers to a
/// query parameter rather than forcing the value to be inlined. A predicate that
/// compares columns against captured Int, Bool, Text, and Float values type-checks
/// cleanly — covering every scalar a `QLit*` node can bind.
#[test]
fn quoted_captured_scalars_typecheck() {
    let src = "type User = { age: Int, active: Bool, name: Text, score: Float }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (minAge: Int) (flag: Bool) (wanted: Text) (cut: Float) -> Bool = pred (fn u -> u.age >= minAge && u.active == flag && u.name == wanted && u.score >= cut)\n";
    let errors = run_typecheck_on_source("quote_capture_ok", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "capturing scalar values into a quote must typecheck cleanly; got: {codes:?}"
    );
}

/// Only base scalars can be captured. A captured value of a non-scalar type
/// (here a record) is rejected (T040): there is no single query parameter to bind
/// it to.
#[test]
fn quoted_captured_non_scalar_is_rejected() {
    let src = "type User = { age: Int }\ntype Box = { n: Int }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (b: Box) -> Bool = pred (fn u -> u.age >= b)\n";
    let errors = run_typecheck_on_source("quote_capture_bad", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T040"),
        "capturing a non-scalar value into a quote must be T040; got: {codes:?}"
    );
}

/// A quote may compare a column against a *field* of a value captured from the
/// enclosing scope (`u.id == link.id`), not only a bound scalar local. The field's
/// value binds as a query parameter, so the natural "target the row I just fetched"
/// shape — reading `link.id` inline — typechecks cleanly.
#[test]
fn quoted_captured_field_access_typechecks() {
    let src = "type User = { id: Int, name: Text }\ntype Link = { id: Int }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (link: Link) -> Bool = pred (fn u -> u.id == link.id)\n";
    let errors = run_typecheck_on_source("quote_field_capture_ok", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "capturing a record field into a quote must typecheck cleanly; got: {codes:?}"
    );
}

/// The captured field must itself be a base scalar. A record-typed field read into a
/// quote is rejected (T040): there is no single value to bind for it.
#[test]
fn quoted_captured_field_access_non_scalar_is_rejected() {
    let src = "type User = { id: Int }\ntype Inner = { n: Int }\ntype Wrap = { inner: Inner }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (w: Wrap) -> Bool = pred (fn u -> u.id == w.inner)\n";
    let errors = run_typecheck_on_source("quote_field_capture_nonscalar", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T040"),
        "capturing a non-scalar record field must be T040; got: {codes:?}"
    );
}

/// A text-match pattern may be a Text value captured from the enclosing scope
/// (`startsWith u.name prefix`), not only a literal — a search term taken as a
/// parameter binds at run time, its wildcards escaped like a literal's.
#[test]
fn quoted_captured_text_pattern_typechecks() {
    let src = "type User = { name: Text }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (prefix: Text) -> Bool = pred (fn u -> Text.startsWith u.name prefix)\n";
    let errors = run_typecheck_on_source("quote_text_pattern_ok", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "capturing a Text pattern into a text match must typecheck cleanly; got: {codes:?}"
    );
}

/// A captured text-match pattern must be Text. A captured Int pattern is rejected
/// (T040): a LIKE pattern is text, so a non-text value has nothing to match.
#[test]
fn quoted_captured_non_text_pattern_is_rejected() {
    let src = "type User = { name: Text }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (n: Int) -> Bool = pred (fn u -> Text.startsWith u.name n)\n";
    let errors = run_typecheck_on_source("quote_text_pattern_bad", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T040"),
        "a captured non-Text text-match pattern must be T040; got: {codes:?}"
    );
}

/// A captured `List <scalar>` is a runtime `IN` list: `List.contains u.age ages`
/// with `ages: List Int` typechecks cleanly, the parity of `ages.Contains(u.Age)`.
#[test]
fn quoted_captured_in_list_typecheck() {
    let src = "type User = { age: Int, name: Text }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (ages: List Int) (names: List Text) -> Bool = pred (fn u -> List.contains u.age ages && List.contains u.name names)\n";
    let errors = run_typecheck_on_source("quote_in_capture_ok", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "capturing a scalar list as an `IN` test must typecheck cleanly; got: {codes:?}"
    );
}

/// A captured `IN` list must hold base scalars. A `List` of a record type is
/// rejected (T040): a record has no single column value to bind per element.
#[test]
fn quoted_captured_in_list_non_scalar_is_rejected() {
    let src = "type User = { age: Int }\ntype Box = { n: Int }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (boxes: List Box) -> Bool = pred (fn u -> List.contains u.age boxes)\n";
    let errors = run_typecheck_on_source("quote_in_capture_nonscalar", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T040"),
        "capturing a non-scalar `IN` list must be T040; got: {codes:?}"
    );
}

/// A correlated `exists` over a captured repository typechecks cleanly: the inner
/// row binds against the repo's entity and the predicate correlates it to the outer
/// row — the parity of `db.Posts.Any(p => p.AuthorId == u.Id)`.
#[test]
fn quoted_exists_typecheck() {
    let src = "import std.repo as Repo\n\ntype User = { id: Int }\ntype Post = { author: Int }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (posts: Repo Post a) -> Bool = pred (fn u -> Repo.exists posts (fn (p: Post) -> p.author == u.id))\n";
    let errors = run_typecheck_on_source("quote_exists_ok", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.is_empty(),
        "a correlated exists over a captured repo must typecheck cleanly; got: {codes:?}"
    );
}

/// The inner table of `exists` must be a `Repo`. A captured value of any other type
/// is rejected (T040): there is no table to probe.
#[test]
fn quoted_exists_non_repo_is_rejected() {
    let src = "import std.repo as Repo\n\ntype User = { id: Int }\ntype Post = { author: Int }\ntype Box = { n: Int }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (b: Box) -> Bool = pred (fn u -> Repo.exists b (fn (p: Post) -> p.author == u.id))\n";
    let errors = run_typecheck_on_source("quote_exists_non_repo", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T040"),
        "an exists over a non-repo captured value must be T040; got: {codes:?}"
    );
}

/// A correlated predicate that compares mismatched column types is rejected (T041),
/// the same way an ordinary quoted comparison is — the inner and outer columns must
/// line up.
#[test]
fn quoted_exists_type_mismatch_is_rejected() {
    let src = "import std.repo as Repo\n\ntype User = { id: Int, name: Text }\ntype Post = { author: Int }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (posts: Repo Post a) -> Bool = pred (fn u -> Repo.exists posts (fn (p: Post) -> p.author == u.name))\n";
    let errors = run_typecheck_on_source("quote_exists_mismatch", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T041"),
        "a correlated comparison of mismatched types must be T041; got: {codes:?}"
    );
}

/// The element type of a captured `IN` list must match the column. A `List Text`
/// tested against an `Int` column is a comparison mismatch (T041).
#[test]
fn quoted_captured_in_list_type_mismatch_is_rejected() {
    let src = "type User = { age: Int }\n\nfn pred (q: Quote (User -> Bool)) -> Bool = true\n\nfn demo (names: List Text) -> Bool = pred (fn u -> List.contains u.age names)\n";
    let errors = run_typecheck_on_source("quote_in_capture_mismatch", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T041"),
        "a captured `IN` list whose element type differs from the column must be T041; got: {codes:?}"
    );
}

// ── T001 message rendering: real type names, never `#N` ───────────────────────

/// A discarded expression's type must render with user-facing names, never the
/// internal `Debug` dump (`Con(TyConId(6), [Var(TyVid(103))])`). Discarding a
/// polymorphic value (`List.empty`) exercises the unresolved-variable arm.
#[test]
fn discarded_result_renders_user_facing_type() {
    let src = "import std.list as List\n\npub fn f () -> Int =\n    List.empty\n    0\n";
    let errors = run_typecheck_on_source("discard_list_empty", src);
    let t022 = errors
        .iter()
        .find_map(|e| match e {
            TypeError::DiscardedResult { ty, .. } => Some(ty.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a T022 DiscardedResult; got: {errors:?}"));
    assert!(
        !t022.contains("TyVid") && !t022.contains("Con(") && !t022.contains('#'),
        "T022 must not leak internal type representations; got {t022:?}"
    );
    assert!(
        t022.contains("List"),
        "T022 should name the discarded type as `List ...`; got {t022:?}"
    );
}

/// Field access on a non-record (`xs.length` on `List Int`) is a `T054` that
/// speaks of field access — not the `T006` "`with` on non-record" the user
/// never wrote — renders the base type by name, and suggests the module
/// function (`List.length`) when the type's constructor shares its name with
/// a stdlib module exporting that function.
#[test]
fn field_access_on_non_record_is_t054_with_module_suggestion() {
    let src = "pub fn f (xs: List Int) -> Int = xs.length\n";
    let errors = run_typecheck_on_source("list_dot_length", src);
    let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
    assert!(
        codes.contains(&"T054"),
        "expected T054 for field access on a non-record; got: {codes:?}"
    );
    assert!(
        !codes.contains(&"T006"),
        "T006 is for `with`-updates; the user wrote no `with`; got: {codes:?}"
    );
    let t054 = errors
        .iter()
        .find_map(|e| match e {
            TypeError::FieldAccessOnNonRecord {
                ty,
                field,
                suggestion,
                ..
            } => Some((ty.clone(), field.clone(), suggestion.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no T054 produced; got: {errors:?}"));
    assert_eq!(t054.0, "List Int", "base type renders by name");
    assert_eq!(t054.1, "length");
    assert_eq!(
        t054.2.as_deref(),
        Some("List.length"),
        "should suggest the module function"
    );
}

/// Field access on a non-record whose type has no same-named module function
/// still reports `T054`, just without a suggestion.
#[test]
fn field_access_on_int_is_t054_without_suggestion() {
    let src = "pub fn f (x: Int) -> Int = x.length\n";
    let errors = run_typecheck_on_source("int_dot_length", src);
    let t054 = errors
        .iter()
        .find_map(|e| match e {
            TypeError::FieldAccessOnNonRecord { ty, suggestion, .. } => {
                Some((ty.clone(), suggestion.clone()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no T054 produced; got: {errors:?}"));
    assert_eq!(t054.0, "Int", "base type renders by name, not `#0`");
    assert_eq!(t054.1, None, "no `Int.length` exists — no suggestion");
}

/// A genuine `with`-update on a non-record keeps `T006`, but the found type
/// renders by name rather than as raw arena ids.
#[test]
fn with_on_non_record_renders_user_facing_type() {
    let src = "pub fn f (xs: List Int) -> List Int = xs with { length = 3 }\n";
    let errors = run_typecheck_on_source("with_on_list", src);
    let t006 = errors
        .iter()
        .find_map(|e| match e {
            TypeError::WithOnNonRecord { ty, .. } => Some(ty.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a T006 WithOnNonRecord; got: {errors:?}"));
    assert_eq!(
        t006, "List Int",
        "T006 must not leak `#6 (#0)`; got {t006:?}"
    );
}

/// Pull the `(expected, found)` strings of the first `T001 TypeMismatch`.
fn first_mismatch(stem: &str, src: &str) -> (String, String) {
    run_typecheck_on_source(stem, src)
        .into_iter()
        .find_map(|e| match e {
            TypeError::TypeMismatch {
                expected, found, ..
            } => Some((expected, found)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no T001 produced for {stem}"))
}

/// A type mismatch renders both sides by their declared names, not the
/// arena-free `#N` placeholder nor a Debug `Con(TyConId(..))` dump. Covers
/// both the return-vs-body path (built in `scc`) and the unify path
/// (`unify::mismatch`).
#[test]
fn type_mismatch_renders_real_type_names() {
    // Return type vs body — constructed in `scc`.
    let (expected, found) = first_mismatch("mismatch_ret", "pub fn f () -> Text = 5\n");
    assert_eq!(expected, "Text", "expected side; got {expected:?}");
    assert_eq!(found, "Int", "found side; got {found:?}");

    // Annotation vs value inside an expression — flows through `unify::mismatch`.
    let (expected, found) = first_mismatch(
        "mismatch_let",
        "pub fn g () -> Int =\n    let x: Text = 5\n    0\n",
    );
    for side in [&expected, &found] {
        assert!(
            !side.contains('#') && !side.contains("Con(") && !side.contains("TyConId"),
            "type names must be readable, not `#N`/Debug; got {side:?}"
        );
    }
    assert!(
        expected == "Text" || found == "Text",
        "one side must name `Text`; got expected={expected:?} found={found:?}"
    );
}

// ── Issue #377 — syntax-trap teaching hints on T001 ───────────────────────────

/// Render the first `T001 TypeMismatch` to its `Display` text.
fn first_t001_display(stem: &str, src: &str) -> String {
    run_typecheck_on_source(stem, src)
        .into_iter()
        .find(|e| e.code() == "T001")
        .map_or_else(|| panic!("no T001 produced for {stem}"), |e| e.to_string())
}

/// Render the first mismatch of either kind. A call-site argument reports as
/// `T002` and everything else as `T001`; a test about the hint on a mismatch
/// does not care which of the two arrived.
fn first_mismatch_display(stem: &str, src: &str) -> String {
    run_typecheck_on_source(stem, src)
        .into_iter()
        .find(|e| matches!(e.code(), "T001" | "T002"))
        .map_or_else(
            || panic!("no mismatch produced for {stem}"),
            |e| e.to_string(),
        )
}

/// Trap A: `add(1, 2)` is parsed as applying `add` to the tuple `(1, 2)`.
/// The mismatch must carry a hint teaching the space-separated call shape.
#[test]
fn paren_comma_call_gets_space_separated_hint() {
    let src = "\
pub fn add (a: Int) (b: Int) -> Int = a + b
pub fn f () -> Int = add(1, 2)
";
    let t001 = first_t001_display("trap_paren_call", src);
    assert!(
        t001.contains("hint:"),
        "T001 must carry a hint for the `add(1, 2)` trap; got:\n{t001}"
    );
    assert!(
        t001.contains("space-separated"),
        "hint must teach that Ridge calls are space-separated; got:\n{t001}"
    );
    assert!(
        t001.contains("add 1 2"),
        "hint must show the correct shape `add 1 2`; got:\n{t001}"
    );
}

/// Trap A negative control: `add (1, 2)` — a space before the parens — is a
/// deliberate (if ill-typed) tuple argument, so no call-shape hint.
#[test]
fn spaced_tuple_argument_gets_no_call_hint() {
    let src = "\
pub fn add (a: Int) (b: Int) -> Int = a + b
pub fn f () -> Int = add (1, 2)
";
    let rendered = first_mismatch_display("trap_spaced_tuple", src);
    assert!(
        !rendered.contains("space-separated"),
        "a spaced tuple argument must NOT gain the call-shape hint; got:\n{rendered}"
    );
}

/// Trap A control: passing a tuple to a function that takes a tuple
/// typechecks — no error at all, with or without a space before the parens.
#[test]
fn correct_tuple_call_produces_no_error() {
    for (stem, call) in [
        ("trap_ok_tuple_adjacent", "g(1, 2)"),
        ("trap_ok_tuple_spaced", "g (1, 2)"),
    ] {
        let src = format!("pub fn g (p: (Int, Int)) -> Int = 0\npub fn f () -> Int = {call}\n");
        let errors = run_typecheck_on_source(stem, &src);
        let codes: Vec<&str> = errors.iter().map(TypeError::code).collect();
        assert!(
            codes.is_empty(),
            "a correct tuple-argument call (`{call}`) must typecheck cleanly; got: {codes:?}"
        );
    }
}

/// Trap B: an anonymous record literal where a named record type is expected
/// must teach that record literals name their constructor.
#[test]
fn anonymous_record_for_nominal_gets_constructor_hint() {
    let src = "\
type User = { name: Text }
pub fn f () -> User = { name = \"a\" }
";
    let t001 = first_t001_display("trap_anon_record", src);
    assert!(
        t001.contains("hint:"),
        "T001 must carry a hint for the anonymous-record trap; got:\n{t001}"
    );
    assert!(
        t001.contains("constructor"),
        "hint must teach that record literals name their constructor; got:\n{t001}"
    );
    assert!(
        t001.contains("User {"),
        "hint must show the correct shape `User {{ … }}`; got:\n{t001}"
    );
}

/// Trap B negative control: a structural-to-structural record mismatch (the
/// expected side has no constructor name) stays hint-free.
#[test]
fn structural_record_mismatch_gets_no_constructor_hint() {
    let src = "\
pub fn f () -> { name: Int } = { name = \"a\" }
";
    let t001 = first_t001_display("trap_structural_record", src);
    assert!(
        !t001.contains("constructor"),
        "a structural-record mismatch must NOT gain the constructor hint; got:\n{t001}"
    );
}

// ── T053 hint completeness ────────────────────────────────────────────────────

/// The hint on `T053` names `Cli.args ()`, which is `env`-gated. A reader who
/// follows it without being told that lands in `T014`, then `R016` for the
/// manifest — three steps the message has to own, or it sends people down a
/// path that does not compile.
#[test]
fn main_with_params_hint_names_the_capability() {
    let errors = run_typecheck_on_source("entry", "pub fn main (x: Int) -> Int =\n  x\n");
    let rendered = errors
        .iter()
        .find(|e| matches!(e, TypeError::MainHasParams { .. }))
        .map(ToString::to_string)
        .expect("T053 reported");

    assert!(rendered.contains("Cli.args"), "{rendered}");
    assert!(
        rendered.contains("fn {env} main"),
        "hint must show the capability on the signature: {rendered}"
    );
    assert!(
        rendered.contains("allow"),
        "hint must mention the manifest allow-list: {rendered}"
    );
}

// ── Record field patterns in exhaustiveness ───────────────────────────────────

/// A record pattern is refutable through its field patterns. Treating any
/// record pattern as matching every value of its type made a match that misses
/// a case look complete, so the program passed `check` and died at runtime.
#[test]
fn record_field_pattern_makes_the_match_non_exhaustive() {
    let src = "type Role = Admin | Guest\n\
               type User = { role: Role }\n\n\
               pub fn describe (u: User) -> Text =\n\
               \x20   match u\n\
               \x20       User { role = Admin } -> \"admin\"\n";
    let codes: Vec<&str> = run_typecheck_on_source("record_field_gap", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.contains(&"T016"),
        "a record pattern that tests a field cannot cover the whole type; got: {codes:?}"
    );
}

/// The other half of the same bug: the fallback arm that made the match total
/// was reported redundant, so following the advice deleted the only arm a
/// `Guest` user could reach.
#[test]
fn fallback_after_record_field_pattern_is_not_redundant() {
    let src = "type Role = Admin | Guest\n\
               type User = { role: Role }\n\n\
               pub fn describe (u: User) -> Text =\n\
               \x20   match u\n\
               \x20       User { role = Admin } -> \"admin\"\n\
               \x20       _ -> \"other\"\n";
    let codes: Vec<&str> = run_typecheck_on_source("record_field_fallback", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        !codes.contains(&"T016") && !codes.contains(&"T017"),
        "the fallback is the only arm a Guest reaches; got: {codes:?}"
    );
}

/// Covering every value of the field covers the record, with no fallback.
#[test]
fn record_field_patterns_can_be_exhaustive_on_their_own() {
    let src = "type Role = Admin | Guest\n\
               type User = { role: Role }\n\n\
               pub fn describe (u: User) -> Text =\n\
               \x20   match u\n\
               \x20       User { role = Admin } -> \"admin\"\n\
               \x20       User { role = Guest } -> \"guest\"\n";
    let codes: Vec<&str> = run_typecheck_on_source("record_field_total", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.is_empty(),
        "both roles are covered, so the match is total; got: {codes:?}"
    );
}

/// A field pattern that only binds — the D053 shorthand, or `..` — still covers
/// the whole record, the way it always did.
#[test]
fn binding_only_record_patterns_stay_irrefutable() {
    let src = "type Role = Admin | Guest\n\
               type User = { role: Role, age: Int }\n\n\
               pub fn describe (u: User) -> Int =\n\
               \x20   match u\n\
               \x20       User { role, age } -> age\n\n\
               pub fn other (u: User) -> Int =\n\
               \x20   match u\n\
               \x20       User { .. } -> 0\n";
    let codes: Vec<&str> = run_typecheck_on_source("record_bindings_only", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.is_empty(),
        "a record pattern that only binds covers its type; got: {codes:?}"
    );
}

/// The constructor-less form has no name to resolve, so it reaches its fields
/// only through the scrutinee's type.
#[test]
fn inline_record_pattern_is_checked_too() {
    let src = "type Role = Admin | Guest\n\
               type User = { role: Role }\n\n\
               pub fn describe (u: User) -> Text =\n\
               \x20   match u\n\
               \x20       { role = Admin } -> \"admin\"\n";
    let codes: Vec<&str> = run_typecheck_on_source("inline_record_gap", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.contains(&"T016"),
        "an inline record pattern testing a field is refutable too; got: {codes:?}"
    );
}

/// Nested records recurse through the same path.
#[test]
fn nested_record_field_pattern_is_checked() {
    let src = "type Role = Admin | Guest\n\
               type Inner = { role: Role }\n\
               type Outer = { inner: Inner }\n\n\
               pub fn describe (o: Outer) -> Text =\n\
               \x20   match o\n\
               \x20       Outer { inner = Inner { role = Admin } } -> \"admin\"\n";
    let codes: Vec<&str> = run_typecheck_on_source("nested_record_gap", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.contains(&"T016"),
        "the gap is one level down but it is still a gap; got: {codes:?}"
    );
}

/// The interior of a record-payload union variant is refutable in exactly the
/// same way, and used to be discarded for exactly the same reason.
#[test]
fn record_variant_interior_is_checked() {
    let src = "type Event = Login { userId: Int } | Tick\n\n\
               pub fn describe (e: Event) -> Int =\n\
               \x20   match e\n\
               \x20       Login { userId = 0 } -> 1\n\
               \x20       Tick -> 0\n";
    let codes: Vec<&str> = run_typecheck_on_source("variant_interior_gap", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.contains(&"T016"),
        "a tested userId does not cover every Login; got: {codes:?}"
    );
}

/// A refutable record pattern in a parameter binder is a `T043` — a function is
/// applied to every value of its type, so the pattern cannot be allowed to fail.
#[test]
fn refutable_record_parameter_reports_t043() {
    let src = "type Role = Admin | Guest\n\
               type User = { role: Role }\n\n\
               pub fn describe (User { role = Admin }: User) -> Text = \"admin\"\n";
    let codes: Vec<&str> = run_typecheck_on_source("refutable_record_param", src)
        .iter()
        .map(TypeError::code)
        .collect();
    assert!(
        codes.contains(&"T043"),
        "a parameter pattern that tests a field is refutable; got: {codes:?}"
    );
}

/// The witness has to be a value the reader can paste back into an arm: record
/// style rather than `User _`, and parenthesised where it sits as an argument.
#[test]
fn record_witness_renders_as_a_constructible_pattern() {
    let src = "type Role = Admin | Guest\n\
               type User = { role: Role }\n\
               type Wrap = W User | Z\n\n\
               pub fn describe (w: Wrap) -> Text =\n\
               \x20   match w\n\
               \x20       W (User { role = Admin }) -> \"admin\"\n\
               \x20       Z -> \"z\"\n";
    let rendered = run_typecheck_on_source("record_witness", src)
        .iter()
        .find(|e| matches!(e, TypeError::NonExhaustiveMatch { .. }))
        .map(ToString::to_string)
        .expect("T016 reported");
    assert!(
        rendered.contains("W (User { role = Guest })"),
        "witness must parse as written: {rendered}"
    );
}

// ── T029 names the reader's type ──────────────────────────────────────────────

/// Pull the `(ty, fix_hint)` of the single `NoInstance` in `errors`.
fn only_no_instance(errors: &[TypeError]) -> (String, String) {
    let mut found = errors.iter().filter_map(|e| match e {
        TypeError::NoInstance { ty, fix_hint, .. } => Some((ty.clone(), fix_hint.clone())),
        _ => None,
    });
    let first = found.next().expect("expected one T029");
    assert!(found.next().is_none(), "expected exactly one T029");
    first
}

/// Interpolating a value whose type has no `ToText` instance must name that
/// type. It used to render the raw `Type` through `Display`, which has no
/// access to the `TyCon` table and printed the constructor's numeric id — so
/// the reader was told an instance was missing and never told for what.
#[test]
fn t029_names_the_type_rather_than_its_id() {
    let src = "\
type Colour = Red | Green | Blue\n\
\n\
pub fn describe () -> Text =\n\
\x20   let c = Red\n\
\x20   $\"colour: ${c}\"\n\
";
    let (ty, hint) = only_no_instance(&run_typecheck_on_source("t029_name", src));
    assert_eq!(ty, "Colour");
    assert!(
        !ty.contains('#') && !hint.contains('#'),
        "no internal id may reach the reader: ty={ty}, hint={hint}"
    );
}

/// The hint has to name the type too. It used to read "add `instance ToText T`",
/// where `T` was a literal from the message template rather than anything in
/// the program.
#[test]
fn t029_hint_names_the_type_for_a_type_the_reader_owns() {
    let src = "\
type Colour = Red | Green | Blue\n\
\n\
pub fn describe () -> Text =\n\
\x20   let c = Red\n\
\x20   $\"colour: ${c}\"\n\
";
    let (_, hint) = only_no_instance(&run_typecheck_on_source("t029_hint", src));
    assert!(hint.contains("Colour"), "hint must name the type: {hint}");
    assert!(
        hint.contains("deriving (ToText)") && hint.contains("instance ToText Colour"),
        "both fixes are open for a type the reader declares: {hint}"
    );
}

/// A type declared outside the workspace takes neither fix: there is no
/// declaration of the reader's to carry a `deriving` clause, and an instance
/// written here is an orphan. Offering them anyway sends the reader to a second
/// error, so the hint has to say something they can act on instead.
#[test]
fn t029_hint_does_not_offer_a_fix_the_orphan_rule_refuses() {
    let src = "\
import std.fs as Fs\n\
\n\
pub fn fs report () -> Result Text Text =\n\
\x20   match Fs.readFile \"x.txt\"\n\
\x20       Err e -> Err $\"failed: ${e}\"\n\
\x20       Ok raw -> Ok raw\n\
";
    let (ty, hint) = only_no_instance(&run_typecheck_on_source("t029_orphan", src));
    assert_eq!(ty, "Error");
    assert!(
        !hint.contains("deriving (ToText)"),
        "`deriving` is not available on a type the reader does not declare: {hint}"
    );
    assert!(
        hint.contains("orphan"),
        "the hint must say why the obvious fix is refused: {hint}"
    );
}

/// T029 also comes out of the constraint solver, not just an interpolation hole,
/// and that path had its own copy of the same defect: it printed the raw
/// `TyConId`. The interpolation fix left it untouched, so a plain failed dispatch
/// still named no type.
#[test]
fn t029_from_the_solver_names_the_type_rather_than_its_id() {
    let src = "\
type Colour = Red | Green | Blue\n\
\n\
class Sizeable a =\n\
\x20   size (x: a) -> Int\n\
\n\
pub fn measure () -> Int =\n\
\x20   size Red\n\
";
    let (ty, hint) = only_no_instance(&run_typecheck_on_source("t029_solver", src));
    assert_eq!(ty, "Colour");
    assert!(
        !ty.contains('#') && !ty.contains("TyConId"),
        "no internal id may reach the reader: {ty}"
    );
    assert!(
        !hint.contains("TyConId") && !hint.contains("`T`"),
        "the hint must name the real type, not a template letter: {hint}"
    );
}

/// The solver's hint owes the reader the same orphan-rule honesty the
/// interpolation one does: for a type they cannot extend, `deriving` is not on
/// the table.
#[test]
fn t029_from_the_solver_respects_the_orphan_rule() {
    let src = "\
class Sizeable a =\n\
\x20   size (x: a) -> Int\n\
\n\
pub fn measure (d: Duration) -> Int =\n\
\x20   size d\n\
";
    let errors = run_typecheck_on_source("t029_solver_orphan", src);
    let (ty, hint) = only_no_instance(&errors);
    assert_eq!(ty, "Duration");
    assert!(
        !hint.contains("deriving (Sizeable)"),
        "`deriving` is not available on a type the reader does not declare: {hint}"
    );
    assert!(
        hint.contains("orphan"),
        "the hint must say why the obvious fix is refused: {hint}"
    );
}

// ── T011 — an alias that stands for nothing ───────────────────────────────────

/// The cycles reported for `src`, each rendered as `A -> B -> A`.
fn alias_cycles(stem: &str, src: &str) -> Vec<String> {
    run_typecheck_on_source(stem, src)
        .into_iter()
        .filter_map(|e| match e {
            TypeError::RecursiveTypeAlias { cycle, .. } => Some(cycle.join(" -> ")),
            _ => None,
        })
        .collect()
}

/// An alias that resolves to itself stands for nothing, and used to type-check
/// clean — leaving the first real error to name a type no value can have.
#[test]
fn an_alias_that_resolves_to_itself_is_reported() {
    let cycles = alias_cycles("t011_self", "type A = A\npub fn f (x: A) -> Int = 0\n");
    assert_eq!(cycles, vec!["A -> A".to_string()]);
}

/// Two aliases that resolve to each other are one mistake, so they get one
/// diagnostic — naming both, in the order they refer to one another.
#[test]
fn a_pair_of_aliases_that_close_on_each_other_is_one_diagnostic() {
    let cycles = alias_cycles(
        "t011_pair",
        "type A = B\ntype B = A\npub fn f (x: A) -> Int = 0\n",
    );
    assert_eq!(cycles, vec!["A -> B -> A".to_string()]);
}

/// The path is what makes a longer cycle findable, so the whole ring is named.
#[test]
fn a_longer_cycle_names_every_alias_on_it() {
    let cycles = alias_cycles(
        "t011_three",
        "type A = B\ntype B = C\ntype C = A\npub fn f (x: A) -> Int = 0\n",
    );
    assert_eq!(cycles, vec!["A -> B -> C -> A".to_string()]);
}

/// Reaching itself through another type is still reaching itself: `List A` has
/// no size, because the `A` inside it is the same alias.
#[test]
fn an_alias_that_contains_itself_is_reported() {
    let cycles = alias_cycles("t011_list", "type A = List A\npub fn f (x: A) -> Int = 0\n");
    assert_eq!(cycles, vec!["A -> A".to_string()]);
}

/// Two independent cycles are two mistakes and get one diagnostic each.
#[test]
fn independent_cycles_are_reported_separately() {
    let cycles = alias_cycles(
        "t011_two_rings",
        "type A = B\ntype B = A\ntype C = D\ntype D = C\npub fn f (x: A) -> Int = 0\n",
    );
    assert_eq!(
        cycles,
        vec!["A -> B -> A".to_string(), "C -> D -> C".to_string()]
    );
}

/// A union may refer to itself — that is how a tree is declared — and must not
/// be caught by the alias check.
#[test]
fn a_recursive_union_is_not_a_cycle() {
    let src = "\
type Tree a = Leaf | Node (Tree a) a (Tree a)
pub fn f (t: Tree Int) -> Int = 0
";
    let errors = run_typecheck_on_source("t011_union", src);
    assert!(errors.is_empty(), "got {errors:?}");
}

/// An alias chain that reaches a real type is what aliases are for.
#[test]
fn an_alias_chain_that_terminates_is_silent() {
    let src = "\
type IntList = List Int
type Numbers = IntList
pub fn f (x: Numbers) -> Int = 0
";
    let errors = run_typecheck_on_source("t011_chain", src);
    assert!(errors.is_empty(), "got {errors:?}");
}

/// A parametric alias applied to a real type terminates too.
#[test]
fn a_parametric_alias_is_silent() {
    let src = "\
type Pair a = (a, a)
type IntPair = Pair Int
pub fn f (p: IntPair) -> Int = 0
";
    let errors = run_typecheck_on_source("t011_parametric", src);
    assert!(errors.is_empty(), "got {errors:?}");
}

// ── T007 — the scrutinee expects, the pattern claims ──────────────────────────

/// Pull the `(expected, pattern)` strings of the first `T007`.
fn first_pattern_mismatch(stem: &str, src: &str) -> (String, String) {
    run_typecheck_on_source(stem, src)
        .into_iter()
        .find_map(|e| match e {
            TypeError::PatternTypeMismatch {
                expected, pattern, ..
            } => Some((expected, pattern)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no T007 produced for {stem}"))
}

/// The scrutinee sets the expectation; the pattern is what claims to meet it.
///
/// Reported as a plain mismatch the two arrive unlabelled and in the order the
/// unifier happened to see them, which read as `expected Text, got Int` for an
/// `Int` matched against `"text"` — the opposite of how the reader holds it.
#[test]
fn a_literal_pattern_mismatch_names_the_scrutinee_as_the_expectation() {
    let src = "\
pub fn f (x: Int) -> Int =
    match x
        \"text\" -> 1
        _ -> 2
";
    let (expected, pattern) = first_pattern_mismatch("t007_literal", src);
    assert_eq!(expected, "Int", "the scrutinee is what was expected");
    assert_eq!(pattern, "Text", "the pattern is what claimed to meet it");
}

/// A list pattern against a non-list scrutinee reports the same way, and says
/// what the pattern implies rather than leaving the reader to infer it.
#[test]
fn a_list_pattern_against_a_non_list_says_what_it_implies() {
    let src = "\
pub fn f (x: Int) -> Int =
    match x
        [a, b] -> a
        _ -> 2
";
    let (expected, pattern) = first_pattern_mismatch("t007_list", src);
    assert_eq!(expected, "Int");
    assert!(
        pattern.starts_with("List"),
        "the pattern implies a list; got {pattern:?}"
    );
}

/// A cons pattern is the same shape claim written differently.
#[test]
fn a_cons_pattern_against_a_non_list_reports_the_same_way() {
    let src = "\
pub fn f (x: Int) -> Int =
    match x
        h :: t -> h
        _ -> 2
";
    let (expected, pattern) = first_pattern_mismatch("t007_cons", src);
    assert_eq!(expected, "Int");
    assert!(
        pattern.starts_with("List"),
        "the pattern implies a list; got {pattern:?}"
    );
}

/// A tuple pattern of the wrong length is a length problem, not a type
/// problem, so it keeps the variant that says so rather than being re-filed as
/// a pattern type mismatch.
#[test]
fn a_tuple_pattern_of_the_wrong_length_is_not_a_type_mismatch() {
    let src = "\
pub fn f (p: (Int, Int)) -> Int =
    match p
        (a, b, c) -> a
        _ -> 2
";
    let errors = run_typecheck_on_source("t007_tuple_len", src);
    assert!(
        errors
            .iter()
            .all(|e| !matches!(e, TypeError::PatternTypeMismatch { .. })),
        "a length mismatch must not read as a type mismatch; got {errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.code() == "T003"),
        "the length mismatch must still be reported; got {errors:?}"
    );
}

/// A pattern that fits produces nothing at all.
#[test]
fn a_pattern_that_fits_is_silent() {
    let src = "\
pub fn f (x: Int) -> Int =
    match x
        0 -> 1
        _ -> 2
";
    let errors = run_typecheck_on_source("t007_ok", src);
    assert!(errors.is_empty(), "got {errors:?}");
}

// ── T002 — which argument, and which way round ────────────────────────────────

/// Pull the fields of the first `T002 TypeMismatchInCall`.
fn first_call_mismatch(stem: &str, src: &str) -> (String, usize, String, String) {
    run_typecheck_on_source(stem, src)
        .into_iter()
        .find_map(|e| match e {
            TypeError::TypeMismatchInCall {
                callee,
                arg_index,
                expected,
                found,
                ..
            } => Some((callee, arg_index, expected, found)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no T002 produced for {stem}"))
}

/// A bad argument is reported against the argument, not against the call.
///
/// The reader of `f 1 "x" 3 "four"` needs to know it is the fourth one; a bare
/// expected/got over the whole call leaves them to work it out.
#[test]
fn a_bad_argument_is_reported_by_position() {
    let src = "\
pub fn f (a: Int) (b: Text) (c: Int) (d: Int) -> Int = c
pub fn g () -> Int = f 1 \"x\" 3 \"four\"
";
    let (callee, arg_index, expected, found) = first_call_mismatch("t002_position", src);
    assert_eq!(callee, "f");
    assert_eq!(
        arg_index, 3,
        "the fourth argument is the one that does not fit"
    );
    assert_eq!(expected, "Int");
    assert_eq!(found, "Text");
}

/// The declaration is the expectation and the argument is what arrived, in
/// that order. Unifying the other way round printed the two types reversed,
/// which reads as though the argument were the thing being conformed to.
#[test]
fn a_call_mismatch_reads_declaration_first() {
    let src = "\
pub fn takesInt (a: Int) -> Int = a
pub fn g () -> Int = takesInt \"text\"
";
    let (_, _, expected, found) = first_call_mismatch("t002_direction", src);
    assert_eq!(expected, "Int", "the parameter is what was expected");
    assert_eq!(found, "Text", "the argument is what arrived");
}

/// A partial application knows the position too, and states it the same way.
#[test]
fn a_partially_applied_call_reports_the_same_way() {
    let src = "\
pub fn add (a: Int) (b: Int) -> Int = a + b
pub fn g () -> Int =
    let h = add \"text\"
    0
";
    let (callee, arg_index, expected, found) = first_call_mismatch("t002_partial", src);
    assert_eq!(callee, "add");
    assert_eq!(arg_index, 0);
    assert_eq!(expected, "Int");
    assert_eq!(found, "Text");
}

/// Every argument that does not fit is reported, so one pass fixes them all.
#[test]
fn every_bad_argument_is_reported_not_just_the_first() {
    let src = "\
pub fn f (a: Int) (b: Int) -> Int = a
pub fn g () -> Int = f \"x\" \"y\"
";
    let indices: Vec<usize> = run_typecheck_on_source("t002_all", src)
        .into_iter()
        .filter_map(|e| match e {
            TypeError::TypeMismatchInCall { arg_index, .. } => Some(arg_index),
            _ => None,
        })
        .collect();
    assert_eq!(
        indices,
        vec![0, 1],
        "both arguments are wrong; both must say so"
    );
}

/// A callee with no name the reader would recognise keeps the plain mismatch.
/// `T002` puts a name in its message, so it may only fire when there is one.
#[test]
fn an_unnamed_callee_keeps_the_plain_mismatch() {
    let src = "\
pub fn g () -> Int = (fn x -> x + 1) \"text\"
";
    let errors = run_typecheck_on_source("t002_unnamed", src);
    assert!(
        errors
            .iter()
            .all(|e| !matches!(e, TypeError::TypeMismatchInCall { .. })),
        "an applied lambda has no name to print; got {errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.code() == "T001"),
        "the mismatch must still be reported; got {errors:?}"
    );
}

/// The `add(1, 2)` trap keeps teaching the call syntax. Someone who wrote a
/// C-style call needs to hear that Ridge calls are space-separated, which is
/// worth more to them than being told which argument does not fit.
#[test]
fn the_paren_comma_trap_still_teaches_the_call_shape() {
    let src = "\
pub fn add (a: Int) (b: Int) -> Int = a + b
pub fn f () -> Int = add(1, 2)
";
    let t001 = first_t001_display("t002_trap_a", src);
    assert!(
        t001.contains("space-separated"),
        "the trap hint must survive; got:\n{t001}"
    );
}

// ── T010 — a type that would contain itself ───────────────────────────────────

/// The `(var, ty)` of the first `T010`.
fn first_occurs(stem: &str, src: &str) -> (String, String) {
    run_typecheck_on_source(stem, src)
        .into_iter()
        .find_map(|e| match e {
            TypeError::OccursCheck { var, ty, .. } => Some((var, ty)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no T010 produced for {stem}"))
}

/// Both sides are types the reader can read.
///
/// This used to print `cannot unify ?65 with #6 (?63)` — two unification
/// counters and an index into the type-constructor arena. `T001` has a test of
/// its own forbidding exactly that; `T010` is built two cases away in the same
/// file and was never held to it.
#[test]
fn an_infinite_type_names_types_not_counters() {
    let src = "\
pub fn f (xs: List a) -> Int =
    match xs
        x :: rest -> f x
        _ -> 0
";
    let (var, ty) = first_occurs("t010_names", src);
    for side in [&var, &ty] {
        assert!(
            !side.contains('#') && !side.contains('?') && !side.contains("TyConId"),
            "T010 must not print internals; got {side:?}"
        );
    }
    assert!(
        ty.contains("List"),
        "the containing type is named; got {ty:?}"
    );
}

/// The message claims a variable occurs inside a type, so it has to. Lettering
/// keys on the raw variable id, and a variable already unified with the one
/// being named reads as a different letter until both are resolved to the same
/// representative — which produced `a` would have to contain itself: `List b`,
/// a sentence the type beside it disproves.
#[test]
fn the_named_variable_appears_in_the_type_it_occurs_inside() {
    for (stem, src) in [
        (
            "t010_consistent_list",
            "\
pub fn f (xs: List a) -> Int =
    match xs
        x :: rest -> f x
        _ -> 0
",
        ),
        (
            "t010_consistent_nested",
            "\
type Nested a = Flat a | Deep (Nested (List a))
pub fn depth (n: Nested a) -> Int =
    match n
        Flat _ -> 0
        Deep inner -> 1 + depth inner
",
        ),
    ] {
        let (var, ty) = first_occurs(stem, src);
        let bare = var.trim_matches('`');
        assert!(
            ty.split(|c: char| !c.is_alphanumeric()).any(|w| w == bare),
            "{stem}: `{bare}` is said to occur inside `{ty}`, and does not"
        );
    }
}

/// A well-typed recursion produces nothing.
#[test]
fn a_recursion_that_terminates_is_silent() {
    let src = "\
pub fn len (xs: List a) -> Int =
    match xs
        _ :: rest -> 1 + len rest
        _ -> 0
";
    let errors = run_typecheck_on_source("t010_ok", src);
    assert!(errors.is_empty(), "got {errors:?}");
}
