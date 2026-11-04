//! Typed comparison contracts use their public stdlib routes end to end.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

fn workspace(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"typed-comparisons\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"io\"]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), source).unwrap();
    dir
}

#[test]
fn float_predicates_use_numerical_zero_exactness_and_ordered_operands() {
    which::which("erl").expect("erl required");
    which::which("erlc").expect("erlc required");
    let source = r#"
import std.float as Float
import std.io as Io

pub fn exercise () -> List Bool = [
    Float.equal 0.0 (-0.0),
    Float.equal (-0.0) 0.0,
    Float.equal 1.25 1.25,
    Float.equal 1.25 1.5,
    Float.equal (0.1 + 0.2) 0.3,
    Float.less 0.0 (-0.0),
    Float.greater (-0.0) 0.0,
    Float.lessOrEqual 0.0 (-0.0),
    Float.greaterOrEqual (-0.0) 0.0,
    Float.less (-2.0) (-1.0),
    Float.greater (-1.0) (-2.0),
    Float.lessOrEqual 3.0 2.0,
    Float.greaterOrEqual 2.0 3.0,
    Float.floor (-0.1) == (-1),
    Float.ceil 0.1 == 1,
    Float.floor (-1.0) == (-1),
    Float.ceil 1.0 == 1,
    Float.floor (-0.0) == 0,
    Float.ceil (-0.0) == 0,
    Float.equal (Float.min 2.0 3.0) 2.0,
    Float.equal (Float.max 2.0 3.0) 3.0,
    Float.totalCompare 0.0 (-0.0) != 0
]

fn io observed (label: Text) (value: Float) -> Float =
    Io.println label
    value

pub fn io effects () -> Bool =
    Float.less (observed "left" 1.0) (observed "right" 2.0)
"#;
    let dir = workspace(source);
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let expression = r#"Expected=[true,true,true,false,false,false,false,true,true,true,true,false,false,true,true,true,true,true,true,true,true,true],Expected=ridge_app_Main:exercise(),true=ridge_app_Main:effects(),io:put_chars("float-predicates-ok\n"),halt()."#;
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(module.parent().unwrap())
        .args(["-eval", expression])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        "left\nright\nfloat-predicates-ok\n"
    );
}

#[test]
fn inferred_field_evidence_preserves_custom_instances_across_call_boundaries() {
    assert_program_files(
        r"
import app.Support as Support

type Key = { id: Int }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id % 10 == right.id % 10
instance Ord Key =
    compare (left: Key) (right: Key) -> Ordering = compare (left.id % 10) (right.id % 10)

type Holder a = { value: a }
fn sorted record -> Bool = List.sort record.value == record.value
fn same record -> Bool = record.left == record.right
fn nested record -> Bool = sorted record.inner
fn captured (left: a) (right: a) -> Bool where Eq a =
    fn check () -> Bool = left == right
    check ()

pub fn exercise () -> Bool =
    fn local record -> Bool = List.sort record.value == record.value
    let first = Key { id = 11 }
    let second = Key { id = 2 }
    let values = Holder { value = [first, second] }
    let integers = local ({ value = [1, 2, 3] })
    let alias: fn (Holder (List Key)) -> Bool = local
    let pair = (alias, alias)
    let (left, right) = pair
    let invoke = fn () -> alias values
    sorted values && integers && local values && Support.sorted values &&
        left values && right values && invoke () &&
        nested ({ inner = values }) &&
        captured first (Key { id = 1 }) &&
        same ({ left = first, right = Key { id = 1 } }) &&
        Support.same ({ left = first, right = Key { id = 1 } })
",
        "true",
        &[(
            "Support",
            r"
pub fn sorted record -> Bool = List.sort record.value == record.value
pub fn same record -> Bool = record.left == record.right
",
        )],
    );
}

#[test]
fn generic_record_forwarding_keeps_distinct_field_evidence() {
    assert_true_program(
        r"
type Key = { id: Int }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id % 10 == right.id % 10
type Other = { id: Int } deriving (Eq)
fn both record -> Bool = record.a == record.b && record.c == record.d
fn relay record -> Bool = both record
pub fn exercise () -> Bool =
    relay ({ a = Key { id = 11 }, b = Key { id = 1 }, c = Other { id = 2 }, d = Other { id = 2 } })
",
    );
}

#[test]
fn fundep_results_refine_shared_inference_variables_before_head_matching() {
    assert_true_program(&format!(
        "{SHARED_FUNDEP_SOURCE}{}",
        r"
fn forward value = transfer value
pub fn exercise () -> Bool =
    let direct: OutputBox (List Int) = transfer (InputBox { value = [] })
    let integers: OutputBox (List Int) = forward (InputBox { value = [] })
    let texts: OutputBox (List Text) = forward (InputBox { value = [] })
    let alias = forward
    let viaAlias: OutputBox (List Bool) = alias (InputBox { value = [] })
    let pair = (forward, 0)
    let (fromPair, _) = pair
    let viaTuple: OutputBox (List Int) = fromPair (InputBox { value = [] })
    let invoke = fn value -> forward value
    let captured: OutputBox (List Int) = invoke (InputBox { value = [] })
    direct.value == [] && integers.value == [] && texts.value == [] &&
        viaAlias.value == [] && viaTuple.value == [] && captured.value == []
"
    ));
}

const SHARED_FUNDEP_SOURCE: &str = r"
type InputBox a = { value: a }
type OutputBox a = { value: a }
class Move q p | q -> p =
    transfer (value: q) -> p
instance Move (InputBox a) (OutputBox a) =
    transfer (value: InputBox a) -> OutputBox a = OutputBox { value = value.value }
";

#[test]
fn shared_fundep_inference_does_not_specialise_rigid_promises_or_effects() {
    let good = "let value: OutputBox (List Int) = forward (InputBox { value = [1] })";
    let bad = "let value: OutputBox (List Bool) = forward (InputBox { value = [1] })";
    let mut bodies = vec![
        "fn invalid (value: InputBox a) -> OutputBox Int = transfer value\n".into(),
        "fn invalid (value: InputBox Int) -> OutputBox Text = transfer value\n".into(),
    ];
    for (first, second) in [(good, bad), (bad, good)] {
        for declaration in [
            "fn forward value = transfer value",
            "let forward = transfer",
            "let pair = (transfer, 0)\n    let (forward, _) = pair",
            "let alias = transfer\n    let forward = fn value -> alias value",
        ] {
            bodies.push(format!("pub fn exercise () -> Bool =\n    {declaration}\n    {first}\n    {second}\n    true\n"));
        }
    }
    bodies.push(
        r#"
import std.io as Io
class Predicate q p | q -> p =
    accepts (value: q) (predicate: p) -> Bool
instance Predicate (InputBox a) (fn a -> Bool) =
    accepts (value: InputBox a) (predicate: fn a -> Bool) -> Bool = predicate value.value
fn io noisy (value: Int) -> Bool =
    Io.println "unexpected"
    true
pub fn invalid () -> Bool = accepts (InputBox { value = 1 }) noisy
"#
        .into(),
    );
    for body in bodies {
        let source = format!("{SHARED_FUNDEP_SOURCE}{body}");
        let dir = workspace(&source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            artifacts
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "T001"),
            "{source}\n{:?}",
            artifacts.diagnostics
        );
        assert!(
            !artifacts
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.starts_with('P')
                    || diagnostic.code.starts_with('L')),
            "{source}\n{:?}",
            artifacts.diagnostics
        );
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn inferred_inner_constraints_reject_invalid_specializations_and_missing_promises() {
    let valid = "{ value = [1, 2] }";
    let invalid = "{ value = [Bad { id = 1 }] }";
    let mut sources: Vec<(String, &[&str])> = Vec::new();
    for (first, second) in [(valid, invalid), (invalid, valid)] {
        for uses in [
            format!("let a = local ({first})\n    local ({second}) && a"),
            format!("let alias = local\n    let a = alias ({first})\n    alias ({second}) && a"),
            format!("let (left, right) = (local, local)\n    let a = left ({first})\n    right ({second}) && a"),
            format!("let invoke = fn record -> local record\n    let a = invoke ({first})\n    invoke ({second}) && a"),
        ] {
            sources.push((format!("type Bad = {{ id: Int }}\npub fn exercise () -> Bool =\n    fn local record -> Bool = List.sort record.value == record.value\n    {uses}\n"), &["T029", "T002"]));
        }
    }
    sources.extend([
        ("pub fn exercise () -> Bool =\n    fn same record -> Bool = record.value == record.value\n    let good = same ({ value = 1 })\n    same ({ value = 1.0 }) && good\n".into(), &["T029"][..]),
        ("pub fn exercise () -> Bool =\n    fn same (left: a) (right: a) -> Bool = left == right\n    same 1 1\n".into(), &["T055"][..]),
        ("fn missing (left: a) (right: a) -> Bool =\n    fn captured () -> Bool = left == right\n    captured ()\n".into(), &["T055"][..]),
    ]);
    for (source, expected) in sources {
        let dir = workspace(&source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        // A materialized alias can already have closed its evidence at the
        // first type; the second use then fails as a type mismatch instead.
        assert!(
            artifacts
                .diagnostics
                .iter()
                .any(|diagnostic| expected.contains(&diagnostic.code)),
            "{source}\n{:?}",
            artifacts.diagnostics
        );
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn operators_dispatch_checked_methods_and_ignore_collection_seeds() {
    assert_true_program(
        r#"
import std.map as Map
import std.set as Set
import std.test as Test
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id % 10 == right.id % 10
instance Ord Key =
    compare (left: Key) (right: Key) -> Ordering = compare (left.id % 10) (right.id % 10)
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash (value.id % 10) hasher
fn same left right = left == right
fn before (left: a) (right: a) -> Bool where Ord a = left < right
pub fn exercise () -> Bool =
    let a = Key { id = 1, label = "a" }
    let b = Key { id = 11, label = "b" }
    let c = Key { id = 2, label = "c" }
    let assertSame = Test.assertEq
    let captured = fn left right -> Test.assertEq left right "captured keys"
    a == b && (a != b) == false && a < c && a <= b && c > b && b >= a &&
        same a b && before b c && (a, 7) == (b, 7) &&
        { key = a } == { key = b } && () == () &&
        Map.fromList [(a, 7)] == Map.fromList [(b, 7)] &&
        Set.fromList [a, c] == Set.fromList [c, b] &&
        Test.assertEq a b "equivalent keys" == Ok () &&
        Test.assertNe a c "distinct keys" == Ok () &&
        assertSame a b "aliased keys" == Ok () && captured a b == Ok ()
"#,
    );
}

#[test]
fn operators_evaluate_both_operands_once_in_source_order() {
    let output = run_program_eval(
        r#"
import std.io as Io
fn io read (label: Text) (value: Int) -> Int =
    Io.println label
    value
pub fn io exercise () -> Bool =
    let results = [read "eq:left" 7 == read "eq:right" 7, read "ne:left" 7 != read "ne:right" 7,
        read "lt:left" 7 < read "lt:right" 8, read "le:left" 7 <= read "le:right" 7,
        read "gt:left" 8 > read "gt:right" 7, read "ge:left" 7 >= read "ge:right" 7]
    results == [true, false, true, true, true, true]
"#,
        "true=ridge_app_Main:exercise(),halt().",
        &[],
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().replace("\r\n", "\n"),
        "eq:left\neq:right\nne:left\nne:right\nlt:left\nlt:right\nle:left\nle:right\ngt:left\ngt:right\nge:left\nge:right\n");
}

#[test]
fn comparison_guards_use_methods_and_preserve_fallthrough_and_recursion() {
    let output = run_program_eval(
        r#"
import std.io as Io
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id % 10 == right.id % 10
instance Ord Key =
    compare (left: Key) (right: Key) -> Ordering = compare (left.id % 10) (right.id % 10)
fn classify (key: Key) -> Int =
    match key
        candidate when candidate == Key { id = 11, label = "other" } -> 1
        candidate when candidate < Key { id = 3, label = "bound" } -> 2
        _ -> 3
fn positive (x: Int) (y: Int) -> Int =
    guard (x > 0) else return 0
    guard (y > 0) else return 0
    x + y
fn fib (n: Int) -> Int =
    fn loop (k: Int) -> Int =
        if k <= 1 then k
        else loop (k - 1) + loop (k - 2)
    loop n
fn io observed () -> Key =
    Io.println "scrutinee"
    Key { id = 12, label = "source" }
pub fn io exercise () -> Bool =
    let result = match observed ()
        key when key == Key { id = 1, label = "miss" } -> 1
        key when key < Key { id = 3, label = "hit" } -> 2
        _ -> 3
    result == 2 && classify (Key { id = 1, label = "source" }) == 1 &&
        classify (Key { id = 12, label = "source" }) == 2 &&
        classify (Key { id = 9, label = "source" }) == 3 &&
        positive 2 3 == 5 && positive 0 3 == 0 && positive 2 0 == 0 && fib 7 == 13
"#,
        "true=ridge_app_Main:exercise(),halt().",
        &[],
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        "scrutinee\n"
    );
}

#[test]
fn operators_reject_values_without_the_required_contract() {
    for expression in [
        "1.0 == 1.0",
        "1.0 != 1.0",
        "1.0 < 1.0",
        "1.0 <= 1.0",
        "1.0 > 1.0",
        "1.0 >= 1.0",
        "(fn () -> 1) == (fn () -> 1)",
        "Map.fromList [(1, 1.0)] == Map.fromList [(1, 1.0)]",
        "Test.assertEq 1.0 1.0 \"float\" == Ok ()",
    ] {
        let dir = workspace(&format!(
            "import std.map as Map\nimport std.test as Test\npub fn exercise () -> Bool = {expression}\n"
        ));
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            artifacts.diagnostics.iter().any(|d| d.code == "T029"),
            "{expression}: {:?}",
            artifacts.diagnostics
        );
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn operators_schema_descriptors_keep_exact_defaults() {
    assert_true_program(
        r#"
import std.schema as Schema (ColumnSchema, Generation, DefaultLit, Supplied)
import std.sql (DbType, DbFloat, DbArray, DbDecimal, sqlFloat, sqlInt, sqlText, sqlArray)
fn column (ty: DbType) (generation: Generation) -> ColumnSchema Unit =
    Schema.generated generation (Schema.mkColumn "x" "x" ty false)
pub fn exercise () -> Bool =
    let base = column (DbArray (DbDecimal 8 2)) Supplied
    let positive = column DbFloat (DefaultLit (sqlFloat 0.0))
    let negative = column DbFloat (DefaultLit (sqlFloat (-0.0)))
    let array = column DbFloat (DefaultLit (sqlArray [sqlFloat 1.5, sqlText "a"]))
    let different = column DbFloat (DefaultLit (sqlArray [sqlFloat 1.5, sqlText "b"]))
    Schema.columnAltered base base == false &&
        Schema.columnAltered base (column (DbArray (DbDecimal 8 3)) Supplied) &&
        Schema.columnAltered positive positive == false &&
        Schema.columnAltered positive negative &&
        Schema.columnAltered positive (column DbFloat (DefaultLit (sqlInt 0))) &&
        Schema.columnAltered array array == false && Schema.columnAltered array different
"#,
    );
}

#[test]
fn operators_schema_descriptor_comparator_is_project_private() {
    let dir = workspace("import std.sql (sameValue, sqlInt)\npub fn exercise () -> Bool = sameValue (sqlInt 1) (sqlInt 1)\n");
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts
            .diagnostics
            .iter()
            .any(|d| d.code.starts_with('R') && d.primary_message.contains("sameValue")),
        "{:?}",
        artifacts.diagnostics
    );
    assert!(artifacts.core_files.is_empty());
}

#[test]
fn float_predicates_reject_non_float_arguments_without_coercion() {
    for name in ["equal", "less", "lessOrEqual", "greater", "greaterOrEqual"] {
        for (left, right) in [("1", "1.0"), ("1.0", "1"), ("\"1\"", "1.0")] {
            let source = format!(
                "import std.float as Float\npub fn exercise () -> Bool = Float.{name} {left} {right}\n"
            );
            let dir = workspace(&source);
            let cache = tempfile::tempdir().unwrap();
            let artifacts = compile_workspace(
                CompileOptions::new(dir.path().to_path_buf())
                    .with_emit(EmitArtefacts::Core)
                    .with_cache_root(cache.path().to_path_buf()),
            )
            .unwrap();
            let argument = if left == "1.0" { 2 } else { 1 };
            let found = if left.starts_with('"') { "Text" } else { "Int" };
            let mismatch = format!("argument {argument}: expected Float, got {found}");
            assert!(
                artifacts
                    .diagnostics
                    .iter()
                    .any(|d| { d.code == "T002" && d.primary_message.contains(&mismatch) }),
                "{source}\n{:?}",
                artifacts.diagnostics
            );
            assert!(artifacts.core_files.is_empty());
        }
    }
}

#[test]
fn structural_comparisons_dispatch_component_instances_and_generic_evidence() {
    let source = r#"
import std.bool as Bool
import app.Support as Support
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (a: Key) (b: Key) -> Bool = a.id == b.id
instance Ord Key =
    compare (a: Key) (b: Key) -> Ordering = compare a.id b.id

type Bomb = { value: Int }
instance Eq Bomb =
    eq (left: Bomb) (right: Bomb) -> Bool = 1 / (left.value - right.value) == 0
instance Ord Bomb =
    compare (left: Bomb) (right: Bomb) -> Ordering = compare (1 / (left.value - right.value)) 0
type Pair a = (a, Int)
fn aliasSame (left: Pair a) (right: Pair a) -> Bool where Eq a = eq left right
fn captured (left: a) (right: a) -> Bool where Eq a =
    let check = fn () -> Bool = eq (left, 0) (right, 0)
    check ()

fn samePair (left: a) (right: a) -> Bool where Eq a = eq (left, 0) (right, 0)
fn orderPair (left: a) (right: a) -> Ordering where Ord a = compare (left, 0) (right, 0)
fn sameRecord (left: a) (right: a) -> Bool where Eq a = eq ({ key = left }) ({ key = right })
fn inferred left right = eq (left, 0) (right, 0)

pub fn exercise () -> List Bool =
    let a = Key { id = 1, label = "z" }
    let b = Key { id = 1, label = "a" }
    let c = Key { id = 2, label = "a" }
    let pairEqual = eq
    let pairOrder = compare
    let bomb = Bomb { value = 0 }
    [eq (a, 7) (b, 7),
     Bool.not (eq (a, 7) (b, 8)),
     compare (a, 7) (b, 8) == Less,
     compare (c, 0) (a, 9) == Greater,
     compare (a, 7) (b, 7) == Equal,
     eq ({ key = a, n = 7 }) ({ n = 7, key = b }),
     compare ({ z = 0, a = c }) ({ a = a, z = 9 }) == Greater,
     eq ((a, 7), { key = a }) ((b, 7), { key = b }),
     samePair a b,
     orderPair a c == Less,
     sameRecord a b,
     inferred a b,
     inferred 1 1,
     pairEqual (a, 7) (b, 7),
     Bool.not (pairEqual (a, 7) (b, 8)),
     pairOrder (a, 7) (b, 7) == Equal,
     Support.same a b,
     Support.order a c == Less,
     aliasSame (a, 7) (b, 7),
     captured a b,
     Bool.not (eq (a, bomb) (c, bomb)),
     compare (a, bomb) (c, bomb) == Less,
     Bool.not (eq ({ a = a, z = bomb }) ({ a = c, z = bomb })),
     compare ({ a = a, z = bomb }) ({ a = c, z = bomb }) == Less]
"#;
    let dir = workspace(source);
    std::fs::write(
        dir.path().join("app/src/Support.ridge"),
        r"
pub fn same (left: a) (right: a) -> Bool where Eq a = eq (left, 7) (right, 7)
pub fn order (left: a) (right: a) -> Ordering where Ord a = compare (left, 7) (right, 7)
",
    )
    .unwrap();
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"]).arg(module.parent().unwrap())
        .args(["-eval", "[true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true]=ridge_app_Main:exercise(),io:put_chars(\"structural-comparisons-ok\\n\"),halt()."])
        .output().unwrap();
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        "structural-comparisons-ok\n"
    );
}

#[test]
fn structural_comparisons_require_all_components_and_written_generic_promises() {
    for source in [
        "pub fn exercise () = eq (1, 0.0) (1, 0.0)",
        "pub fn exercise () = compare (1, 0.0) (1, 0.0)",
        "pub fn exercise () = eq ({ n = 1, x = 0.0 }) ({ x = 0.0, n = 1 })",
        "pub fn exercise () = compare ({ n = 1, x = 0.0 }) ({ x = 0.0, n = 1 })",
        "fn ready () = true\npub fn exercise () = eq (1, ready) (1, ready)",
        "fn ready () = true\npub fn exercise () = compare ({ x = ready }) ({ x = ready })",
        "fn same left right = eq (left, 0) (right, 0)\npub fn exercise () = (same 1 1, same 0.0 0.0)",
        "fn same left right = eq (left, 0) (right, 0)\npub fn exercise () = (same 0.0 0.0, same 1 1)",
    ] {
        let dir = workspace(source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core).with_cache_root(cache.path().to_path_buf())).unwrap();
        assert!(artifacts.diagnostics.iter().any(|d| d.code == "T029"),
            "{source}\n{:?}", artifacts.diagnostics);
        assert!(artifacts.core_files.is_empty());
    }
    for (class, expression) in [
        ("Eq", "eq (left, 0) (right, 0)"),
        ("Ord", "compare ({ key = left }) ({ key = right })"),
    ] {
        let result = if class == "Eq" { "Bool" } else { "Ordering" };
        let source = format!("pub fn same (left: a) (right: a) -> {result} = {expression}");
        let dir = workspace(&source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            artifacts
                .diagnostics
                .iter()
                .any(|d| d.code == "T055" && d.primary_message.contains(class)),
            "{source}\n{:?}",
            artifacts.diagnostics
        );
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn comparison_instances_keep_same_named_types_in_distinct_modules() {
    let dir = workspace(
        r"
import app.A as A
import app.B as B
pub fn exercise () -> Bool =
    eq (A.key 1) (A.key 11) == false &&
        eq (B.key 1) (B.key 11) &&
        compare (A.key 1) (A.key 11) == Less &&
        compare (B.key 1) (B.key 11) == Equal
",
    );
    for (module, eq_body, ord_body) in [
        ("A", "left.id == right.id", "compare left.id right.id"),
        (
            "B",
            "left.id % 10 == right.id % 10",
            "compare (left.id % 10) (right.id % 10)",
        ),
    ] {
        let source = format!(
            r"
pub type Key = {{ id: Int }}
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = {eq_body}
instance Ord Key =
    compare (left: Key) (right: Key) -> Ordering = {ord_body}
pub fn key (id: Int) -> Key = Key {{ id = id }}
"
        );
        std::fs::write(dir.path().join(format!("app/src/{module}.ridge")), source).unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(module.parent().unwrap())
        .args([
            "-eval",
            "true=ridge_app_Main:exercise(),io:put_chars(\"namespaced-comparisons-ok\\n\"),halt().",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        "namespaced-comparisons-ok\n"
    );
}

#[test]
fn scoped_comparison_registry_keeps_orphan_overlap_and_superclass_checks() {
    for (expected, source) in [
        ("T031", "import app.A (Key)\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Bool = true\n"),
        ("T032", "type Key = { id: Int }\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Bool = true\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Bool = true\n"),
        ("T033", "type Key = { id: Int }\ninstance Ord Key =\n    compare (left: Key) (right: Key) -> Ordering = Equal\n"),
    ] {
        let dir = workspace("import app.B as B\npub fn exercise () -> Bool = B.marker ()\n");
        let producer = if expected == "T031" {
            "pub type Key = { id: Int }\n"
        } else {
            "pub type Key = { id: Int }\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Bool = true\n"
        };
        std::fs::write(dir.path().join("app/src/A.ridge"), producer).unwrap();
        std::fs::write(dir.path().join("app/src/B.ridge"),
            format!("{source}\npub fn marker () -> Bool = true\n")).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core).with_cache_root(cache.path().to_path_buf())).unwrap();
        let diagnostic = artifacts.diagnostics.iter().find(|d| d.code == expected)
            .unwrap_or_else(|| panic!("expected {expected}: {:?}", artifacts.diagnostics));
        assert_eq!(diagnostic.source_id.as_str(), "app/src/B.ridge");
        assert_eq!(usize::try_from(diagnostic.primary_span.start).unwrap(),
            source.rfind("instance").unwrap());
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn parameterized_instances_validate_each_source_call_in_both_orders() {
    for source in [
        "fn ready () = true\npub fn exercise () = (encode [1], encode [ready])\n",
        "fn ready () = true\npub fn exercise () = (encode [ready], encode [1])\n",
        "fn ready () = true\nfn both left right = (encode [left], encode [right])\npub fn exercise () = both ready 1\n",
        "fn ready () = true\nfn both left right = (encode [left], encode [right])\npub fn exercise () = both 1 ready\n",
    ] {
        let dir = workspace(source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core).with_cache_root(cache.path().to_path_buf())).unwrap();
        assert!(artifacts.diagnostics.iter().any(|diagnostic| diagnostic.code == "T029"
            && diagnostic.primary_message.contains("Encode")), "{source}\n{:?}", artifacts.diagnostics);
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn derived_comparisons_dispatch_nested_manual_and_generic_instances() {
    let source = r#"
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id % 10 == right.id % 10
instance Ord Key =
    compare (left: Key) (right: Key) -> Ordering = compare (left.id % 10) (right.id % 10)
type Wrapped = { key: Key } deriving (Eq, Ord)
type Envelope a = { value: a } deriving (Eq, Ord)
pub fn exercise () -> List Bool =
    let a = Key { id = 1, label = "first" }
    let b = Key { id = 11, label = "second" }
    [eq (Wrapped { key = a }) (Wrapped { key = b }),
     compare (Wrapped { key = a }) (Wrapped { key = b }) == Equal,
     eq (Envelope { value = a }) (Envelope { value = b }),
     compare (Envelope { value = a }) (Envelope { value = b }) == Equal]
"#;
    let dir = workspace(source);
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(module.parent().unwrap())
        .args([
            "-eval",
            "[true,true,true,true]=ridge_app_Main:exercise(),halt().",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn derived_comparisons_cover_recursive_variants_aliases_and_unused_parameters() {
    let source = r#"
import std.bool as Bool
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id % 10 == right.id % 10
instance Ord Key =
    compare (left: Key) (right: Key) -> Ordering = compare (left.id % 10) (right.id % 10)
type Envelope a = { value: a } deriving (Eq, Ord)
type Items a = List (Envelope a)
type Nested a = { values: Items a } deriving (Eq, Ord)
type Choice a = Empty | WithKey a | Named { value: a, rank: Int } deriving (Eq, Ord)
type Tree a = Leaf a | Branch (Tree a) (Tree a) deriving (Eq, Ord)
type Rose a = | RoseNode a (RoseTail a) deriving (Eq, Ord)
type RoseTail a = Stop | More (Rose a) deriving (Eq, Ord)
type Spiral a = End a | Recur (Spiral (List a)) deriving (Eq, Ord)
type Phantom a = | NoValue deriving (Eq, Ord)
opaque type Wrapper = { key: Key } deriving (Eq, Ord)
type Bomb = { value: Int }
instance Eq Bomb =
    eq (left: Bomb) (right: Bomb) -> Bool = 1 / (left.value - right.value) == 0
instance Ord Bomb =
    compare (left: Bomb) (right: Bomb) -> Ordering = compare (1 / (left.value - right.value)) 0
type ShortCircuit = { first: Key, last: Bomb } deriving (Eq, Ord)
fn phantom () -> Phantom (fn Int -> Bool) = NoValue
pub fn exercise () -> Bool =
    let a = Key { id = 1, label = "first" }
    let b = Key { id = 11, label = "second" }
    let c = Key { id = 2, label = "third" }
    let bomb = Bomb { value = 0 }
    eq (Nested { values = [Envelope { value = a }] }) (Nested { values = [Envelope { value = b }] }) &&
    compare (Nested { values = [Envelope { value = a }] }) (Nested { values = [Envelope { value = b }] }) == Equal &&
    eq (WithKey a) (WithKey b) &&
    compare (WithKey a) (Named { value = b, rank = 0 }) == Less &&
    compare (Named { value = a, rank = 2 }) (Named { value = b, rank = 3 }) == Less &&
    Bool.not (eq (WithKey a) (Named { value = a, rank = 0 })) &&
    eq (Branch (Leaf a) (Leaf c)) (Branch (Leaf b) (Leaf c)) &&
    compare (Branch (Leaf a) (Leaf c)) (Branch (Leaf b) (Leaf c)) == Equal &&
    eq (RoseNode a (More (RoseNode c Stop))) (RoseNode b (More (RoseNode c Stop))) &&
    compare (RoseNode a (More (RoseNode c Stop))) (RoseNode b (More (RoseNode c Stop))) == Equal &&
    eq (Recur (End [a])) (Recur (End [b])) &&
    compare (Recur (End [a])) (Recur (End [b])) == Equal &&
    eq (phantom ()) (phantom ()) &&
    compare (phantom ()) (phantom ()) == Equal &&
    eq (Wrapper { key = a }) (Wrapper { key = b }) &&
    Bool.not (eq (ShortCircuit { first = a, last = bomb }) (ShortCircuit { first = c, last = bomb })) &&
    compare (ShortCircuit { first = a, last = bomb }) (ShortCircuit { first = c, last = bomb }) == Less
"#;
    let dir = workspace(source);
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(module.parent().unwrap())
        .args(["-eval", "true=ridge_app_Main:exercise(),halt()."])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn source_comparison_instances_keep_ids_after_generated_schema_types() {
    let dir = workspace("import app.B as B\npub fn exercise () -> Bool = B.same ()\n");
    std::fs::write(
        dir.path().join("app/src/A.ridge"),
        "pub type Entity = { id: Int } deriving (Schema)\npub fn marker () -> Bool = true\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/src/B.ridge"),
        r"
import app.A as A
type Key = { id: Int }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id == right.id
pub fn same () -> Bool = A.marker () && eq (Key { id = 1 }) (Key { id = 1 })
",
    )
    .unwrap();
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    assert!(!artifacts.core_files.is_empty());
}

#[test]
fn reserved_source_types_still_require_an_import() {
    let dir =
        workspace("import app.B as B\nimport app.A as A\npub fn exercise () -> Bool = true\n");
    std::fs::write(
        dir.path().join("app/src/A.ridge"),
        "pub type Hidden = { id: Int }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/src/B.ridge"),
        "pub fn misuse (value: Hidden) -> Bool = true\n",
    )
    .unwrap();
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts
            .diagnostics
            .iter()
            .any(|d| d.code == "T056" && d.primary_message.contains("Hidden")),
        "{:?}",
        artifacts.diagnostics
    );
    assert!(artifacts.core_files.is_empty());
}

#[test]
fn derived_comparisons_require_evidence_for_applied_generic_fields() {
    for (class, method) in [("Eq", "eq"), ("Ord", "compare")] {
        for value in ["0.0", "ready"] {
            let source = format!("type Box a = {{ value: a }} deriving (Eq, Ord)\nfn ready () = true\npub fn exercise () = {method} (Box {{ value = {value} }}) (Box {{ value = {value} }})\n");
            let dir = workspace(&source);
            let cache = tempfile::tempdir().unwrap();
            let artifacts = compile_workspace(
                CompileOptions::new(dir.path().to_path_buf())
                    .with_emit(EmitArtefacts::Core)
                    .with_cache_root(cache.path().to_path_buf()),
            )
            .unwrap();
            assert!(
                artifacts
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "T029" && d.primary_message.contains(class)),
                "{source}\n{:?}",
                artifacts.diagnostics
            );
            assert!(artifacts.core_files.is_empty());
        }
    }
}

#[test]
fn derived_comparisons_keep_manual_eq_and_written_promises() {
    for (source, code, detail) in [
        ("type Key = { id: Int } deriving (Ord)\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Bool = left.id % 10 == right.id % 10\n", "T029", "manual"),
        ("type Box a = { value: a } deriving (Eq, Ord)\npub fn same (left: Box a) (right: Box a) -> Bool = eq left right\n", "T055", "Eq"),
        ("type Box a = { value: a } deriving (Eq, Ord)\npub fn order (left: Box a) (right: Box a) -> Ordering = compare left right\n", "T055", "Ord"),
    ] {
        let dir = workspace(source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core).with_cache_root(cache.path().to_path_buf())).unwrap();
        assert!(artifacts.diagnostics.iter().any(|d| d.code == code
            && d.primary_message.contains(detail)), "{source}\n{:?}", artifacts.diagnostics);
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn derived_comparisons_preserve_heterogeneous_multi_parameter_contexts() {
    let source = r#"
import std.bool as Bool
class Convert a b =
    convert (value: a) -> b
instance Convert Int Int =
    convert (value: Int) -> Int = value % 10
instance Convert Int Text =
    convert (value: Int) -> Text = $"${value}"
type Carrier a b = { value: a, marker: b }
fn converted (value: Carrier a b) -> b where Convert a b = convert value.value
instance Eq (Carrier a b) where Convert a b, Eq b =
    eq (left: Carrier a b) (right: Carrier a b) -> Bool = eq (converted left) (converted right)
type Outer a b = { wrapped: Carrier a b } deriving (Eq)
type MixedCarrier a b c = { value: a, first: b, second: c }
fn firstConverted (value: MixedCarrier a b c) -> b where Convert a b = convert value.value
fn secondConverted (value: MixedCarrier a b c) -> c where Convert a c = convert value.value
instance Eq (MixedCarrier a b c) where Convert a b, Convert a c, Eq b, Eq c =
    eq (left: MixedCarrier a b c) (right: MixedCarrier a b c) -> Bool =
        eq (firstConverted left) (firstConverted right) && eq (secondConverted left) (secondConverted right)
type MixedOuter a b c = { wrapped: MixedCarrier a b c } deriving (Eq)
pub fn exercise () -> Bool =
    eq (Outer { wrapped = Carrier { value = 1, marker = 0 } }) (Outer { wrapped = Carrier { value = 11, marker = 0 } }) &&
    Bool.not (eq (Outer { wrapped = Carrier { value = 1, marker = "" } }) (Outer { wrapped = Carrier { value = 11, marker = "" } })) &&
    Bool.not (eq (MixedOuter { wrapped = MixedCarrier { value = 1, first = 0, second = "" } }) (MixedOuter { wrapped = MixedCarrier { value = 11, first = 0, second = "" } }))
"#;
    let dir = workspace(source);
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Both)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(module.parent().unwrap())
        .args(["-eval", "true=ridge_app_Main:exercise(),halt()."])
        .output()
        .unwrap();
    if !output.status.success() {
        for path in &artifacts.core_files {
            if path.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main") {
                eprintln!("{}", std::fs::read_to_string(path).unwrap());
            }
        }
    }
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn derived_contexts_preserve_function_head_capabilities() {
    let template = r#"
import std.io as Io
class Predicate a =
    evaluate (value: a) -> Bool
instance Predicate (fn Int -> Bool) =
    evaluate (value: fn Int -> Bool) -> Bool = value 1
type Carrier a = { value: a }
instance Eq (Carrier a) where Predicate a =
    eq (left: Carrier a) (right: Carrier a) -> Bool = evaluate left.value && evaluate right.value
type Outer a = { wrapped: Carrier a } deriving (Eq)
fn pureValue (value: Int) -> Bool = value == 1
fn wrongParameter (value: Bool) -> Bool = value
fn wrongReturn (value: Int) -> Int = value
fn io effect (value: Int) -> Bool =
    Io.println "effect"
    value == 1
pub fn exercise () -> Bool = eq (Outer { wrapped = Carrier { value = TARGET } }) (Outer { wrapped = Carrier { value = TARGET } })
"#;
    for (target, invalid) in [
        ("pureValue", false),
        ("effect", true),
        ("wrongParameter", true),
        ("wrongReturn", true),
    ] {
        let dir = workspace(&template.replace("TARGET", target));
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        if invalid {
            assert!(
                !artifacts.diagnostics.is_empty(),
                "the complete function head must match: {target}"
            );
            assert!(
                artifacts.core_files.is_empty(),
                "invalid evidence cannot reach emitted Core"
            );
        } else {
            assert!(
                artifacts.diagnostics.is_empty(),
                "{:?}",
                artifacts.diagnostics
            );
            assert!(!artifacts.core_files.is_empty());
        }
    }
    assert_true_program(&template.replace("TARGET", "pureValue"));
}

#[test]
fn function_instance_heads_use_the_declaring_modules_private_aliases() {
    let producer = r"
type Input = Int
pub class Predicate a =
    evaluate (value: a) -> Bool
instance Predicate (fn Input -> Bool) =
    evaluate (value: fn Input -> Bool) -> Bool = value 1
pub fn accepts (value: a) -> Bool where Predicate a = evaluate value
";
    let template = r"
import app.A as A
type Input = Bool
fn intValue (value: Int) -> Bool = value == 1
fn boolValue (value: Input) -> Bool = value
pub fn exercise () -> Bool = A.accepts TARGET
";
    for (target, invalid) in [("intValue", false), ("boolValue", true)] {
        let dir = workspace(&template.replace("TARGET", target));
        std::fs::write(dir.path().join("app/src/A.ridge"), producer).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        if invalid {
            assert!(!artifacts.diagnostics.is_empty());
            assert!(artifacts.core_files.is_empty());
        } else {
            assert!(
                artifacts.diagnostics.is_empty(),
                "{:?}",
                artifacts.diagnostics
            );
            assert!(!artifacts.core_files.is_empty());
        }
    }
}

#[test]
fn promoted_function_wrappers_preserve_their_complete_head() {
    let template = r#"
import std.io as Io
type Callable a = { run: a }
pub fn toText (value: Callable (fn Int -> Bool)) -> Text =
    if value.run 1 then "yes" else "no"
fn pureValue (value: Int) -> Bool = value == 1
fn io effect (value: Int) -> Bool =
    Io.println "effect"
    value == 1
pub fn exercise () -> Text = toText (Callable { run = TARGET })
"#;
    for (target, invalid) in [("pureValue", false), ("effect", true)] {
        let dir = workspace(&template.replace("TARGET", target));
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        if invalid {
            assert!(
                !artifacts.diagnostics.is_empty(),
                "promoted evidence must check its full head"
            );
            assert!(artifacts.core_files.is_empty());
        } else {
            assert!(
                artifacts.diagnostics.is_empty(),
                "{:?}",
                artifacts.diagnostics
            );
            assert!(!artifacts.core_files.is_empty());
        }
    }
}

fn assert_true_program(source: &str) {
    assert_program_result(source, "true");
}

fn assert_program_result(source: &str, expected: &str) {
    assert_program_files(source, expected, &[]);
}

fn assert_program_files(source: &str, expected: &str, files: &[(&str, &str)]) {
    assert_program_eval(
        source,
        &format!("{expected}=ridge_app_Main:exercise(),halt()."),
        files,
    );
}

fn assert_program_eval(source: &str, expression: &str, files: &[(&str, &str)]) {
    run_program_eval(source, expression, files);
}

fn run_program_eval(
    source: &str,
    expression: &str,
    files: &[(&str, &str)],
) -> std::process::Output {
    let dir = workspace(source);
    for (name, contents) in files {
        std::fs::write(dir.path().join(format!("app/src/{name}.ridge")), contents).unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|path| path.file_stem().and_then(|name| name.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(module.parent().unwrap())
        .args(["-eval", expression])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn ordering_values_follow_their_declared_order() {
    assert_true_program(
        r"
import std.list as List
pub fn exercise () -> Bool =
    eq (compare Less Equal) Less &&
    eq (compare Equal Greater) Less &&
    eq (compare Greater Less) Greater &&
    eq (compare Equal Equal) Equal &&
    eq (List.sort [Greater, Less, Equal]) [Less, Equal, Greater]
",
    );
}

#[test]
fn comparison_containers_cover_empty_prefix_variant_and_custom_key_cases() {
    assert_program_result(
        r#"
import std.list as List
import std.bool as Bool
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id % 10 == right.id % 10
instance Ord Key =
    compare (left: Key) (right: Key) -> Ordering = compare (left.id % 10) (right.id % 10)
type Entry = { key: Key, data: Int } deriving (Eq)
fn ok (key: Key) -> Result Key Int = Ok key
fn err () -> Result Key Int = Err 0
pub fn exercise () -> List Bool =
    let a = Key { id = 11, label = "a" }
    let b = Key { id = 1, label = "b" }
    let c = Key { id = 2, label = "c" }
    let first = Entry { key = a, data = 1 }
    let second = Entry { key = c, data = 2 }
    [eq [a, c] [b, c],
    Bool.not (eq [a] []),
    eq (compare [] [a]) Less,
    eq (compare [a] []) Greater,
    eq (compare [a] [b, c]) Less,
    eq (compare [a, c] [b]) Greater,
    eq (Some a) (Some b),
    Bool.not (eq None (Some a)),
    eq (compare (Some a) None) Less,
    eq (compare None (Some a)) Greater,
    eq (ok a) (ok b),
    Bool.not (eq (ok a) (err ())),
    eq (compare (ok a) (err ())) Less,
    eq (compare (err ()) (ok a)) Greater,
    eq (List.sortBy (fn value -> value.key) [second, first]) [first, second]]
"#,
        "[true,true,true,true,true,true,true,true,true,true,true,true,true,true,true]",
    );
}

#[test]
fn derived_comparisons_retain_monomorphic_implicit_row_evidence() {
    let source = r"
import std.sql (Row, toRow)
import std.map as Map
type Person = { id: Int }
type Carrier a = { value: a }
instance Eq (Carrier a) where Row a =
    eq (left: Carrier a) (right: Carrier a) -> Bool = Map.length (toRow left.value) == Map.length (toRow right.value)
type Outer = { wrapped: Carrier Person } deriving (Eq)
pub fn exercise () -> Bool =
    eq (Outer { wrapped = Carrier { value = Person { id = 1 } } }) (Outer { wrapped = Carrier { value = Person { id = 2 } } })
";
    assert_true_program(source);
    let imported = source.replace("type Person = { id: Int }", "import app.A (Person)");
    assert_program_files(
        &imported,
        "true",
        &[(
            "A",
            "pub type Entity = { id: Int } deriving (Schema)\npub type Person = { id: Int }\n",
        )],
    );
}

#[test]
fn implicit_rows_respect_opaque_construction() {
    let source = r#"
import app.A (Age)
import std.sql as Sql
import std.map as Map
pub fn exercise () -> Result Age Error = Sql.fromRow (Map.fromList [("value", Sql.toSql (-1))])
"#;
    for (opaque, derives, invalid) in [
        ("", "", false),
        ("opaque ", "", true),
        ("opaque ", " deriving (Row)", false),
    ] {
        let dir = workspace(source);
        std::fs::write(dir.path().join("app/src/A.ridge"), format!(r"
import std.sql (Row)
pub {opaque}type Age = {{ value: Int }}{derives}
pub fn create (value: Int) -> Option Age = if value >= 0 then Some (Age {{ value = value }}) else None
pub fn read (age: Age) -> Int = age.value
")).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        if invalid {
            assert!(
                artifacts
                    .diagnostics
                    .iter()
                    .any(|error| error.code == "T029"),
                "opaque construction needs its owner's explicit contract: {:?}",
                artifacts.diagnostics
            );
            assert!(artifacts.core_files.is_empty());
        } else {
            assert!(
                artifacts.diagnostics.is_empty(),
                "{:?}",
                artifacts.diagnostics
            );
            assert!(!artifacts.core_files.is_empty());
        }
    }
}

#[test]
fn derived_comparisons_preserve_composite_query_context_evidence() {
    let source = r"
import std.repo (Query, Joined, Refinable)
import std.data (MemAdapter)
type Person = { id: Int }
type Carrier q p = { receiver: q, predicate: p }
instance Eq (Carrier q p) where Refinable q p =
    eq (left: Carrier q p) (right: Carrier q p) -> Bool = true
type Outer = { value: Carrier (Joined (Query Person MemAdapter) Person MemAdapter) (fn Person Person -> Bool) } deriving (Eq)
pub fn exercise () -> Bool = true
";
    let dir = workspace(source);
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    assert!(!artifacts.core_files.is_empty());
}

#[test]
fn value_comparisons_preserve_decimal_scale_and_core_value_contracts() {
    assert_true_program(
        r#"
import std.bytes as Bytes
import std.uuid as Uuid
import std.date as Date
import std.timeofday as Time
import std.time as Clock (Duration)
import std.proc (Output)
import std.decimal as Decimal
fn same left right = eq left right
fn order left right = compare left right
fn dates () -> Bool =
    match (Date.fromYmd 2026 10 29, Date.fromIso "2026-10-29", Date.fromIso "2026-10-30")
        (Ok a, Ok b, Ok c) -> same a b && order a c == Less
        _ -> false
fn times () -> Bool =
    match (Time.fromIso "01:02:03.000001", Time.fromIso "01:02:03.000002")
        (Ok a, Ok b) -> same a a && order a b == Less
        _ -> false
fn timestamps () -> Bool =
    match (Clock.fromIso "2026-10-29T00:00:00.000001Z", Clock.fromIso "2026-10-29T00:00:00.000002Z")
        (Ok a, Ok b) -> same a a && order a b == Less && Clock.diffMs a b == 0
        _ -> false
fn uuids () -> Bool =
    match (Uuid.fromText "00000000-0000-4000-8000-00000000000a", Uuid.fromText "00000000-0000-4000-8000-00000000000A")
        (Ok a, Ok b) -> same a b && order a b == Equal
        _ -> false
pub fn exercise () -> Bool =
    let a = 1.5m
    let b = 1.50m
    same a b && order a b == Equal && order (Decimal.neg 2.0m) (Decimal.neg 1.0m) == Less &&
    same 0.0m (Decimal.neg 0.00m) && Decimal.toText a == "1.5" && Decimal.toText b == "1.50" &&
    same () () &&
    same (Bytes.fromUtf8 "a") (Bytes.fromUtf8 "a") && order (Bytes.fromUtf8 "a") (Bytes.fromUtf8 "b") == Less &&
    same (Clock.ofMillis 1) (Duration { ms = 1 }) && order (Clock.ofMillis 1) (Clock.ofMillis 2) == Less &&
    same (Output { stdout = "out", stderr = "", exitCode = 0 }) (Output { stdout = "out", stderr = "", exitCode = 0 }) &&
    same (Decimal.fromText "invalid") (Decimal.fromText "invalid") &&
    dates () && times () && timestamps () && uuids ()
"#,
    );
}

#[test]
fn list_membership_and_unique_use_component_equality_and_keep_first_values() {
    let source = r#"
import std.list as List
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id == right.id
fn member value values = List.contains value values
fn distinct (values: List a) -> List a where Eq a = List.unique values
pub fn exercise () -> Bool =
    let first = Key { id = 1, label = "first" }
    let later = Key { id = 1, label = "later" }
    let other = Key { id = 2, label = "other" }
    let values = [first, other, later, other]
    member later [first] &&
    List.contains first [] == false &&
    List.contains other [first] == false &&
    List.map (fn key -> key.label) (distinct values) == ["first", "other"] &&
    List.contains 1.50m [1.5m] && List.length (List.unique [1.5m, 1.50m, 2.0m]) == 2 &&
    List.map (fn value -> member value values) [later, other] == [true, true]
"#;
    assert_true_program(source);
}

#[test]
fn list_equality_consumers_require_supported_and_declared_evidence() {
    for (source, code) in [
        (
            "pub fn invalid () -> Bool = List.contains 1.0 [1.0]\n",
            "T029",
        ),
        (
            "pub fn invalid () -> List Float = List.unique [1.0]\n",
            "T029",
        ),
        (
            "pub fn invalid (value: a) (values: List a) -> Bool = List.contains value values\n",
            "T055",
        ),
        (
            "pub fn invalid (values: List a) -> List a = List.unique values\n",
            "T055",
        ),
        (
            "pub fn invalid (value: fn Int -> Int) -> Bool = List.contains value [value]\n",
            "T029",
        ),
    ] {
        let dir = workspace(source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            artifacts.diagnostics.iter().any(|error| error.code == code),
            "{source}: {:?}",
            artifacts.diagnostics
        );
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn quoted_float_membership_retains_sql_parameters_without_ordinary_eq() {
    let source = r#"
import std.query (planScan, planToSql)
import std.sql (sqlValue)
type Price = { amount: Float } deriving (Row)
fn predicate (value: Quote (Price -> Bool)) -> QExpr = value.tree
pub fn exercise () -> Bool =
    let amounts = [1.5, 2.5]
    let tree = predicate (fn price -> List.contains price.amount amounts)
    let plan = planScan "prices" tree [] (0 - 1) 0 false
    match planToSql plan
        (sql, parameters) ->
            sqlValue sql == "SELECT * FROM \"prices\" WHERE \"amount\" IN ($1, $2)" &&
            List.length parameters == 2
"#;
    assert_true_program(source);
}

#[test]
fn value_comparison_instances_keep_resource_and_function_boundaries() {
    for source in [
        "pub fn invalid (value: Instant) -> Bool = eq value value\n",
        "pub fn invalid (value: JsonValue) -> Bool = eq value value\n",
        "import std.sql (SqlValue)\npub fn invalid (value: SqlValue) -> Bool = eq value value\n",
        "pub fn invalid () -> Ordering = compare true false\n",
        "pub fn invalid () -> Ordering = compare () ()\n",
    ] {
        let dir = workspace(source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            artifacts
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "T029"),
            "{source}: {:?}",
            artifacts.diagnostics
        );
        assert!(artifacts.core_files.is_empty());
    }
    let source = "pub fn sameHandle (left: Handle a) (right: Handle a) -> Bool = eq left right\npub fn sameSupervisor (left: Supervisor a) (right: Supervisor a) -> Bool = eq left right\npub fn sameMonitor (left: Monitor) (right: Monitor) -> Bool = eq left right\n";
    let dir = workspace(source);
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    assert!(!artifacts.core_files.is_empty());
}

#[test]
fn actor_comparisons_use_stable_identity_without_mailbox_configuration() {
    let source = r"
pub fn sameHandle (left: Handle a) (right: Handle a) -> Bool = eq left right
pub fn sameSupervisor (left: Supervisor a) (right: Supervisor a) -> Bool = eq left right
pub fn sameMonitor (left: Monitor) (right: Monitor) -> Bool = eq left right
";
    // Exercise the backend adapter with valid runtime representations. No actor
    // state or liveness query may participate in these pure Eq methods.
    let expression = r#"
P=self(), Q=spawn(fun() -> receive stop -> ok end end),
R=make_ref(), S=make_ref(),
true=ridge_app_Main:sameHandle({ridge_handle,P,unbounded},{ridge_handle,P,{bounded,1,error}}),
false=ridge_app_Main:sameHandle({ridge_handle,P,unbounded},{ridge_handle,Q,unbounded}),
true=ridge_app_Main:sameHandle({ridge_sup_handle,P,<<"child">>,unbounded},{ridge_sup_handle,P,<<"child">>,{bounded,1,error}}),
false=ridge_app_Main:sameHandle({ridge_sup_handle,P,<<"a">>,unbounded},{ridge_sup_handle,P,<<"b">>,unbounded}),
false=ridge_app_Main:sameHandle({ridge_sup_handle,P,<<"a">>,unbounded},{ridge_sup_handle,Q,<<"a">>,unbounded}),
false=ridge_app_Main:sameHandle({ridge_handle,P,unbounded},{ridge_sup_handle,P,<<"a">>,unbounded}),
true=ridge_app_Main:sameSupervisor({ridge_sup,P},{ridge_sup,P}),
false=ridge_app_Main:sameSupervisor({ridge_sup,P},{ridge_sup,Q}),
true=ridge_app_Main:sameMonitor(R,R), false=ridge_app_Main:sameMonitor(R,S),
Q!stop, halt().
"#;
    assert_program_eval(source, expression, &[]);
}

#[test]
fn collection_callbacks_receive_eq_from_a_hash_promise() {
    let source = r#"
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
fn invoke (writer: fn a Hasher -> Hasher) (same: fn a a -> Bool) (left: a) (right: a) (hasher: Hasher) -> Hasher =
    if same left right then writer left hasher else hasher
fn use (left: a) (right: a) (hasher: Hasher) -> Hasher where Hash a = invoke hash eq left right hasher
pub fn exercise (hasher: Hasher) -> Hasher =
    use (Key { id = 7, label = "first" }) (Key { id = 7, label = "second" }) hasher
"#;
    let dir = workspace(source);
    let checked = ridge_driver::check_workspace_incremental(
        ridge_driver::CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true),
    )
    .unwrap();
    assert!(checked.type_errors.is_empty(), "{:?}", checked.type_errors);
    assert_program_eval(source,
        "S=ridge_rt:hash_new(<<0:128>>),Expected=ridge_rt:hash_int(7,S),Expected=ridge_app_Main:exercise(S),halt().", &[]);
}

#[test]
fn persistent_collection_kernel_uses_ridge_evidence_and_retains_representatives() {
    let source = r"
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
pub fn key (id: Int) (label: Text) -> Key = Key { id = id, label = label }
pub fn same (left: Key) (right: Key) -> Bool = eq left right
pub fn write (value: Key) (hasher: Hasher) -> Hasher = hash value hasher
pub fn collision (_value: Key) (hasher: Hasher) -> Hasher = hasher
pub fn decimalSame (left: Decimal) (right: Decimal) -> Bool = eq left right
";
    let expression = r#"
H=fun ridge_app_Main:write/2, E=fun ridge_app_Main:same/2,
C=fun ridge_app_Main:collision/2, VE=fun ridge_app_Main:decimalSame/2,
K=fun ridge_app_Main:key/2, A=K(1,<<"first">>), Alias=K(1,<<"later">>),
B=K(2,<<"second">>), D=K(3,<<"third">>), Missing=K(9,<<"absent">>),
Empty=ridge_rt:typed_map_empty(), 0=ridge_rt:typed_map_size(Empty),
Never=fun(_,_) -> error(unexpected_callback) end,
none=ridge_rt:typed_map_get(Never,Never,A,Empty),
Empty=ridge_rt:typed_map_remove(Never,Never,A,Empty),
M=ridge_rt:typed_map_from_list(H,E,[{A,10},{B,20},{Alias,30}]),
2=ridge_rt:typed_map_size(M), [{A,30},{B,20}]=lists:sort(ridge_rt:typed_map_to_list(M)),
{some,30}=ridge_rt:typed_map_get(H,E,Alias,M), none=ridge_rt:typed_map_get(H,E,Missing,M),
true=ridge_rt:typed_map_contains(H,E,Alias,M),
false=ridge_rt:typed_map_contains(H,E,Missing,M),
Removed=ridge_rt:typed_map_remove(H,E,Alias,M), 1=ridge_rt:typed_map_size(Removed),
none=ridge_rt:typed_map_get(H,E,A,Removed), {some,30}=ridge_rt:typed_map_get(H,E,A,M),
Left=ridge_rt:typed_map_from_list(H,E,[{A,1},{B,2}]),
Right=ridge_rt:typed_map_from_list(H,E,[{Alias,9},{D,3}]),
Merged=ridge_rt:typed_map_merge(H,E,Left,Right),
[{A,9},{B,2},{D,3}]=lists:sort(ridge_rt:typed_map_to_list(Merged)),
[{A,1},{B,2}]=lists:sort(ridge_rt:typed_map_to_list(Left)),
put(visits,[]), Mapped=ridge_rt:typed_map_map(fun(Key,V) ->
 put(visits,[Key|get(visits)]), fun() -> V end end,M),
[A,B]=lists:sort(get(visits)), {some,Thunk}=ridge_rt:typed_map_get(H,E,Alias,Mapped),
30=Thunk(), 2=ridge_rt:typed_map_size(Mapped),
put(visits,[]), Filtered=ridge_rt:typed_map_filter_map(fun(Key,V) ->
 put(visits,[Key|get(visits)]), case V of 30 -> {some,V+1}; _ -> none end end,M),
[A,B]=lists:sort(get(visits)), [{A,31}]=ridge_rt:typed_map_to_list(Filtered),
0=ridge_rt:typed_map_size(ridge_rt:typed_map_filter(fun(_,_) -> false end,M)),
Collided=ridge_rt:typed_map_from_list(C,E,[{A,1},{B,2},{D,3},{Alias,9}]),
3=ridge_rt:typed_map_size(Collided), {some,9}=ridge_rt:typed_map_get(C,E,Alias,Collided),
[{A,9},{D,3}]=lists:sort(ridge_rt:typed_map_to_list(ridge_rt:typed_map_remove(C,E,B,Collided))),
none=ridge_rt:typed_map_get(C,E,Missing,Collided),
DecA=ridge_rt:typed_map_from_list(H,E,[{A,{decimal,10,1}},{B,{decimal,20,1}}]),
DecB=ridge_rt:typed_map_from_list(H,E,[{B,{decimal,2,0}},{Alias,{decimal,1,0}}]),
true=ridge_rt:typed_map_eq(H,E,VE,DecA,DecB),
false=ridge_rt:typed_map_eq(H,E,VE,DecA,ridge_rt:typed_map_insert(H,E,A,{decimal,3,0},DecB)),
false=ridge_rt:typed_map_eq(H,E,VE,DecA,Empty),
S=ridge_rt:typed_set_from_list(C,E,[A,B,Alias]), T=ridge_rt:typed_set_from_list(C,E,[Alias,D]),
[A,B]=lists:sort(ridge_rt:typed_map_keys(S)),
[A,B,D]=lists:sort(ridge_rt:typed_map_keys(ridge_rt:typed_map_merge(C,E,S,T))),
[A]=ridge_rt:typed_map_keys(ridge_rt:typed_set_intersection(C,E,S,T)),
[B]=ridge_rt:typed_map_keys(ridge_rt:typed_set_difference(C,E,S,T)),
put(eq_calls,0), CountEq=fun(X,Y) -> put(eq_calls,get(eq_calls)+1), E(X,Y) end,
{some,30}=ridge_rt:typed_map_get(H,CountEq,Alias,M), 1=get(eq_calls),
put(eq_calls,0), none=ridge_rt:typed_map_get(H,CountEq,Missing,M), 0=get(eq_calls),
CountHash=fun(Key,Hasher) -> put(hash_calls,get(hash_calls)+1), C(Key,Hasher) end,
lists:foreach(fun({Key,Before,ExpectedEq,ExpectedSize,Representative}) ->
 put(hash_calls,0), put(eq_calls,0),
 Before=ridge_rt:typed_map_get(CountHash,CountEq,Key,Collided),
 Split=ridge_rt:typed_map_insert(CountHash,CountEq,Key,99,Collided),
 2=get(hash_calls), SplitEq=2*ExpectedEq, SplitEq=get(eq_calls),
 put(hash_calls,0), put(eq_calls,0), put(update_calls,0),
 Updated=ridge_rt:typed_map_update(CountHash,CountEq,Key,fun(Current) ->
   Before=Current, put(update_calls,get(update_calls)+1), 99 end,Collided),
 1=get(hash_calls), ExpectedEq=get(eq_calls), 1=get(update_calls),
 ExpectedPairs=lists:sort(ridge_rt:typed_map_to_list(Split)),
 ExpectedPairs=lists:sort(ridge_rt:typed_map_to_list(Updated)),
 ExpectedSize=ridge_rt:typed_map_size(Updated),
 {some,99}=ridge_rt:typed_map_get(C,E,Key,Updated),
 [{Representative,99}]=[{Stored,Value} || {Stored,Value} <- ridge_rt:typed_map_to_list(Updated), E(Key,Stored)],
 [{A,9},{B,2},{D,3}]=lists:sort(ridge_rt:typed_map_to_list(Collided))
end,[{Alias,{some,9},1,3,A},{D,{some,3},3,3,D},{Missing,none,3,4,Missing}]),
put(hash_calls,0), put(update_calls,0),
First=ridge_rt:typed_map_update(CountHash,Never,A,fun(none) ->
 put(update_calls,get(update_calls)+1), 11 end,Empty),
1=get(hash_calls), 1=get(update_calls), [{A,11}]=ridge_rt:typed_map_to_list(First),
put(update_calls,0),
try ridge_rt:typed_map_update(C,E,Alias,fun({some,9}) ->
 put(update_calls,get(update_calls)+1), error(update_failed) end,Collided) of
 _ -> error(expected_update_failure)
catch error:update_failed -> ok end,
1=get(update_calls), {some,9}=ridge_rt:typed_map_get(C,E,Alias,Collided),
halt().
"#;
    assert_program_eval(source, expression, &[]);
}

fn evidence_recipes(workspace: &ridge_ir::LoweredWorkspace) -> Vec<String> {
    workspace
        .modules
        .iter()
        .flatten()
        .flat_map(|module| {
            module.items.iter().filter_map(|item| match item {
                ridge_ir::IrItem::Fn(function) if function.name == "$ridge_evidence" => {
                    Some(function)
                }
                _ => None,
            })
        })
        .flat_map(|function| {
            let ridge_ir::IrExpr::Match { arms, .. } = &function.body else {
                panic!("factory must dispatch on the recipe")
            };
            arms.iter().filter_map(|arm| {
                if matches!(arm.pat, ridge_ir::IrPat::Wild { .. }) {
                    return None;
                }
                let ridge_ir::IrPat::Tuple { elems, .. } = &arm.pat else {
                    panic!("recipe and context must be matched together")
                };
                let ridge_ir::IrPat::Lit {
                    value: ridge_ir::IrLit::Text(identity),
                    ..
                } = &elems[0]
                else {
                    panic!("recipe identity must be text")
                };
                Some(identity.clone())
            })
        })
        .collect()
}

#[test]
fn collection_evidence_recipe_identity_survives_type_arena_reordering() {
    let source = "type Key = { id: Int } deriving (Eq, Hash)\npub fn write (key: Key) (hasher: Hasher) -> Hasher = hash key hasher\n";
    let mut results = Vec::new();
    for prefix in ["", "type Earlier = { sentinel: Text }\n"] {
        let dir = workspace(&format!("{prefix}{source}"));
        let checked = ridge_driver::check_workspace_incremental(
            ridge_driver::CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true),
        )
        .unwrap();
        assert!(checked.type_errors.is_empty(), "{:?}", checked.type_errors);
        let key = checked
            .typed
            .tycons
            .iter()
            .find(|tycon| tycon.name == "Key")
            .unwrap()
            .id;
        let lowered = ridge_lower::lower_workspace(&checked.typed, &checked.resolved);
        assert!(lowered.errors.is_empty());
        let recipes = evidence_recipes(&lowered.workspace);
        assert!(recipes
            .iter()
            .any(|identity| identity == "Hash:static:[(\"app.Main\", \"Key\")]:[]"));
        results.push((key, recipes));
    }
    assert_ne!(
        results[0].0, results[1].0,
        "control must actually move the type's arena slot"
    );
    assert_eq!(
        results[0].1, results[1].1,
        "retained recipes must use source identities"
    );
}

#[test]
fn collection_evidence_recipes_distinguish_complete_context_argument_positions() {
    let mut recipes = Vec::new();
    for target in ["b", "c"] {
        let source = format!(
            r#"
class Convert a b =
    convert (value: a) -> b
instance Convert Int Int =
    convert (value: Int) -> Int = value
instance Convert Int Text =
    convert (_value: Int) -> Text = "converted"
type Carrier a b c = {{ value: a, first: b, second: c }}
instance Eq (Carrier a b c) where Convert a {target} =
    eq (_left: Carrier a b c) (_right: Carrier a b c) -> Bool = true
pub fn exercise () -> Bool =
    let value = Carrier {{ value = 7, first = 0, second = "" }}
    eq value value
"#
        );
        let dir = workspace(&source);
        let checked = ridge_driver::check_workspace_incremental(
            ridge_driver::CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true),
        )
        .unwrap();
        assert!(checked.type_errors.is_empty(), "{:?}", checked.type_errors);
        let lowered = ridge_lower::lower_workspace(&checked.typed, &checked.resolved);
        assert!(lowered.errors.is_empty());
        let recipe = evidence_recipes(&lowered.workspace)
            .into_iter()
            .find(|identity| identity.starts_with("Eq:static:") && identity.contains("Carrier"))
            .unwrap();
        recipes.push(recipe);
    }
    assert!(recipes[0].contains("[0, 1]"), "{}", recipes[0]);
    assert!(recipes[1].contains("[0, 2]"), "{}", recipes[1]);
    assert_ne!(
        recipes[0], recipes[1],
        "changing a secondary context argument must invalidate its provider"
    );
}

#[test]
fn collection_evidence_recipes_distinguish_specialized_instance_heads() {
    let mut recipes = Vec::new();
    for (target, value) in [("Int", "7"), ("Text", "\"seven\"")] {
        let source = format!(
            r"
class Mark a =
    mark (value: a) -> Bool
type Box a = {{ value: a }}
instance Mark (Box {target}) =
    mark (_value: Box {target}) -> Bool = true
pub fn exercise () -> Bool = mark (Box {{ value = {value} }})
"
        );
        let dir = workspace(&source);
        let checked = ridge_driver::check_workspace_incremental(
            ridge_driver::CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true),
        )
        .unwrap();
        assert!(checked.type_errors.is_empty(), "{:?}", checked.type_errors);
        let lowered = ridge_lower::lower_workspace(&checked.typed, &checked.resolved);
        assert!(lowered.errors.is_empty());
        let recipe = evidence_recipes(&lowered.workspace)
            .into_iter()
            .find(|identity| {
                identity.starts_with("app.Main::Mark:static:") && identity.contains("Box")
            })
            .unwrap();
        assert!(recipe.contains(":head:"), "{recipe}");
        recipes.push(recipe);
    }
    assert_ne!(
        recipes[0], recipes[1],
        "a changed specialization must not accept captured evidence for the previous head"
    );
}

#[test]
fn collection_evidence_is_emitted_from_typed_instance_and_structural_plans() {
    let source = r"
type Key = { id: Int } deriving (Eq, Hash)
pub fn key (id: Int) -> Key = Key { id = id }
pub fn write (value: (Key, List Int)) (hasher: Hasher) -> Hasher = hash value hasher
pub fn same (left: (Key, List Int)) (right: (Key, List Int)) -> Bool = eq left right
";
    let expression = r#"
K=ridge_app_Main:key(7), S=ridge_rt:hash_new(<<0:128>>),
A={K,[1,2]}, B={K,[1,3]},
true=ridge_app_Main:same(A,A), false=ridge_app_Main:same(A,B),
H=ridge_app_Main:write(A,S), H=ridge_app_Main:write(A,S),
KeyId = <<"Hash:static:[(\"app.Main\", \"Key\")]:[]">>,
KeyEvidence=ridge_rt:evidence_bind(ridge_app_Main,KeyId,[]),
Writer=maps:get(hash,KeyEvidence),
Expected=ridge_rt:hash_int(7,S),
%% A derived record prefixes its shape, so compare its method with the
%% dictionary's actual generated instance rather than guessing its encoding.
Raw=ridge_app_Main:'$inst_Hash_Key'(),
RawWriter=maps:get(hash,Raw), Encoded=RawWriter(K,S), Encoded=Writer(K,S),
false=(Expected =:= Encoded),
KeyEvidence=ridge_rt:evidence_current(KeyEvidence),
halt().
"#;
    assert_program_eval(source, expression, &[]);
}

#[test]
fn collection_evidence_refreshes_after_a_loader_failure_following_a_code_swap() {
    let expression = r#"
Module=ridge_evidence_failed_load_probe,
Parse=fun(Text) -> {ok,Tokens,_}=erl_scan:string(Text),
 {ok,Form}=erl_parse:parse_form(Tokens), Form end,
Binary=fun(Value) ->
 Forms=[Parse(Text) || Text <- ["-module(" ++ atom_to_list(Module) ++ ").",
  "-export(['$ridge_evidence'/2,'__ridge_state_fields'/0,'__ridge_state_defaults'/0,init/1,'__ridge_init'/1,handle_call/3,handle_cast/2]).",
  "init(A) -> ridge_rt:actor_init(" ++ atom_to_list(Module) ++ ", A).",
  "'__ridge_init'([]) -> {ok,kept}.",
  "handle_call(_,_,S) -> {reply,S,S}.",
  "handle_cast(_,S) -> {noreply,S}.",
  "'__ridge_state_defaults'() -> #{}.",
  "'__ridge_state_fields'() -> " ++ (case Value of 1 -> "[]";
    2 -> "throw({bundle_rejected,metadata_failure})" end) ++ ".",
  "'$ridge_evidence'(<<\"value\">>,[]) -> {some,{#{value => fun() -> " ++
    integer_to_list(Value) ++ " end},none}}."]],
 {ok,Module,Bin}=compile:forms(Forms,[return_errors]), Bin end,
{module,Module}=code:load_binary(Module,"old-evidence",Binary(1)),
{ok,Actor}=gen_server:start(Module,[],[]),
ActorRef=monitor(process,Actor),
ok=ridge_rt:invalidate_record_versions([Module]),
Original=ridge_rt:evidence_bind(Module,<<"value">>,[]),
1=(maps:get(value,Original))(),
persistent_term:put(ridge_loader_vsn,<<"evidence-base">>),
Name=atom_to_binary(Module),
Manifest=iolist_to_binary(json:encode(#{<<"base_vsn">> => <<"evidence-base">>,
 <<"new_vsn">> => <<"evidence-next">>,
 <<"modules">> => [Name], <<"actors">> => [#{<<"beam">> => Name,<<"renames">> => []}]})),
{error,{upgrade_failed,throw,{bundle_rejected,metadata_failure},_}}=
 ridge_loader:apply_bundle(Manifest,[{Name,Binary(2)}],
 #{base_vsn => <<"evidence-base">>,purge_after_ms => 0}),
%% Establish that the injected failure occurred after a real code swap.
%% Valid bytecode was committed; its actor metadata fails during migration.
%% A callback's thrown term cannot masquerade as a preflight rejection.
{some,{Raw,none}}=Module:'$ridge_evidence'(<<"value">>,[]), 2=(maps:get(value,Raw))(),
%% Recovery rolls forward confirmed code and never resumes an actor whose
%% migration failed. A new process uses the loaded implementation's init.
<<"evidence-next">>=ridge_loader:current_version(),
receive {'DOWN',ActorRef,process,Actor,killed} -> ok
after 1000 -> error(incomplete_actor_survived) end,
false=is_process_alive(Actor),
{ok,Fresh}=gen_server:start(Module,[],[]),
kept=gen_server:call(Fresh,probe,500),
Current=ridge_rt:evidence_current(Original),
2=(maps:get(value,Current))(),
{error,{base_version_mismatch,<<"evidence-base">>,<<"evidence-next">>}}=
 ridge_loader:apply_bundle(Manifest,[{Name,Binary(2)}],
 #{base_vsn => <<"evidence-base">>,purge_after_ms => 0}),
ok=gen_server:stop(Fresh),
halt().
"#;
    assert_program_eval("pub fn exercise () -> Bool = true\n", expression, &[]);
}

#[test]
fn timestamp_text_evidence_and_interpolation_use_the_published_iso_conversion() {
    assert_true_program(
        r#"
import std.time as Time
import std.text as Text
fn render (value: a) -> Text where ToText a = toText value
pub fn exercise () -> Bool =
    let value = Time.epoch ()
    let expected = Time.toIso value
    Text.startsWith "1970-01-01T00:00:00" expected &&
    eq (render value) expected && eq $"${value}" expected
"#,
    );
}

#[test]
fn collection_evidence_last_use_named_provider_survives_a_source_reload() {
    let source = r"
type Key = { id: Int }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
pub fn key () -> Key = Key { id = 7 }
pub fn write (value: Key) (hasher: Hasher) -> Hasher = hash value hasher
";
    let replacement = source.replace("= hash value hasher", "= hasher");
    assert_evidence_source_reload(
        source,
        &replacement,
        r#"
Module=ridge_app_Main, Key=Module:key(), S=ridge_rt:hash_new(<<0:128>>),
Identity = <<"Hash:static:[(\"app.Main\", \"Key\")]:[]">>,
Original=ridge_rt:evidence_bind(Module,Identity,[]),
Expected=ridge_rt:hash_int(7,S), Expected=(maps:get(hash,Original))(Key,S),
{ok,Binary}=file:read_file(Next),
{module,Module}=code:load_binary(Module,Next,Binary),
ok=ridge_rt:invalidate_record_versions([Module]),
S=Module:write(Key,S),
Current=ridge_rt:evidence_current(Original),
Expected=(maps:get(hash,Current))(Key,S),
true=code:soft_purge(Module), {ok,ThirdBinary}=file:read_file(ThirdPath),
{module,Module}=code:load_binary(Module,ThirdPath,ThirdBinary),
ok=ridge_rt:invalidate_record_versions([Module]),
Third=ridge_rt:evidence_current(Original),
ExpectedThird=ridge_rt:hash_int(8,S), ExpectedThird=(maps:get(hash,Third))(Key,S), halt().
"#,
    );
}

#[test]
fn collection_evidence_last_use_structural_provider_survives_a_source_reload() {
    let source = r"
type Key = { id: Int }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
pub fn key () -> Key = Key { id = 7 }
pub fn keep (value: Key) (hasher: Hasher) -> Hasher = hash value hasher
pub fn write (value: (Key, Key)) (hasher: Hasher) -> Hasher = hash value hasher
";
    let replacement = source.replace(
        "pub fn write (value: (Key, Key)) (hasher: Hasher) -> Hasher = hash value hasher",
        "pub fn write (_value: (Key, Key)) (hasher: Hasher) -> Hasher = hasher",
    );
    assert_evidence_source_reload(
        source,
        &replacement,
        r#"
Module=ridge_app_Main, Key=Module:key(), Pair={Key,Key}, S=ridge_rt:hash_new(<<0:128>>),
LeafId = <<"Hash:static:[(\"app.Main\", \"Key\")]:[]">>,
Leaf=ridge_rt:evidence_bind(Module,LeafId,[]),
Original=ridge_rt:evidence_bind(Module,<<"Hash:structural:None:2">>,[Leaf,Leaf]),
Tagged=ridge_rt:hash_tag({<<"tuple">>,0,2,[]},S),
Expected=ridge_rt:hash_int(7,ridge_rt:hash_int(7,Tagged)),
Expected=(maps:get(hash,Original))(Pair,S),
{ok,Binary}=file:read_file(Next),
{module,Module}=code:load_binary(Module,Next,Binary),
ok=ridge_rt:invalidate_record_versions([Module]),
S=Module:write(Pair,S),
LeafNow=ridge_rt:evidence_current(Leaf),
Single=ridge_rt:hash_int(7,S), Single=(maps:get(hash,LeafNow))(Key,S),
Current=ridge_rt:evidence_current(Original),
Expected=(maps:get(hash,Current))(Pair,S),
try (maps:get(hash,Current))({Key,Key,Key},S) of
 _ -> error(expected_tuple_arity_failure)
catch error:invalid_structural_value -> ok end,
true=code:soft_purge(Module), {ok,ThirdBinary}=file:read_file(ThirdPath),
{module,Module}=code:load_binary(Module,ThirdPath,ThirdBinary),
ok=ridge_rt:invalidate_record_versions([Module]),
Third=ridge_rt:evidence_current(Original),
ExpectedThird=ridge_rt:hash_int(8,ridge_rt:hash_int(8,Tagged)),
ExpectedThird=(maps:get(hash,Third))(Pair,S), halt().
"#,
    );
}

#[test]
fn collection_evidence_last_use_record_operations_preserve_component_semantics() {
    let source = r"
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Ord Key =
    compare (left: Key) (right: Key) -> Ordering = compare left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
pub fn key (id: Int) (label: Text) -> Key = Key { id = id, label = label }
pub fn same (left: { a: Key, b: Key }) (right: { a: Key, b: Key }) -> Bool = eq left right
pub fn order (left: { a: Key, b: Key }) (right: { a: Key, b: Key }) -> Ordering = compare left right
pub fn write (value: { a: Key, b: Key }) (hasher: Hasher) -> Hasher = hash value hasher
";
    let replacement = source
        .replace("= eq left right", "= true")
        .replace("= compare left right", "= Equal")
        .replace("= hash value hasher", "= hasher");
    assert_evidence_source_reload(
        source,
        &replacement,
        r#"
Module=ridge_app_Main, K=fun Module:key/2, S=ridge_rt:hash_new(<<0:128>>),
A=#{a => K(7,<<"first">>),b => K(9,<<"last">>)},
Alias=#{a => K(7,<<"alias">>),b => K(9,<<"other">>)},
B=#{a => K(7,<<"first">>),b => K(10,<<"last">>)}, Partial=#{a => K(8,<<"stop">>)},
LeafId=fun(Class) -> <<Class/binary,":static:[(\"app.Main\", \"Key\")]:[]">> end,
RootId=fun(Class) -> <<Class/binary,":structural:Some([\"a\", \"b\"]):2">> end,
Bind=fun(Class) -> Leaf=ridge_rt:evidence_bind(Module,LeafId(Class),[]),
 ridge_rt:evidence_bind(Module,RootId(Class),[Leaf,Leaf]) end,
Eq=Bind(<<"Eq">>), Ord=Bind(<<"Ord">>), Hash=Bind(<<"Hash">>),
true=(maps:get(eq,Eq))(A,Alias), false=(maps:get(eq,Eq))(A,B),
'Less'=(maps:get(compare,Ord))(A,B),
Tagged=ridge_rt:hash_tag({<<"record">>,0,2,[<<"a">>,<<"b">>]},S),
Expected=ridge_rt:hash_int(9,ridge_rt:hash_int(7,Tagged)),
Expected=(maps:get(hash,Hash))(A,S),
{ok,Binary}=file:read_file(Next), {module,Module}=code:load_binary(Module,Next,Binary),
ok=ridge_rt:invalidate_record_versions([Module]),
true=Module:same(A,B), 'Equal'=Module:order(A,B), S=Module:write(A,S),
EqNow=ridge_rt:evidence_current(Eq), OrdNow=ridge_rt:evidence_current(Ord),
HashNow=ridge_rt:evidence_current(Hash),
true=(maps:get(eq,EqNow))(A,Alias), false=(maps:get(eq,EqNow))(A,B),
false=(maps:get(eq,EqNow))(A,Partial),
'Less'=(maps:get(compare,OrdNow))(A,B), 'Equal'=(maps:get(compare,OrdNow))(A,Alias),
'Greater'=(maps:get(compare,OrdNow))(B,A), 'Less'=(maps:get(compare,OrdNow))(A,Partial),
Expected=(maps:get(hash,HashNow))(A,S), Expected=(maps:get(hash,HashNow))(Alias,S),
true=code:soft_purge(Module), {ok,ThirdBinary}=file:read_file(ThirdPath),
{module,Module}=code:load_binary(Module,ThirdPath,ThirdBinary),
ok=ridge_rt:invalidate_record_versions([Module]),
Third=ridge_rt:evidence_current(Hash),
ExpectedThird=ridge_rt:hash_int(10,ridge_rt:hash_int(8,Tagged)),
ExpectedThird=(maps:get(hash,Third))(A,S), halt().
"#,
    );
}

#[test]
fn old_collection_aliases_reuse_bounded_data_only_indexes_across_reloads() {
    let expression = r#"
Parse=fun(Text) -> {ok,Tokens,_}=erl_scan:string(Text), {ok,Form}=erl_parse:parse_form(Tokens), Form end,
Module=ridge_collection_cache_probe,
Load=fun(Offset) ->
 Forms=[Parse(Text) || Text <- ["-module(ridge_collection_cache_probe).",
 "-export(['$ridge_evidence'/2]).",
 "'$ridge_evidence'(<<\"hash\">>,[]) -> case erase(cache_reload_hook) of F when is_function(F,0) -> F(); _ -> ok end, " ++
 "{some,{#{hash => fun(K,S) -> put(hashes,get(hashes)+1),ridge_rt:hash_int(K+" ++ integer_to_list(Offset) ++ ",S) end},none}}; " ++
 "'$ridge_evidence'(<<\"eq\">>,[]) -> {some,{#{eq => fun(A,B) -> A =:= B end},none}}."]],
 {ok,Module,Binary}=compile:forms(Forms,[return_errors]),
 {module,Module}=code:load_binary(Module,"collection-cache-probe",Binary),
 ok=ridge_rt:invalidate_record_versions([Module]) end,
put(hashes,0), Load(0),
H=ridge_rt:evidence_bind(Module,<<"hash">>,[]), E=ridge_rt:evidence_bind(Module,<<"eq">>,[]),
Evidence=ridge_rt:key_evidence(H,E),
Original=ridge_rt:indexed_map_from_list(Evidence,[{I,I} || I <- lists:seq(1,1024)]),
ok=ridge_rt:invalidate_record_versions([ridge_unrelated_cache_probe]),
put(hashes,0),
lists:foreach(fun(_) -> {some,1}=ridge_rt:indexed_map_get(Evidence,1,Original) end,lists:seq(1,20)),
1044=get(hashes), 1024=ridge_rt:indexed_map_size(Original), 1044=get(hashes),
Modified=ridge_rt:indexed_map_insert(Evidence,1,999,Original),
{some,999}=ridge_rt:indexed_map_get(Evidence,1,Modified),
{some,1}=ridge_rt:indexed_map_get(Evidence,1,Original),
Closure=ridge_rt:indexed_map_from_list(Evidence,[{1,fun() -> callable end}]),
Big=ridge_rt:indexed_map_from_list(Evidence,[{1,binary:part(binary:copy(<<7>>,2097152),0,1000)}]),
ok=ridge_rt:invalidate_record_versions([ridge_unrelated_cache_probe]),
{some,F}=ridge_rt:indexed_map_get(Evidence,1,Closure), callable=F(),
{some,_}=ridge_rt:indexed_map_get(Evidence,1,Big),
Cached=fun(M) -> {ridge_map_snapshot,_,Identity}=element(3,M),
 case get('$ridge_collection_migrations') of
  {_,Entries,_} -> lists:keymember(Identity,1,Entries); _ -> false end end,
false=Cached(Closure), false=Cached(Big),
Maps=[ridge_rt:indexed_map_from_list(Evidence,[{I,I}]) || I <- lists:seq(1,9)],
ok=ridge_rt:invalidate_record_versions([ridge_unrelated_cache_probe]),
lists:foreach(fun({I,M}) ->
 {some,I}=ridge_rt:indexed_map_get(Evidence,I,M),
 {_,Entries,Bytes}=get('$ridge_collection_migrations'),
 true=length(Entries)=<4, true=Bytes=<1048576
end,lists:zip(lists:seq(1,9),Maps)),
lists:foreach(fun(Offset) ->
 true=code:soft_purge(Module), Load(Offset), put(hashes,0),
 {some,1}=ridge_rt:indexed_map_get(Evidence,1,Original),
 {some,1}=ridge_rt:indexed_map_get(Evidence,1,Original),
 1026=get(hashes)
end,[1,2]),
true=code:soft_purge(Module), put(cache_reload_hook,fun() -> Load(3) end), put(hashes,0),
{some,1}=ridge_rt:indexed_map_get(Evidence,1,Original),
{some,1}=ridge_rt:indexed_map_get(Evidence,1,Original), 1026=get(hashes),
Legacy=setelement(3,Original,0), {some,1}=ridge_rt:indexed_map_get(Evidence,1,Legacy),
halt().
"#;
    assert_program_eval("pub fn exercise () -> Bool = true\n", expression, &[]);
}

#[test]
fn collection_retained_key_evidence_collapses_new_equivalences_and_preserves_update_cost() {
    let expression = r#"
Parse=fun(Text) -> {ok,Tokens,_}=erl_scan:string(Text), {ok,Form}=erl_parse:parse_form(Tokens), Form end,
Module=ridge_collection_key_probe,
Load=fun(EqBody) ->
 Forms=[Parse(Text) || Text <- ["-module(ridge_collection_key_probe).",
 "-export(['$ridge_evidence'/2]).",
 "'$ridge_evidence'(<<\"hash\">>,[]) -> {some,{#{hash => fun(_,S) -> put(hashes,get(hashes)+1),ridge_rt:hash_int(0,S) end},none}}; " ++
 "'$ridge_evidence'(<<\"eq\">>,[]) -> {some,{#{eq => fun(A,B) -> put(equals,get(equals)+1)," ++ EqBody ++ " end},none}}."]],
 {ok,Module,Binary}=compile:forms(Forms,[return_errors]),
 {module,Module}=code:load_binary(Module,"key-contract-probe",Binary),
 ok=ridge_rt:invalidate_record_versions([Module]) end,
put(hashes,0), put(equals,0), Load("A =:= B"),
H=ridge_rt:evidence_bind(Module,<<"hash">>,[]), E=ridge_rt:evidence_bind(Module,<<"eq">>,[]),
Evidence=ridge_rt:key_evidence(H,E),
Original=ridge_rt:indexed_map_from_list(Evidence,[{1,10},{2,20}]),
Set=ridge_rt:indexed_set_from_list(Evidence,[1,2]),
[{First,_},{_,Last}]=ridge_rt:indexed_map_to_list(Original),
[FirstSet,_]=ridge_rt:indexed_map_keys(Set),
2=ridge_rt:indexed_map_size(Original), Load("true"),
1=ridge_rt:indexed_map_size(Original), [FirstSet]=ridge_rt:indexed_map_keys(Set),
{ok,Current}=ridge_rt:migrate_value(Original),
[{First,Last}]=ridge_rt:indexed_map_to_list(Current),
put(hashes,0), put(equals,0), put(updates,0),
Updated=ridge_rt:indexed_map_update(Evidence,99,fun({some,Old}) ->
 Last=Old,put(updates,get(updates)+1),Old+1 end,Current),
1=get(hashes), 1=get(equals), 1=get(updates),
Next=Last+1, [{First,Next}]=ridge_rt:indexed_map_to_list(Updated),
[{First,Last}]=ridge_rt:indexed_map_to_list(Current),
put(hashes,0), Mapped=ridge_rt:indexed_map_map(fun(_,V) -> V+1 end,Current),
0=get(hashes), [{First,Next}]=ridge_rt:indexed_map_to_list(Mapped),
true=code:soft_purge(Module), Load("erlang:error(eq_failed)"),
try ridge_rt:migrate_value(Original) of _ -> error(expected_eq_failure)
catch error:eq_failed -> ok end,
try ridge_rt:key_evidence(#{hash => fun ridge_rt:hash_int/2},#{eq => fun(A,B) -> A =:= B end}) of
 _ -> error(expected_evidence_failure)
catch error:invalid_evidence -> ok end,
halt().
"#;
    assert_program_eval("pub fn exercise () -> Bool = true\n", expression, &[]);
}

#[test]
fn collection_retained_key_evidence_is_private_to_its_stdlib_file() {
    for module in ["map", "set", "list"] {
        let source = format!(
            "import std.{module} as Collection\npub fn invalid () = Collection._keyEvidence ()\n"
        );
        let dir = workspace(&source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            artifacts
                .diagnostics
                .iter()
                .any(|d| d.code.starts_with('R')),
            "{:?}",
            artifacts.diagnostics
        );
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn collection_retained_key_evidence_reindexes_after_a_method_only_reload() {
    let source = r"
import std.map as Map
import std.set as Set
type Key = { id: Int }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
pub fn key () -> Key = Key { id = 7 }
pub fn make () -> Map Key Int = Map.fromList [(key (), 42)]
pub fn makeSet () -> Set Key = Set.fromList [key ()]
pub fn lookup (value: Key) (items: Map Key Int) -> Option Int = Map.get value items
pub fn contains (value: Key) (items: Set Key) -> Bool = Set.contains value items
pub fn count (items: Map a b) -> Int = Map.length items
pub fn enumerate (items: Set a) -> List a = Set.toList items
";
    let replacement = source
        .replace("= Map.fromList [(key (), 42)]", "= Map.empty")
        .replace("= Set.fromList [key ()]", "= Set.empty");
    assert_evidence_source_reload(
        source,
        &replacement,
        r"
Module=ridge_app_Main, Key=Module:key(), Original=Module:make(), Set=Module:makeSet(),
1=Module:count(Original), {some,42}=Module:lookup(Key,Original), true=Module:contains(Key,Set),
{ok,Binary}=file:read_file(Next), {module,Module}=code:load_binary(Module,Next,Binary),
ok=ridge_rt:invalidate_record_versions([Module]),
{some,42}=Module:lookup(Key,Original), true=Module:contains(Key,Set),
true=code:soft_purge(Module), {ok,ThirdBinary}=file:read_file(ThirdPath),
{module,Module}=code:load_binary(Module,ThirdPath,ThirdBinary),
ok=ridge_rt:invalidate_record_versions([Module]),
1=Module:count(Original), [Key]=Module:enumerate(Set),
{some,42}=Module:lookup(Key,Original), true=Module:contains(Key,Set),
{ok,Current}=ridge_rt:migrate_value(Original),
{ok,Current}=ridge_rt:migrate_value(Current),
{ridge_indexed_map,_,_,{ridge_map,Seed,_,_}}=Original,
{ridge_indexed_map,_,_,{ridge_map,Seed,_,_}}=Current,
{some,42}=Module:lookup(Key,Current), halt().
",
    );
}

#[test]
fn collection_key_witness_defers_refresh_until_its_evidence_is_consumed() {
    let expression = r#"
Parse=fun(S) -> {ok,T,_}=erl_scan:string(S), {ok,F}=erl_parse:parse_form(T), F end,
P=ridge_lazy_key_evidence_probe,
Load=fun(Offset) ->
 Body="'$ridge_evidence'(<<\"hash\">>,[]) -> put(hash_builds,get(hash_builds)+1)," ++
  "{some,{#{hash => fun(K,S) -> ridge_rt:hash_int(K+" ++ integer_to_list(Offset) ++ ",S) end},none}}; " ++
  "'$ridge_evidence'(<<\"eq\">>,[]) -> put(eq_builds,get(eq_builds)+1)," ++
  "{some,{#{eq => fun(A,B) -> A =:= B end},none}}.",
 Forms=[Parse(S) || S <- ["-module(ridge_lazy_key_evidence_probe).","-export(['$ridge_evidence'/2]).",Body]],
 {ok,P,Binary}=compile:forms(Forms,[return_errors]),
 {module,P}=code:load_binary(P,"lazy-evidence-probe",Binary),
 ok=ridge_rt:invalidate_record_versions([P]) end,
Reset=fun() -> put(hash_builds,0),put(eq_builds,0) end,
NoBuild=fun() -> 0=get(hash_builds),0=get(eq_builds) end,
Reset(), Load(0),
H=ridge_rt:evidence_bind(P,<<"hash">>,[]), E=ridge_rt:evidence_bind(P,<<"eq">>,[]),
Original=ridge_rt:indexed_map_from_list(ridge_rt:key_evidence(H,E),[{7,42}]),
Load(1), {ok,Current}=ridge_rt:migrate_value(Original),
{ridge_indexed_map,{ridge_key_evidence,_,FreshH,_},_,_}=Current,
Reset(), Unused=ridge_rt:key_evidence(H,E), NoBuild(),
Mixed=ridge_rt:key_evidence(FreshH,E), NoBuild(),
{some,42}=ridge_rt:indexed_map_get(Unused,7,Current),
{some,42}=ridge_rt:indexed_map_get(Mixed,7,Current), NoBuild(),
NewHash=maps:get(hash,FreshH), Eq=fun(A,B) -> A =:= B end,
Fresh=ridge_rt:indexed_map_from_list(Unused,[{7,99}]),
1=get(hash_builds),1=get(eq_builds), {some,99}=ridge_rt:typed_map_get(NewHash,Eq,7,Fresh),
Reset(), FromMixed=ridge_rt:indexed_map_from_list(Mixed,[{7,77}]),
0=get(hash_builds),1=get(eq_builds), {some,77}=ridge_rt:typed_map_get(NewHash,Eq,7,FromMixed),
Reset(), Inserted=ridge_rt:indexed_map_insert(Unused,7,55,ridge_rt:indexed_map_empty()),
1=get(hash_builds),1=get(eq_builds), {some,55}=ridge_rt:typed_map_get(NewHash,Eq,7,Inserted),
Reset(), Updated=ridge_rt:indexed_map_update(Unused,7,fun(none) -> 66 end,ridge_rt:indexed_map_empty()),
1=get(hash_builds),1=get(eq_builds), {some,66}=ridge_rt:typed_map_get(NewHash,Eq,7,Updated),
Reset(), {some,42}=ridge_rt:indexed_map_get(Unused,7,Original),
1=get(hash_builds),1=get(eq_builds), halt().
"#;
    assert_program_eval("pub fn exercise () -> Bool = true\n", expression, &[]);
}

#[test]
fn collection_vacated_values_survive_removing_their_key_instances() {
    let source = r"
import std.map as Map
import std.set as Set
type Key = { id: Int }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
pub fn key () -> Key = Key { id = 7 }
pub fn make () -> Map Key Int = Map.fromList [(key (), 42)]
pub fn makeSet () -> Set Key = Set.fromList [key ()]
pub fn literal () -> Map Key Int = Map.empty
pub fn emptyList () -> Map Key Int = Map.fromList []
pub fn emptySetList () -> Set Key = Set.fromList []
pub fn removed (items: Map Key Int) -> Map Key Int = Map.remove (key ()) items
pub fn inserted (items: Map Key Int) -> Map Key Int = Map.insert (key ()) 99 items
pub fn updated (items: Map Key Int) -> Map Key Int = Map.update (key ()) (fn _old -> 99) items
pub fn filtered (items: Map a b) -> Map a b = Map.filter (fn _key _value -> false) items
pub fn filteredMap (items: Map a b) -> Map a b = Map.filterMap (fn _key _value -> None) items
pub fn mapped (items: Map a Int) -> Map a Int = Map.map (fn _ v -> v + 1) items
pub fn removedSet (items: Set Key) -> Set Key = Set.remove (key ()) items
pub fn insertedSet (items: Set Key) -> Set Key = Set.insert (key ()) items
pub fn subtracted (items: Set Key) -> Set Key = Set.difference items items
pub fn intersected (items: Set Key) -> Set Key = Set.intersection items Set.empty
pub fn count (items: Map a b) -> Int = Map.length items
pub fn entries (items: Map a b) -> List (a, b) = Map.toList items
pub fn setCount (items: Set a) -> Int = Set.length items
pub fn elements (items: Set a) -> List a = Set.toList items
pub fn revision () -> Int = 1
";
    let replacement = r"
import std.map as Map
import std.set as Set
type Key = { id: Int }
pub fn key () -> Key = Key { id = 7 }
pub fn count (items: Map a b) -> Int = Map.length items
pub fn entries (items: Map a b) -> List (a, b) = Map.toList items
pub fn setCount (items: Set a) -> Int = Set.length items
pub fn elements (items: Set a) -> List a = Set.toList items
pub fn revision () -> Int = 2
";
    let third = replacement.replace("= 2", "= 3");
    assert_compiled_source_reloads(
        [source, replacement, &third],
        r"
Module=ridge_app_Main, Original=Module:make(), Set=Module:makeSet(),
Removed=Module:removed(Original),
Maps=[{literal,Module:literal()},{from_list,Module:emptyList()},
 {removed,Removed},{filtered,Module:filtered(Original)},
 {filter_map,Module:filteredMap(Original)},{mapped,Module:mapped(Removed)}],
Sets=[{from_list,Module:emptySetList()},{removed,Module:removedSet(Set)},
 {difference,Module:subtracted(Set)},{intersection,Module:intersected(Set)}],
lists:foreach(fun({_,Empty}) -> {ridge_indexed_map,none,0,_}=Empty end,Maps++Sets),
{ridge_indexed_map,_,_,{ridge_map,Seed,_,_}}=Original,
{ridge_indexed_map,none,0,{ridge_map,Seed,0,_}}=Removed,
lists:foreach(fun(Restored) ->
 {ridge_indexed_map,_,_,{ridge_map,Seed,1,_}}=Restored,
 [{_,99}]=Module:entries(Restored)
end,[Module:inserted(Removed),Module:updated(Removed)]),
RemovedSet=Module:removedSet(Set),
{ridge_indexed_map,_,_,{ridge_map,SetSeed,_,_}}=Set,
{ridge_indexed_map,_,_,{ridge_map,SetSeed,1,_}}=Module:insertedSet(RemovedSet),
1=Module:count(Original), 1=Module:setCount(Set),
Check=fun() ->
 lists:foreach(fun({Label,M}) ->
   case catch Module:count(M) of 0 -> ok; Failure -> error({empty_map_failed,Label,Failure}) end,
   []=Module:entries(M), {ok,Current}=ridge_rt:migrate_value(M),
   0=Module:count(Current), []=Module:entries(Current)
 end,Maps),
 lists:foreach(fun({Label,S}) ->
   case catch Module:setCount(S) of 0 -> ok; Failure -> error({empty_set_failed,Label,Failure}) end,
   []=Module:elements(S), {ok,Current}=ridge_rt:migrate_value(S),
   0=Module:setCount(Current), []=Module:elements(Current)
 end,Sets)
end,
Check(),
{ok,Binary}=file:read_file(Next), {module,Module}=code:load_binary(Module,Next,Binary),
ok=ridge_rt:invalidate_record_versions([Module]), 2=Module:revision(), Check(),
try Module:count(Original) of _ -> error(expected_nonempty_evidence_failure)
catch error:{missing_evidence_provider,Module,_} -> ok end,
true=code:soft_purge(Module), {ok,ThirdBinary}=file:read_file(ThirdPath),
{module,Module}=code:load_binary(Module,ThirdPath,ThirdBinary),
ok=ridge_rt:invalidate_record_versions([Module]), 3=Module:revision(), Check(), halt().
",
    );
}

fn assert_evidence_source_reload(source: &str, replacement: &str, checks: &str) {
    let third = replacement.replace("= hash value.id hasher", "= hash (value.id + 1) hasher");
    assert_ne!(
        third, replacement,
        "the third version must change the Hash implementation"
    );
    assert_compiled_source_reloads([source, replacement, &third], checks);
}

fn assert_compiled_source_reloads(sources: [&str; 3], checks: &str) {
    let _ = compiled_source_reloads_output(sources, checks);
}

fn compiled_source_reloads_output(sources: [&str; 3], checks: &str) -> std::process::Output {
    let directories = sources.map(workspace);
    let caches = [
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    ];
    let modules: Vec<_> = directories
        .iter()
        .zip(&caches)
        .map(|(dir, cache)| {
            let artifacts = compile_workspace(
                CompileOptions::new(dir.path().to_path_buf())
                    .with_emit(EmitArtefacts::Beam)
                    .with_cache_root(cache.path().to_path_buf()),
            )
            .unwrap();
            assert!(
                artifacts.diagnostics.is_empty(),
                "{:?}",
                artifacts.diagnostics
            );
            artifacts
                .beam_files
                .into_iter()
                .find(|path| {
                    path.file_stem().and_then(|name| name.to_str()) == Some("ridge_app_Main")
                })
                .unwrap()
        })
        .collect();
    let path = modules[1].to_string_lossy().replace('\\', "/");
    let third_path = modules[2].to_string_lossy().replace('\\', "/");
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(modules[0].parent().unwrap())
        .args([
            "-eval",
            &format!("Next={path:?},ThirdPath={third_path:?},{checks}"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

const fn collection_actor_source() -> &'static str {
    r"
import std.map as Map
import std.list as List
type Key = { id: Int, writer: fn Int Hasher -> Hasher }
type Archive = { indices: List (Map Key Int) }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = value.writer value.id hasher
pub fn key (id: Int) (writer: fn Int Hasher -> Hasher) -> Key = Key { id = id, writer = writer }
pub fn make (writer: fn Int Hasher -> Hasher) -> Map Key Int =
    Map.fromList (List.map (fn id -> (key id writer, id)) (List.range 0 63))
actor Store =
    state cache: Map Key Int = Map.empty
    state archive: Archive = Archive { indices = [] }
    on install (value: Map Key Int) =
        cache <- value
    on installNested (value: Map Key Int) =
        archive <- Archive { indices = [value] }
    on lookup (value: Key) -> Option Int = Map.get value cache
    on lookupNested (value: Key) -> List (Option Int) =
        List.map (fn index -> Map.get value index) archive.indices
    on ping () -> Int = 1
"
}

#[test]
fn collection_actor_reads_preserve_values_across_provider_only_reloads() {
    let source = collection_actor_source();
    let second = source.replace(
        "value.writer value.id hasher",
        "value.writer (value.id + 1) hasher",
    );
    let third = source.replace(
        "value.writer value.id hasher",
        "value.writer (value.id + 2) hasher",
    );
    let output = compiled_source_reloads_output(
        [source, &second, &third],
        r#"
Main=ridge_app_Main,
Writer=fun(K,S) -> C=case get(actor_hash_calls) of undefined -> 0; V -> V end,
 put(actor_hash_calls,C+1),ridge_rt:hash_int(K,S) end,
Map=Main:make(Writer), Key=Main:key(7,Writer),
H={ridge_handle,Pid,_}=ridge_rt:spawn_actor(ridge_app_Main_store,[],[]),
ok=ridge_rt:send_op(H,{install,Map}), ok=ridge_rt:send_op(H,{'installNested',Map}),
1=ridge_rt:ask(H,{ping},5000),
Read=fun() -> {dictionary,D}=process_info(Pid,dictionary),proplists:get_value(actor_hash_calls,D,0) end,
0=Read(), {some,7}=ridge_rt:ask(H,{lookup,Key},5000), 1=Read(),
[{some,7}]=ridge_rt:ask(H,{'lookupNested',Key},5000), 2=Read(),
StateBefore=sys:get_state(Pid),
{ok,Binary}=file:read_file(Next), {module,Main}=code:load_binary(Main,Next,Binary),
ok=ridge_rt:invalidate_record_versions([Main]),
Before=Read(), {some,7}=ridge_rt:ask(H,{lookup,Key},5000), First=Read(),
{some,7}=ridge_rt:ask(H,{lookup,Key},5000), Second=Read(),
io:format("ACTOR_COST size=64 first_hashes=~p second_hashes=~p state_changed=~p~n",
 [First-Before,Second-First,StateBefore =/= sys:get_state(Pid)]),
[{some,7}]=ridge_rt:ask(H,{'lookupNested',Key},5000), NestedFirst=Read(),
[{some,7}]=ridge_rt:ask(H,{'lookupNested',Key},5000), NestedSecond=Read(),
io:format("ACTOR_COST nested_first_hashes=~p nested_second_hashes=~p~n",
 [NestedFirst-Second,NestedSecond-NestedFirst]),
true=code:soft_purge(Main), {ok,ThirdBinary}=file:read_file(ThirdPath),
{module,Main}=code:load_binary(Main,ThirdPath,ThirdBinary),
ok=ridge_rt:invalidate_record_versions([Main]),
BeforeThird=Read(), {some,7}=ridge_rt:ask(H,{lookup,Key},5000), AfterThird=Read(),
{some,7}=ridge_rt:ask(H,{lookup,Key},5000), AfterThirdAgain=Read(),
io:format("ACTOR_COST third_first_hashes=~p third_second_hashes=~p~n",
 [AfterThird-BeforeThird,AfterThirdAgain-AfterThird]),
[{some,7}]=ridge_rt:ask(H,{'lookupNested',Key},5000), NestedThird=Read(),
[{some,7}]=ridge_rt:ask(H,{'lookupNested',Key},5000), NestedThirdAgain=Read(),
io:format("ACTOR_COST nested_third_first_hashes=~p nested_third_second_hashes=~p~n",
 [NestedThird-AfterThirdAgain,NestedThirdAgain-NestedThird]),
ok=gen_server:stop(Pid), halt().
"#,
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn collection_actor_loader_refreshes_direct_and_nested_indexes_before_resuming() {
    let source = collection_actor_source();
    let second = source.replace(
        "value.writer value.id hasher",
        "value.writer (value.id + 1) hasher",
    );
    let third = source.replace(
        "value.writer value.id hasher",
        "value.writer (value.id + 2) hasher",
    );
    let output = compiled_source_reloads_output(
        [source, &second, &third],
        r#"
Main=ridge_app_Main,
Writer=fun(K,S) -> C=case get(actor_hash_calls) of undefined -> 0; V -> V end,
 put(actor_hash_calls,C+1),ridge_rt:hash_int(K,S) end,
Map=Main:make(Writer),Key=Main:key(7,Writer),64=ridge_rt:indexed_map_size(Map),
{ridge_indexed_map,_,_,{ridge_map,Seed,64,_}}=Map,
H={ridge_handle,Pid,_}=ridge_rt:spawn_actor(ridge_app_Main_store,[],[]),
ok=ridge_rt:send_op(H,{install,Map}),ok=ridge_rt:send_op(H,{'installNested',Map}),
1=ridge_rt:ask(H,{ping},5000),
Read=fun() -> {dictionary,D}=process_info(Pid,dictionary),proplists:get_value(actor_hash_calls,D,0) end,
0=Read(),{some,7}=ridge_rt:ask(H,{lookup,Key},5000),1=Read(),
[{some,7}]=ridge_rt:ask(H,{'lookupNested',Key},5000),2=Read(),
Parse=fun(Text) -> {ok,Tokens,_}=erl_scan:string(Text),{ok,Form}=erl_parse:parse_form(Tokens),Form end,
Unrelated=ridge_collection_reload_unrelated,
{ok,Unrelated,UnrelatedBinary}=compile:forms([Parse(Text) || Text <- [
 "-module(ridge_collection_reload_unrelated).","-export([value/0]).","value() -> 1."]],[return_errors]),
{ok,SecondBinary}=file:read_file(Next),{ok,ThirdBinary}=file:read_file(ThirdPath),
persistent_term:put(ridge_loader_vsn,<<"initial">>),
lists:foreach(fun({Module,Binary,Version}) ->
 case Version of <<"third">> -> true=code:soft_purge(Main); _ -> ok end,
 Base=ridge_loader:current_version(),
 Manifest=iolist_to_binary(json:encode(#{base_vsn => Base,new_vsn => Version,
  modules => [atom_to_binary(Module)],actors => []})),
 Before=Read(),
 {Micros,{ok,Report}}=timer:tc(fun() -> ridge_loader:apply_bundle(Manifest,
  [{atom_to_binary(Module),Binary}],#{base_vsn => Base,purge_after_ms => 0}) end),
 Refreshed=Read(),1=maps:get(actors_suspended,Report),1=maps:get(actors_refreshed,Report),
 0=maps:get(actors_migrated,Report),0=maps:get(actors_restarted,Report),
 Generation=ridge_rt:evidence_generation(),
 #{cache := {ridge_indexed_map,_,{ridge_map_snapshot,Generation,_},{ridge_map,Seed,64,_}},
   archive := #{indices := [{ridge_indexed_map,_,{ridge_map_snapshot,Generation,_},{ridge_map,Seed,64,_}}]}}=sys:get_state(Pid),
 {some,7}=ridge_rt:ask(H,{lookup,Key},5000),First=Read(),
 {some,7}=ridge_rt:ask(H,{lookup,Key},5000),Second=Read(),
 [{some,7}]=ridge_rt:ask(H,{'lookupNested',Key},5000),Nested=Read(),
 [{some,7}]=ridge_rt:ask(H,{'lookupNested',Key},5000),NestedAgain=Read(),
 io:format("ACTOR_LOADER_COST version=~s reload_us=~p refresh_hashes=~p direct=~p/~p nested=~p/~p~n",
  [Version,Micros,Refreshed-Before,First-Refreshed,Second-First,Nested-Second,NestedAgain-Nested]),
 1=First-Refreshed,1=Second-First,1=Nested-Second,1=NestedAgain-Nested
end,[{Unrelated,UnrelatedBinary,<<"unrelated">>},{Main,SecondBinary,<<"second">>},
 {Main,ThirdBinary,<<"third">>}]),
ok=gen_server:stop(Pid),halt().
"#,
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn collection_messages_refresh_evidence_from_another_vm_incarnation() {
    let source = r"
import std.map as Map
type Key = { id: Int }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
pub fn make () -> Map Key Int = Map.fromList [(Key { id = 7 }, 7)]
pub fn read (values: Map Key Int) -> Option Int = Map.get (Key { id = 7 }) values
pub fn same (left: Map Key Int) (right: Map Key Int) -> Bool = eq left right
";
    let second = source.replace("hash value.id hasher", "hash (value.id + 1) hasher");
    let third = source.replace("hash value.id hasher", "hash (value.id + 2) hasher");
    let output = compiled_source_reloads_output(
        [source, &second, &third],
        r#"
Map=ridge_app_Main:make(),{some,7}=ridge_app_Main:read(Map),
Wire=filename:join(filename:dirname(Next),"foreign-value.etf"),
ok=file:write_file(Wire,term_to_binary(Map)),
Eval=lists:flatten(io_lib:format(
 "Fresh=ridge_app_Main:make(),{some,7}=ridge_app_Main:read(Fresh),"
 "{ok,Bytes}=file:read_file(~p),Foreign=binary_to_term(Bytes),"
 "true=ridge_app_Main:same(Foreign,Fresh),"
 "{ok,{lookup,Current}}=ridge_rt:migrate_message({lookup,Foreign}),"
 "true=ridge_app_Main:same(Current,Fresh),"
 "Actual=ridge_app_Main:read(Current),io:format(\"SOURCE_WIRE_LOOKUP ~~p~~n\",[Actual]),"
 "{some,7}=Actual,halt().",[Wire])),
Port=open_port({spawn_executable,os:find_executable("erl")},
 [binary,exit_status,stderr_to_stdout,use_stdio,
  {args,["-noshell","-pa",filename:dirname(Next),"-eval",Eval]}]),
Collect=fun F(Acc) -> receive
 {Port,{data,Chunk}} -> F(<<Acc/binary,Chunk/binary>>);
 {Port,{exit_status,Code}} -> {Code,Acc}
 after 15000 -> erlang:error(child_vm_timeout) end end,
{Code,Out}=Collect(<<>>),io:format("~s",[Out]),0=Code,halt().
"#,
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn collection_evidence_refreshes_context_after_reloads_and_code_purge() {
    let expression = r#"
Parse=fun(Text) -> {ok,Tokens,_}=erl_scan:string(Text),
 {ok,Form}=erl_parse:parse_form(Tokens), Form end,
Load=fun(Module,Body) ->
 Forms=[Parse(Text) || Text <- ["-module(" ++ atom_to_list(Module) ++ ").",
  "-export(['$ridge_evidence'/2]).",Body]],
 {ok,Module,Binary}=compile:forms(Forms,[return_errors]),
 {module,Module}=code:load_binary(Module,"comparison-evidence-probe",Binary),
 ok=ridge_rt:invalidate_record_versions([Module]) end,
Child=ridge_evidence_child_probe, Parent=ridge_evidence_parent_probe,
ChildBody=fun(Value) -> "'$ridge_evidence'(<<\"child\">>,[]) -> " ++
 "put(child_builds,get(child_builds)+1),{some,{#{value => fun() -> " ++
 integer_to_list(Value) ++ " end},none}}." end,
put(child_builds,0), put(parent_builds,0),
Load(Child,ChildBody(1)),
Load(Parent,"'$ridge_evidence'(<<\"parent\">>,[Child]) -> " ++
 "put(parent_builds,get(parent_builds)+1),{some,{#{value => maps:get(value,Child)},none}}."),
Leaf=ridge_rt:evidence_bind(Child,<<"child">>,[]),
Original=ridge_rt:evidence_bind(Parent,<<"parent">>,[Leaf]),
1=(maps:get(value,Original))(),
1=get(child_builds), 1=get(parent_builds),
Original=ridge_rt:evidence_current(Original),
1=get(child_builds), 1=get(parent_builds),
Generation=ridge_rt:evidence_generation(),
ok=ridge_rt:invalidate_record_versions([]),
Generation=ridge_rt:evidence_generation(),
Load(Child,ChildBody(2)),
Second=ridge_rt:evidence_current(Original),
2=(maps:get(value,Second))(),
2=get(child_builds), 2=get(parent_builds),
Second=ridge_rt:evidence_current(Second),
2=get(child_builds), 2=get(parent_builds),
true=code:soft_purge(Child),
Load(Child,ChildBody(3)),
Third=ridge_rt:evidence_current(Original),
3=(maps:get(value,Third))(),
3=get(child_builds), 3=get(parent_builds),
try ridge_rt:evidence_bind(Parent,<<"changed-context">>,[Leaf]) of
 _ -> error(expected_missing_recipe)
catch error:function_clause -> ok end,
try ridge_rt:evidence_current(#{value => fun() -> old end}) of
 _ -> error(expected_invalid_evidence)
catch error:invalid_evidence -> ok end,
halt().
"#;
    assert_program_eval("pub fn exercise () -> Bool = true\n", expression, &[]);
}

#[test]
fn collection_primitive_keeps_constraint_parameters() {
    let source = r"
@primitive
fn keyMethods () -> (fn a Hasher -> Hasher, fn a a -> Bool) where Hash a, Eq a
pub fn use () -> (fn a Hasher -> Hasher, fn a a -> Bool) where Hash a = keyMethods ()
";
    let dir = workspace(source);
    let mut options =
        ridge_driver::CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true);
    options.is_stdlib = true;
    let checked = ridge_driver::check_workspace_incremental(options).unwrap();
    assert!(checked.resolved.lex_errors.is_empty());
    assert!(
        checked.resolved.parse_errors.is_empty(),
        "{:?}",
        checked.resolved.parse_errors
    );
    assert!(
        checked.resolved.errors.is_empty(),
        "{:?}",
        checked.resolved.errors
    );
    assert!(checked.type_errors.is_empty(), "{:?}", checked.type_errors);
    let lowered = ridge_lower::lower_workspace(&checked.typed, &checked.resolved);
    assert!(lowered.errors.is_empty(), "{:?}", lowered.errors);
    let primitive = lowered
        .workspace
        .modules
        .iter()
        .flatten()
        .flat_map(|m| &m.items)
        .find_map(|item| {
            if let ridge_ir::IrItem::Primitive(primitive) = item {
                (primitive.name == "keyMethods").then_some(primitive)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        primitive.params.len(),
        2,
        "Hash and Eq evidence must reach the primitive"
    );
    assert!(primitive
        .params
        .iter()
        .any(|name| name.starts_with("$dict_Hash_")));
    assert!(primitive
        .params
        .iter()
        .any(|name| name.starts_with("$dict_Eq_")));
    let caller = lowered
        .workspace
        .modules
        .iter()
        .flatten()
        .flat_map(|m| &m.items)
        .find_map(|item| {
            if let ridge_ir::IrItem::Fn(function) = item {
                (function.name == "use").then_some(function)
            } else {
                None
            }
        })
        .unwrap();
    let ridge_ir::IrExpr::Call { args, .. } = &caller.body else {
        panic!("expected a direct primitive call: {:?}", caller.body);
    };
    assert_eq!(
        args.len(),
        2,
        "the nullary call must carry only its evidence"
    );
    assert!(args.iter().all(|arg| !matches!(
        arg,
        ridge_ir::IrExpr::Lit {
            value: ridge_ir::IrLit::Unit,
            ..
        }
    )));
    let cache = tempfile::tempdir().unwrap();
    let untrusted = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        untrusted.diagnostics.iter().any(|d| d.code == "R022"),
        "{:?}",
        untrusted.diagnostics
    );
    assert!(
        untrusted.core_files.is_empty(),
        "ordinary projects cannot define this primitive"
    );
}

#[test]
fn collection_source_evidence_layout_matches_imports_and_targets() {
    for (module, source) in [
        (
            "std.map",
            include_str!("../../ridge-stdlib/stdlib/map.ridge"),
        ),
        (
            "std.set",
            include_str!("../../ridge-stdlib/stdlib/set.ridge"),
        ),
        (
            "std.list",
            include_str!("../../ridge-stdlib/stdlib/list.ridge"),
        ),
    ] {
        let dir = workspace(source);
        let mut options =
            ridge_driver::CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true);
        options.is_stdlib = true;
        let checked = ridge_driver::check_workspace_incremental(options).unwrap();
        assert!(checked.type_errors.is_empty(), "{:?}", checked.type_errors);
        for schemes in &checked.typed.module_schemes {
            for (name, scheme) in schemes {
                if source.contains(&format!("pub fn {name} "))
                    && scheme.constraints.iter().any(|c| {
                        checked
                            .typed
                            .class_table
                            .get(c.class)
                            .is_some_and(|class| class.name == "Hash")
                    })
                {
                    let (imported, params, _) = ridge_typecheck::reconciled_fn_dict_sig(
                        module,
                        name,
                        &checked.typed.stdlib_tycons,
                        &checked.typed.builtins,
                        &checked.typed.class_table,
                    )
                    .unwrap();
                    assert_eq!(
                        scheme
                            .constraints
                            .iter()
                            .map(|c| c.class)
                            .collect::<Vec<_>>(),
                        imported.iter().map(|c| c.class).collect::<Vec<_>>(),
                        "{module}.{name}: dictionary order"
                    );
                    assert!(
                        scheme
                            .constraints
                            .iter()
                            .all(|c| c.tys == scheme.constraints[0].tys),
                        "{module}.{name}: same key evidence"
                    );
                    let target = ridge_stdlib::stdlib_targets::lookup(module, name).unwrap();
                    assert_eq!(
                        usize::try_from(target.arity()).unwrap(),
                        params.len() + imported.len(),
                        "{module}.{name}: target arity"
                    );
                }
            }
        }
    }
}

#[test]
fn collection_stdlib_consumers_typecheck() {
    let dir = tempfile::tempdir().unwrap();
    ridge_driver::write_stdlib_test_workspace(dir.path()).unwrap();
    let mut options =
        ridge_driver::CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true);
    options.is_stdlib = true;
    let checked = ridge_driver::check_workspace_incremental(options).unwrap();
    assert!(checked.resolved.lex_errors.is_empty());
    assert!(
        checked.resolved.parse_errors.is_empty(),
        "{:?}",
        checked.resolved.parse_errors
    );
    assert!(
        checked.resolved.errors.is_empty(),
        "{:?}",
        checked.resolved.errors
    );
    let errors: Vec<_> = checked
        .type_errors
        .iter()
        .map(|(module, error)| {
            format!(
                "{}: {}",
                checked.resolved.graph.modules[module.0 as usize].fully_qualified_name,
                error.render(&checked.typed.tycons)
            )
        })
        .collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn public_collections_use_custom_keys_and_unconstrained_values() {
    assert_true_program(
        r#"
import std.map as Map
import std.set as Set
import std.list as List
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
fn lookup (key: a) (items: Map a b) -> Option b where Hash a = Map.get key items
pub fn exercise () -> Bool =
    let a = Key { id = 1, label = "first" }
    let alias = Key { id = 1, label = "later" }
    let b = Key { id = 2, label = "second" }
    let original = Map.fromList [(a, 10), (b, 20), (alias, 30)]
    let removed = Map.remove alias original
    let merged = Map.merge original (Map.fromList [(alias, 90)])
    let changed = Map.update alias (fn _ -> 50) original
    let mapped = Map.map (fn _ v -> fn () -> v) original
    let filtered = Map.filterMap (fn _ v -> if eq v 30 then Some (v + 1) else None) original
    let s = Set.fromList [a, b, alias]
    let other = Set.fromList [alias]
    let grouped = List.groupBy (fn k -> k) [a, b, alias]
    let mappedValue = match Map.get alias mapped
        Some f -> eq (f ()) 30
        None -> false
    let groupedValues = match Map.get alias grouped
        Some keys -> eq (List.map (fn k -> k.label) keys) ["first", "later"]
        None -> false
    eq (Map.length original) 2 && eq (lookup alias original) (Some 30) &&
    eq (Map.length removed) 1 && eq (Map.get a removed) None &&
    eq (Map.get a original) (Some 30) && eq (Map.get a merged) (Some 90) &&
    eq (Map.get a changed) (Some 50) &&
    mappedValue &&
    eq (Map.length filtered) 1 && eq (Map.get a filtered) (Some 31) &&
    eq (Set.length s) 2 && Set.contains alias s &&
    eq (Set.length (Set.union s other)) 2 &&
    eq (List.map (fn k -> k.label) (Set.toList (Set.intersection s other))) ["first"] &&
    eq (List.map (fn k -> k.label) (Set.toList (Set.difference s other))) ["second"] &&
    groupedValues &&
    eq (List.sort (List.map (fn k -> k.label) (Map.keys original))) ["first", "second"] &&
    eq (List.sort (List.map (fn k -> k.label) (Map.keys grouped))) ["first", "second"]
"#,
    );
}

#[test]
fn collection_key_requirements_reject_invalid_uses_before_core() {
    for source in [
        "import std.map as Map\npub fn exercise () = Map.fromList [(1.0, 7)]\n",
        "import std.set as Set\npub fn exercise () = Set.fromList [fn x -> x]\n",
        "import std.list as List\npub fn exercise () = List.groupBy (fn x -> x) [1.0]\n",
        "import std.map as Map\ntype Key = { value: Int } deriving (Eq)\npub fn exercise () = Map.fromList [(Key { value = 7 }, 1)]\n",
        "import std.map as Map\nfn lookup (key: a) (items: Map a b) -> Option b = Map.get key items\n",
        "import std.map as Map\npub fn exercise () =\n    let build = Map.fromList\n    let good = build [(1, 1)]\n    let bad = build [(1.0, 1)]\n    Map.length good + Map.length bad\n",
        "import std.map as Map\npub fn exercise () =\n    let build = Map.fromList\n    let bad = build [(1.0, 1)]\n    let good = build [(1, 1)]\n    Map.length good + Map.length bad\n",
    ] {
        let dir = workspace(source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        ).unwrap();
        assert!(artifacts.diagnostics.iter().all(|d|
            !d.code.starts_with('P') && !d.code.starts_with('R') && !d.code.starts_with('L')
        ), "{source}: {:?}", artifacts.diagnostics);
        assert!(artifacts.diagnostics.iter().any(|d| matches!(d.code, "T002" | "T029" | "T055")),
            "{source}: {:?}", artifacts.diagnostics);
        assert!(artifacts.core_files.is_empty(), "must reject before Core: {source}");
    }
    assert_true_program(
        r"
import std.map as Map
import std.set as Set
pub fn exercise () -> Bool =
    let items: Map (fn Int -> Int) (fn Int -> Int) = Map.empty
    let keys: Set (fn Int -> Int) = Set.empty
    eq (Map.length (Map.map (fn _ value -> value) items)) 0 && eq (Set.length keys) 0
",
    );
}

#[test]
fn collection_equality_ignores_seed_order_and_representative_but_compares_values() {
    assert_true_program(
        r#"
import std.map as Map
import std.set as Set
import std.bool as Bool
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (_value: Key) (hasher: Hasher) -> Hasher = hasher
type Envelope = { items: Map Key Decimal, keys: Set Key } deriving (Eq)
fn sameMaps (left: Map k v) (right: Map k v) -> Bool where Hash k, Eq v = eq left right
fn sameSets (left: Set a) (right: Set a) -> Bool where Hash a = eq left right
pub fn exercise () -> Bool =
    let a = Key { id = 1, label = "first" }
    let alias = Key { id = 1, label = "later" }
    let b = Key { id = 2, label = "second" }
    let first = Map.fromList [(a, 1.0m), (b, 2m)]
    let second = Map.fromList [(b, 2.00m), (alias, 1m)]
    let s = Set.fromList [a, b]
    let t = Set.fromList [b, alias, b]
    sameMaps first second && sameSets s t &&
    eq (Envelope { items = first, keys = s }) (Envelope { items = second, keys = t }) &&
    Bool.not (eq first (Map.insert alias 3m second)) &&
    Bool.not (eq first (Map.remove alias second)) &&
    Bool.not (eq s (Set.fromList [alias])) &&
    eq (Map.remove a (Map.remove b first)) (Map.remove b (Map.remove alias second))
"#,
    );
}

#[test]
fn collection_backed_parsed_values_compare_their_actual_fields() {
    assert_true_program(
        r#"
import std.cli as Cli
import std.bool as Bool
pub fn exercise () -> Bool =
    let first = Cli.parse ["--left=1", "--right=2", "--", "file"]
    let second = Cli.parse ["--right=2", "--left=1", "--", "file"]
    let changed = Cli.parse ["--left=3", "--right=2", "--", "file"]
    eq first second && Bool.not (eq first changed)
"#,
    );
}

#[test]
fn collection_record_migration_preserves_value_lookup_and_key_index() {
    let source = r#"
import std.map as Map
import std.set as Set
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
pub fn key (id: Int) -> Key = Key { id = id, label = "key" }
pub fn same (left: Key) (right: Key) -> Bool = eq left right
pub fn write (value: Key) (hasher: Hasher) -> Hasher = hash value hasher
pub fn byValue (value: Key) -> Map Int Key = Map.fromList [(7, value)]
pub fn byKey (value: Key) -> Map Key Int = Map.fromList [(value, 7)]
pub fn asSet (value: Key) -> Set Key = Set.fromList [value]
"#;
    let expression = r#"
Probe=ridge_collection_migration_probe,
Parse=fun(Text) -> {ok,Tokens,_}=erl_scan:string(Text), {ok,Form}=erl_parse:parse_form(Tokens), Form end,
Forms=[Parse(Text) || Text <- [
 "-module(ridge_collection_migration_probe).",
 "-export(['__ridge_record_versions'/0,'__ridge_record_migrations'/0]).",
 "'__ridge_record_versions'() -> #{'Key' => 2}.",
 "'__ridge_record_migrations'() -> [{1,fun(K) -> K#{id := 2,'__ridge_v' := {ridge_collection_migration_probe,'Key',2}} end}]."
]],
{ok,Probe,Binary}=compile:forms(Forms,[return_errors]),
{module,Probe}=code:load_binary(Probe,"collection-migration-probe",Binary),
Old=(ridge_app_Main:key(1))#{'__ridge_v' => {Probe,'Key',1}},
New=Old#{id := 2,'__ridge_v' := {Probe,'Key',2}},
H=fun ridge_app_Main:write/2,E=fun ridge_app_Main:same/2,
Values=ridge_app_Main:byValue(Old),
{ok,NextValues}=ridge_rt:migrate_value(Values),
{some,New}=ridge_rt:typed_map_get(fun ridge_rt:hash_int/2,fun(A,B) -> A =:= B end,7,NextValues),
Keys=ridge_app_Main:byKey(Old),
{ok,NextKeys}=ridge_rt:migrate_value(Keys),
{some,7}=ridge_rt:typed_map_get(H,E,New,NextKeys),
[{Old,7}]=ridge_rt:typed_map_to_list(Keys),
[{New,7}]=ridge_rt:typed_map_to_list(NextKeys),
1=ridge_rt:typed_map_size(NextKeys),
{ok,NextSet}=ridge_rt:migrate_value(ridge_app_Main:asSet(Old)),
[New]=ridge_rt:typed_map_keys(NextSet),
true=ridge_rt:typed_map_contains(H,E,New,NextSet),
halt().
"#;
    assert_program_eval(source, expression, &[]);
}

#[test]
fn compound_hash_calls_component_evidence_and_preserves_boundaries() {
    let source = r#"
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
fn key (side: Bool) -> Key = Key { id = 7, label = if side then "first" else "later" }
fn emit (value: a) (hasher: Hasher) -> Hasher where Hash a =
    let write = hash
    write value hasher
fn pairGeneric (value: a) (hasher: Hasher) -> Hasher where Hash a = hash (value, 9) hasher
fn recordGeneric value hasher = hash ({ z = 9, a = value }) hasher
pub fn pair (side: Bool) (hasher: Hasher) -> Hasher = pairGeneric (key side) hasher
pub fn record (side: Bool) (hasher: Hasher) -> Hasher = recordGeneric (key side) hasher
pub fn elements (side: Bool) (hasher: Hasher) -> Hasher = emit [key side, Key { id = 8, label = "end" }] hasher
fn some (side: Bool) -> Option Key = Some (key side)
fn ok (side: Bool) -> Result Key Int = Ok (key side)
fn err () -> Result Key Int = Err 7
fn none () -> Option Key = None
fn empty () -> List Key = []
pub fn optional (side: Bool) (hasher: Hasher) -> Hasher = emit (some side) hasher
pub fn result (side: Bool) (hasher: Hasher) -> Hasher = emit (ok side) hasher
pub fn missing (hasher: Hasher) -> Hasher = emit (none ()) hasher
pub fn failure (hasher: Hasher) -> Hasher = emit (err ()) hasher
pub fn emptyList (hasher: Hasher) -> Hasher = emit (empty ()) hasher
pub fn nested (side: Bool) (hasher: Hasher) -> Hasher =
    if side then emit [[1], [2]] hasher else emit [[1, 2]] hasher
pub fn split (side: Bool) (hasher: Hasher) -> Hasher =
    if side then emit ([1], [2, 3]) hasher else emit ([1, 2], [3]) hasher
pub fn exercise () -> Bool =
    eq (key true, 9) (key false, 9) &&
    eq ({ z = 9, a = key true }) ({ a = key false, z = 9 }) &&
    eq [key true] [key false] && eq (some true) (some false) && eq (ok true) (ok false)
"#;
    let expression = r"
S = ridge_rt:hash_new(<<0:128>>), true = ridge_app_Main:exercise(),
lists:foreach(fun(F) ->
 A = erlang:apply(ridge_app_Main,F,[true,S]), A = erlang:apply(ridge_app_Main,F,[false,S])
end,[pair,record,elements,optional,result]),
lists:foreach(fun(F) ->
 {ridge_hasher,_,Frames} = erlang:apply(ridge_app_Main,F,[true,S]),
 [7,9] = [V || [_,Encoded] <- lists:reverse(Frames), {int,V} <- [binary_to_term(Encoded)]]
end,[pair,record]),
{ridge_hasher,_,ListFrames} = ridge_app_Main:elements(true,S),
[7,8] = [V || [_,Encoded] <- lists:reverse(ListFrames), {int,V} <- [binary_to_term(Encoded)]],
false = ridge_app_Main:nested(true,S) =:= ridge_app_Main:nested(false,S),
false = ridge_app_Main:split(true,S) =:= ridge_app_Main:split(false,S),
false = ridge_app_Main:optional(true,S) =:= ridge_app_Main:missing(S),
false = ridge_app_Main:result(true,S) =:= ridge_app_Main:failure(S),
false = S =:= ridge_app_Main:emptyList(S),
halt().
";
    assert_program_eval(source, expression, &[]);
}

#[test]
fn compound_hash_derives_aliases_recursive_shapes_and_owned_value_records() {
    let source = r#"
import app.Owner as Owner
import std.time (Duration)
import std.proc (Output)
type Value = Text
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hash value.id hasher
type Parcel a = { z: List Decimal, a: (a, Int), choice: Option a } deriving (Eq, Hash)
type Tree a = End a | Branch (Tree a) (List (Tree a)) deriving (Eq, Hash)
type Alias a = (a, Decimal) deriving (Eq, Hash)
type Phantom a = { value: Int } deriving (Eq, Hash)
opaque type Wrapped a = { value: a } deriving (Eq, Hash)
fn emit (value: a) (hasher: Hasher) -> Hasher where Hash a = hash value hasher
fn key (side: Bool) -> Key = Key { id = 7, label = if side then "first" else "last" }
fn parcel (side: Bool) -> Parcel Key = Parcel { z = if side then [1.5m] else [1.50m], a = (key side, 9), choice = Some (key side) }
fn tree (side: Bool) -> Tree Key = Branch (End (key side)) [End (key side)]
fn alias (side: Bool) -> Alias Key = (key side, if side then 1.5m else 1.50m)
fn phantom () -> Phantom (fn () -> Bool) = Phantom { value = 7 }
pub fn parcelHash (side: Bool) (hasher: Hasher) -> Hasher = emit (parcel side) hasher
pub fn treeHash (side: Bool) (hasher: Hasher) -> Hasher = emit (tree side) hasher
pub fn aliasHash (side: Bool) (hasher: Hasher) -> Hasher = emit (alias side) hasher
pub fn aliasDirect (side: Bool) (hasher: Hasher) -> Hasher = emit (key side, if side then 1.5m else 1.50m) hasher
pub fn wrappedHash (side: Bool) (hasher: Hasher) -> Hasher = emit (Wrapped { value = key side }) hasher
pub fn ownerHash (side: Bool) (hasher: Hasher) -> Hasher = emit (Owner.make (key side)) hasher
pub fn phantomHash (hasher: Hasher) -> Hasher = emit (phantom ()) hasher
pub fn durationHash (value: Duration) (hasher: Hasher) -> Hasher = emit value hasher
pub fn outputHash (value: Output) (hasher: Hasher) -> Hasher = emit value hasher
pub fn errorHash (value: Error) (hasher: Hasher) -> Hasher = emit value hasher
pub fn exercise () -> Bool = eq (parcel true) (parcel false) && eq (tree true) (tree false) && eq (alias true) (alias false)
"#;
    let owner = r"
type Value = Int
pub type Local a = Vacant | Occupied { value: a, rank: Value } deriving (Eq, Hash)
pub fn make (value: a) -> Local a = Occupied { value = value, rank = 1 }
";
    let expression = r#"
S = ridge_rt:hash_new(<<0:128>>), true = ridge_app_Main:exercise(),
lists:foreach(fun(F) ->
 A = erlang:apply(ridge_app_Main,F,[true,S]), A = erlang:apply(ridge_app_Main,F,[false,S])
end,[parcelHash,treeHash,aliasHash,wrappedHash,ownerHash]),
Alias = ridge_app_Main:aliasHash(true,S), Alias = ridge_app_Main:aliasDirect(true,S),
{ridge_hasher,_,OwnerFrames} = ridge_app_Main:ownerHash(true,S),
[1,7] = [V || [_,Encoded] <- lists:reverse(OwnerFrames), {int,V} <- [binary_to_term(Encoded)]],
false = S =:= ridge_app_Main:phantomHash(S),
D = ridge_app_Main:durationHash(#{ms=>7},S),
D = ridge_app_Main:durationHash(#{ms=>7,'__ridge_v'=>unrelated},S),
O = ridge_app_Main:outputHash(#{stdout=><<"out">>,stderr=><<"err">>,exitCode=>0},S),
O = ridge_app_Main:outputHash(#{stdout=><<"out">>,stderr=><<"err">>,exitCode=>0,'__ridge_v'=>other},S),
E = ridge_app_Main:errorHash(#{code=><<"code">>,message=><<"message">>},S),
E = ridge_app_Main:errorHash(#{code=><<"code">>,message=><<"message">>,'__ridge_v'=>other},S),
halt().
"#;
    assert_program_eval(source, expression, &[("Owner", owner)]);
}

#[test]
fn compound_hash_requires_the_opaque_owners_explicit_contract() {
    let source = "import app.Owner (Secret)\ntype Wrapper = { secret: Secret } deriving (Eq, Hash)\npub fn exercise (value: Secret) (hasher: Hasher) -> Hasher = hash value hasher\n";
    for (owner, invalid) in [
        ("pub opaque type Secret = { value: Int }\ninstance Eq Secret =\n    eq (left: Secret) (right: Secret) -> Bool = eq left.value right.value\n", true),
        ("pub opaque type Secret = { value: Int } deriving (Eq, Hash)\n", false),
    ] {
        let dir = workspace(source);
        std::fs::write(dir.path().join("app/src/Owner.ridge"), owner).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(CompileOptions::new(dir.path().to_path_buf()).with_emit(EmitArtefacts::Core).with_cache_root(cache.path().to_path_buf())).unwrap();
        assert!(!artifacts.diagnostics.iter().any(|d| d.code.starts_with('P') || d.code.starts_with('L') || d.code.starts_with('R')), "{:?}", artifacts.diagnostics);
        if invalid {
            assert!(artifacts.diagnostics.iter().any(|d| d.code == "T029" && d.primary_message.contains("Hash")), "{:?}", artifacts.diagnostics);
            assert!(artifacts.core_files.is_empty());
        } else {
            assert!(artifacts.diagnostics.is_empty(), "{:?}", artifacts.diagnostics);
            assert!(!artifacts.core_files.is_empty());
        }
    }
}

#[test]
fn compound_hash_rejects_missing_contracts_and_manual_equality_derives() {
    for (source, code) in [
        ("type Bad = { value: Float } deriving (Eq, Hash)\n", "T029"),
        ("type Bad = { value: fn Int -> Int } deriving (Eq, Hash)\n", "T029"),
        ("type Bad = { value: Hasher } deriving (Eq, Hash)\n", "T029"),
        ("type Bad = { value: Int } deriving (Hash)\n", "T033"),
        ("type Key = { id: Int, label: Text } deriving (Hash)\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Bool = eq left.id right.id\n", "T029"),
        ("type Pair a = (a, Int) deriving (Eq, Hash)\npub fn invalid (value: Pair a) (hasher: Hasher) -> Hasher = hash value hasher\n", "T055"),
        ("fn ready () = true\npub fn invalid (hasher: Hasher) -> Hasher = hash [ready] hasher\n", "T029"),
        ("fn absent () -> Option Float = None\npub fn invalid (hasher: Hasher) -> Hasher = hash (absent ()) hasher\n", "T029"),
        ("fn value () -> Result Int Float = Ok 7\npub fn invalid (hasher: Hasher) -> Hasher = hash (value ()) hasher\n", "T029"),
        ("pub fn invalid (hasher: Hasher) -> Hasher = hash ({ value = 1.0 }) hasher\n", "T029"),
    ] {
        let dir = workspace(source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(CompileOptions::new(dir.path().to_path_buf()).with_emit(EmitArtefacts::Core).with_cache_root(cache.path().to_path_buf())).unwrap();
        assert!(!artifacts.diagnostics.iter().any(|d| d.code.starts_with('P') || d.code.starts_with('L') || d.code.starts_with('R')), "{source}: {:?}", artifacts.diagnostics);
        assert!(artifacts.diagnostics.iter().any(|d| d.code == code), "{source}: {:?}", artifacts.diagnostics);
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn primitive_hash_states_preserve_branches_framing_and_value_identity() {
    let source = r"
import std.decimal as Decimal
fn emit (value: a) (hasher: Hasher) -> Hasher where Hash a =
    let write = hash
    write value hasher
pub fn feedInt (value: Int) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedBool (value: Bool) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedText (value: Text) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedUnit (value: Unit) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedOrdering (value: Ordering) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedBytes (value: Bytes) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedUuid (value: Uuid) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedTimestamp (value: Timestamp) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedDate (value: Date) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedTime (value: Time) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedDecimal (value: Decimal) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedHandle (value: Handle Int) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedSupervisor (value: Supervisor Int) (hasher: Hasher) -> Hasher = emit value hasher
pub fn feedMonitor (value: Monitor) (hasher: Hasher) -> Hasher = emit value hasher
pub fn rounded (hasher: Hasher) -> Hasher = emit (Decimal.truncate (-1) 120m) hasher
type Key = { id: Int, label: Text }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = eq left.id right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = emit value.id hasher
pub fn key (label: Text) (hasher: Hasher) -> Hasher = emit (Key { id = 7, label = label }) hasher
";
    let expression = r#"
Seed = <<0:128>>, S = ridge_rt:hash_new(Seed), Pid = self(), Ref = make_ref(),
{ok,Uuid} = ridge_rt:uuid_from_text(<<"00112233-4455-6677-8899-aabbccddeeff">>),
{ok,Date} = ridge_rt:date_from_iso(<<"2026-10-30">>),
{ok,Time} = ridge_rt:tod_from_iso(<<"12:34:56.123456">>),
Cases = [{feedInt,7,int,7}, {feedBool,true,bool,true}, {feedText,<<"hi">>,text,<<"hi">>},
 {feedUnit,ok,unit,ok}, {feedOrdering,'Greater',ordering,'Greater'},
 {feedBytes,<<0,255>>,bytes,<<0,255>>}, {feedUuid,Uuid,uuid,Uuid},
 {feedTimestamp,{timestamp,1234567},timestamp,{timestamp,1234567}},
 {feedDate,Date,date,Date}, {feedTime,Time,time,Time},
 {feedDecimal,{decimal,150,2},decimal,{15,1}},
 {feedHandle,{ridge_handle,Pid,unbounded},handle,{direct,Pid}},
 {feedSupervisor,{ridge_sup,Pid},supervisor,{ridge_sup,Pid}}, {feedMonitor,Ref,monitor,Ref}],
lists:foreach(fun({F,V,Tag,Canonical}) ->
 Encoded = term_to_binary({Tag,Canonical}), Frame = [<<(byte_size(Encoded)):64>>,Encoded],
 ExpectedState = {ridge_hasher,Seed,[Frame]},
 ExpectedState = erlang:apply(ridge_app_Main,F,[V,S]),
 <<Expected:64,_/binary>> = crypto:mac(hmac,sha256,Seed,Frame),
 Expected = ridge_rt:hash_finish(ExpectedState)
end,Cases),
OtherSeed = <<1:128>>, Other = ridge_app_Main:feedInt(7,ridge_rt:hash_new(OtherSeed)),
{ridge_hasher,OtherSeed,[OtherFrame]} = Other,
<<OtherExpected:64,_/binary>> = crypto:mac(hmac,sha256,OtherSeed,OtherFrame),
OtherExpected = ridge_rt:hash_finish(Other),
A = ridge_app_Main:feedInt(7,S), Before = ridge_rt:hash_finish(A),
_ = ridge_app_Main:feedText(<<"branch">>,A), Before = ridge_rt:hash_finish(A),
S = ridge_rt:hash_new(Seed), A = ridge_app_Main:key(<<"first">>,S), A = ridge_app_Main:key(<<"later">>,S),
FramedA = ridge_app_Main:feedText(<<"c">>,ridge_app_Main:feedText(<<"ab">>,S)),
FramedB = ridge_app_Main:feedText(<<"bc">>,ridge_app_Main:feedText(<<"a">>,S)),
false = FramedA =:= FramedB,
D = ridge_app_Main:feedDecimal({decimal,15,1},S), D = ridge_app_Main:feedDecimal({decimal,150,2},S),
Z = ridge_app_Main:feedDecimal({decimal,0,0},S), Z = ridge_app_Main:feedDecimal({decimal,0,99},S),
N = ridge_app_Main:feedDecimal({decimal,-120,0},S), N = ridge_app_Main:feedDecimal({decimal,-12,-1},S),
R = ridge_app_Main:feedDecimal({decimal,120,0},S), R = ridge_app_Main:rounded(S),
H = ridge_app_Main:feedHandle({ridge_handle,Pid,unbounded},S),
H = ridge_app_Main:feedHandle({ridge_handle,Pid,{bounded,3,drop_newest}},S),
SH = ridge_app_Main:feedHandle({ridge_sup_handle,Pid,child,unbounded},S),
SH = ridge_app_Main:feedHandle({ridge_sup_handle,Pid,child,{bounded,3,error}},S),
false = H =:= SH,
false = SH =:= ridge_app_Main:feedHandle({ridge_sup_handle,Pid,other,unbounded},S),
false = ridge_app_Main:feedMonitor(Ref,S) =:= ridge_app_Main:feedMonitor(make_ref(),S),
halt().
"#;
    assert_program_eval(source, expression, &[]);
}

#[test]
fn primitive_hash_evidence_keeps_unsupported_values_out_of_the_state() {
    for ty in [
        "Float",
        "Hasher",
        "fn Int -> Int",
        "Instant",
        "JsonValue",
        "SqlValue",
    ] {
        let dir = workspace(&format!(
            "import std.sql (SqlValue)\npub fn invalid (value: {ty}) (hasher: Hasher) -> Hasher = hash value hasher\n"
        ));
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            !artifacts.diagnostics.iter().any(|d| d.code.starts_with('P')
                || d.code.starts_with('L')
                || d.code.starts_with('R')),
            "{ty}: {:?}",
            artifacts.diagnostics
        );
        assert!(
            artifacts.diagnostics.iter().any(|d| d.code == "T029"),
            "{ty}: {:?}",
            artifacts.diagnostics
        );
        assert!(artifacts.core_files.is_empty());
    }
}

#[test]
fn manual_hash_methods_forward_opaque_state_and_supply_eq_superclasses() {
    let source = r"
type Key = { id: Int }
instance Eq Key =
    eq (left: Key) (right: Key) -> Bool = left.id == right.id
instance Hash Key =
    hash (value: Key) (hasher: Hasher) -> Hasher = hasher
fn emit (value: a) (hasher: Hasher) -> Hasher where Hash a =
    let write = hash
    write value hasher
fn same (left: a) (right: a) -> Bool where Hash a = eq left right
pub fn forward (hasher: Hasher) -> Hasher = emit (Key { id = 7 }) hasher
pub fn exercise () -> Bool =
    same (Key { id = 7 }) (Key { id = 7 }) &&
    same (Key { id = 7 }) (Key { id = 8 }) == false
";
    assert_program_eval(
        source,
        "State={ridge_hasher,<<0:128>>,[]},State=ridge_app_Main:forward(State),true=ridge_app_Main:exercise(),halt().",
        &[],
    );
}

#[test]
fn hash_state_and_method_contracts_reject_invalid_programs_before_execution() {
    let mut accepted = Vec::new();
    for source in [
        "pub fn invalid (hasher: Hasher) -> Bool = eq hasher hasher\n",
        "import std.sql as Sql\npub fn invalid (row: Map Text SqlValue) -> Result Hasher Error = Sql.fromRow row\n",
        "class Hash a =\n    hash (value: a) (hasher: Int) -> Int\n",
        "class Eq a =\n    eq (left: a) (right: a) -> Int\ntype Key = { id: Int }\ninstance Eq Key =\n    eq (left: Key) (right: Key) -> Int = 7\npub fn invalid () -> Bool = Key { id = 1 } == Key { id = 2 }\n",
        "class Ord a =\n    compare (left: a) (right: a) -> Int\ntype Key = { id: Int }\ninstance Ord Key =\n    compare (left: Key) (right: Key) -> Int = 0\npub fn invalid () -> Bool = Key { id = 1 } < Key { id = 2 }\n",
        "type Hasher = { value: Int }\n",
        "type Key = { id: Int }\ninstance Hash Key =\n    hash (value: Key) (hasher: Hasher) -> Hasher = hasher\n",
        "type Key = { id: Int } deriving (Eq)\ninstance Hash Key =\n    hash (value: Key) (hasher: Int) -> Int = hasher\n",
        "import std.io as Io\ntype Key = { id: Int } deriving (Eq)\ninstance Hash Key =\n    hash (value: Key) (hasher: Hasher) -> Hasher =\n        Io.println \"unexpected effect\"\n        hasher\n",
        "class Supply a =\n    supply (value: a) -> Hasher\ntype Key = { id: Int }\ninstance Supply Key =\n    supply (value: Key) -> Int = 1\npub fn invalid () -> Hasher = supply (Key { id = 1 })\n",
        "class Describe a =\n    describe (value: a) -> Text\ntype Box a = { value: a }\ninstance Describe (Box a) =\n    describe (value: Box Int) -> Text = \"specialized\"\n",
        "class Pair a =\n    first (value: a) -> Int\n    second (value: a) -> Int\ntype Key = { id: Int }\ninstance Pair Key =\n    first (value: Key) -> Int = value.id\n",
        "pub fn invalid () -> Hasher = Hasher { value = 1 }\n",
        "class Inspect a =\n    inspect (value: a) -> Text\ninstance Inspect (fn { value: Int | r } -> Int) =\n    inspect (value: fn { value: Int, extra: Text } -> Int) -> Text = \"narrowed\"\n",
    ] {
        let dir = workspace(source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            !artifacts.diagnostics.iter().any(|error| error.code.starts_with('P') || error.code.starts_with('L')),
            "invalid fixture syntax: {source}: {:?}", artifacts.diagnostics
        );
        if artifacts.diagnostics.is_empty() {
            accepted.push(source);
        } else {
            assert!(artifacts.core_files.is_empty(), "must not emit: {source}");
        }
    }
    assert!(accepted.is_empty(), "must reject before Core: {accepted:?}");
}

#[test]
fn outer_join_terminals_require_rows_for_both_entities() {
    for shape in ["LeftJoin", "RightJoin", "FullJoin"] {
        for terminal in [
            "Repo.exists query",
            "Repo.every (fn (u: User) (p: Post) -> u.age >= 0) query",
        ] {
            let valid = format!(
                "import std.data (DbError, MemAdapter)\nimport std.sql (Row)\nimport std.repo as Repo\nimport std.repo ({shape})\ntype User = {{ age: Int }} deriving (Row)\ntype Post = {{ author: Int }} deriving (Row)\npub fn db probe (query: {shape} User Post MemAdapter) -> Result Bool DbError = {terminal}\n"
            );
            for (source, missing) in [
                (valid.clone(), false),
                (valid.replace(" deriving (Row)", ""), false),
                (
                    valid.replace(
                        "type User = { age: Int } deriving (Row)",
                        "opaque type User = { age: Int }",
                    ),
                    true,
                ),
                (
                    valid.replace(
                        "type Post = { author: Int } deriving (Row)",
                        "opaque type Post = { author: Int }",
                    ),
                    true,
                ),
            ] {
                let dir = workspace(&source);
                std::fs::write(
                    dir.path().join("app/ridge.toml"),
                    "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"db\"]\n",
                ).unwrap();
                let cache = tempfile::tempdir().unwrap();
                let artifacts = compile_workspace(
                    CompileOptions::new(dir.path().to_path_buf())
                        .with_emit(EmitArtefacts::Core)
                        .with_cache_root(cache.path().to_path_buf()),
                )
                .unwrap();
                if missing {
                    assert!(
                        artifacts
                            .diagnostics
                            .iter()
                            .any(|error| error.code == "T029"),
                        "{source}: {:?}",
                        artifacts.diagnostics
                    );
                    assert!(!artifacts.diagnostics.iter().any(|error| error.code.starts_with('P') || error.code.starts_with('L')), "{:?}", artifacts.diagnostics);
                    assert!(artifacts.core_files.is_empty());
                } else {
                    assert!(
                        artifacts.diagnostics.is_empty(),
                        "{source}: {:?}",
                        artifacts.diagnostics
                    );
                    assert!(!artifacts.core_files.is_empty());
                }
            }
        }
    }
}

#[test]
fn class_methods_preserve_generic_instance_heads() {
    assert_program_result(
        r#"
class Describe a =
    describe (value: a) -> Text
type Box a = { value: a }
instance Describe (Box a) =
    describe (value: Box a) -> Text = "box"
pub fn exercise () -> Text = describe (Box { value = "text" })
"#,
        "<<\"box\">>",
    );
}

#[test]
fn auto_promoted_renderers_do_not_hide_declared_effects() {
    let renderer = "pub fn render (value: a) -> Text where ToText a = toText value\n";
    for (caps, body, invalid) in [
        ("", "\"rendered\"", false),
        (
            "io ",
            "Io.println \"unexpected effect\"\n    \"rendered\"",
            true,
        ),
        ("io ", "\"rendered\"", true),
    ] {
        let source = format!(
            r"
import app.Renderer (render)
import std.io as Io
type Printable = {{ value: Int }}
pub fn {caps}toText (value: Printable) -> Text =
    {body}
pub fn exercise () -> Text = render (Printable {{ value = 7 }})
"
        );
        let dir = workspace(&source);
        std::fs::write(dir.path().join("app/src/Renderer.ridge"), renderer).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            !artifacts
                .diagnostics
                .iter()
                .any(|error| error.code.starts_with('P') || error.code.starts_with('L')),
            "{:?}",
            artifacts.diagnostics
        );
        if invalid {
            assert!(
                artifacts
                    .diagnostics
                    .iter()
                    .any(|error| error.code == "T029"),
                "effectful function must not supply pure ToText: {source}\n{:?}",
                artifacts.diagnostics
            );
            assert!(artifacts.core_files.is_empty());
        } else {
            assert!(
                artifacts.diagnostics.is_empty(),
                "{:?}",
                artifacts.diagnostics
            );
            assert!(!artifacts.core_files.is_empty());
        }
    }
}

#[test]
fn effectful_to_text_functions_remain_callable_with_explicit_capabilities() {
    assert_program_result(
        r#"
type Printable = { value: Int }
pub fn io toText (value: Printable) -> Text = "ordinary"
pub fn io exercise () -> Text = toText (Printable { value = 7 })
"#,
        "<<\"ordinary\">>",
    );
}

#[test]
fn class_methods_preserve_open_rows_inside_function_heads() {
    assert_program_result(
        r#"
class Inspect a =
    inspect (value: a) -> Text
instance Inspect (fn { value: Int | r } -> Int) =
    inspect (value: fn { value: Int | r } -> Int) -> Text = "open"
fn read (value: { value: Int | r }) -> Int = value.value
pub fn exercise () -> Text = inspect read
"#,
        "<<\"open\">>",
    );
}

#[test]
fn class_methods_without_arguments_dispatch_from_the_declared_return() {
    assert_program_result(
        r#"
class Make a =
    make () -> a
instance Make Int =
    make () -> Int = 7
instance Make Text =
    make () -> Text = "made"
class FromUnit a =
    fromUnit (token: Unit) -> a
instance FromUnit Int =
    fromUnit (token: Unit) -> Int = 11
instance FromUnit Text =
    fromUnit (token: Unit) -> Text = "unit"
fn number () -> Int = make ()
fn label () -> Text = make ()
fn supplied () -> a where Make a = make ()
fn suppliedNumber () -> Int = supplied ()
fn suppliedLabel () -> Text = supplied ()
fn unitNumber () -> Int = fromUnit ()
fn unitLabel () -> Text = fromUnit ()
pub fn exercise () -> (Int, Text, Int, Text, Int, Text) =
    (number (), label (), suppliedNumber (), suppliedLabel (), unitNumber (), unitLabel ())
"#,
        "{7,<<\"made\">>,7,<<\"made\">>,11,<<\"unit\">>}",
    );
}

#[test]
fn return_directed_class_methods_remain_first_class_values() {
    assert_program_result(
        r#"
class Build a =
    build () -> a
instance Build Int =
    build () -> Int = 7
instance Build Text =
    build () -> Text = "built"
fn number (factory: fn () -> Int) -> Int = factory ()
fn label (factory: fn () -> Text) -> Text = factory ()
pub fn exercise () -> (Int, Text) = (number build, label build)
"#,
        "{7,<<\"built\">>}",
    );
}

#[test]
fn class_signatures_keep_their_declaring_modules_private_aliases() {
    let owner = r"
type Value = Int
pub class Supply a =
    supply (value: a) -> Value
pub type Key = { id: Int }
instance Supply Key =
    supply (value: Key) -> Value = value.id
";
    let source = r"
import app.Owner (Key)
type Value = Hasher
pub fn exercise () -> Int = supply (Key { id = 7 })
";
    assert_program_files(source, "7", &[("Owner", owner)]);
    let invalid = source.replace("exercise () -> Int", "exercise () -> Hasher");
    let dir = workspace(&invalid);
    std::fs::write(dir.path().join("app/src/Owner.ridge"), owner).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts
            .diagnostics
            .iter()
            .any(|error| error.code == "T001"),
        "{:?}",
        artifacts.diagnostics
    );
    assert!(artifacts.core_files.is_empty());
}
