//! Grammar↔parser conformance guard.
//!
//! `docs/grammar.ebnf` is the language's normative surface syntax. Nothing kept
//! it and the hand-written parser in step, so the parser had quietly drifted
//! *below* the grammar — rejecting forms the spec allows. This test pins the two
//! together in both directions:
//!
//! 1. [`CORPUS`] holds a source module per syntactic production (tagged with the
//!    productions it exercises). Every entry must parse with no lex or parse
//!    error, so a regression that stops accepting a spec form fails here.
//! 2. [`corpus_covers_every_syntactic_production`] extracts the production names
//!    from the grammar file itself and asserts each one is either exercised by
//!    the corpus or listed in [`EXEMPT`]. Add a production to the grammar and the
//!    test fails until the corpus covers it — the anti-drift ratchet.
//! 3. [`KNOWN_DIVERGENCES`] records grammar-legal forms the parser does *not* yet
//!    accept. They are asserted to still fail, so when a later change starts
//!    accepting one the test flags it for promotion into [`CORPUS`].
//!
//! Only surface syntax is covered; the parser accepts a few forms *beyond* the
//! grammar (e.g. list-destructuring patterns) — reconciling those belongs to the
//! grammar docs, not here.

#![allow(clippy::panic)]

use std::collections::BTreeSet;

use ridge_parser::parse_source;

const GRAMMAR: &str = include_str!("../../../docs/grammar.ebnf");

/// `(production tags, source)`. Each source must parse cleanly; the tags are the
/// grammar productions the source is claimed to exercise.
const CORPUS: &[(&[&str], &str)] = &[
    (
        &[
            "Program",
            "TopLevel",
            "ImportDecl",
            "ModulePath",
            "ImportList",
            "ImportItem",
        ],
        "\
import std.list as List
import std.map (get, insert)
import std.net.http as Http (Request, listen)
",
    ),
    (
        &["ConstDecl", "Visibility"],
        "\
pub const maxRetries: Int = 3
const pi: Float = 3.14159
",
    ),
    (
        &[
            "TypeDecl",
            "TypeParam",
            "TypeBody",
            "RecordType",
            "FieldDecl",
        ],
        "type User = { name: Text, email: Text, age: Int }\n",
    ),
    (
        &["UnionType", "Constructor", "Deriving"],
        "\
type Shape =
    | Circle Float
    | Rectangle Float Float
    | Login { userId: Int, at: Timestamp }
    deriving (Eq, Ord)
",
    ),
    (
        &["TypeApp", "TypeVariable"],
        "\
type Pair a b = { first: a, second: b }
type Id = Int
type Names = List Text
",
    ),
    (
        &[
            "Type",
            "FunctionType",
            "CapFunctionType",
            "PlainFunctionType",
            "TypeAtom",
            "PrimitiveType",
            "TupleType",
            "ListTypeApply",
        ],
        "\
fn applyTwice (f: Int -> Int) (x: Int) -> Int = f (f x)
fn withCap (g: fn io Text -> Unit) -> Unit = ()
fn pairFirst (t: (Int, Text)) -> Int = 0
fn firstName (xs: [Text]) -> Option Text = List.head xs
fn grouped (x: (Int)) -> Int = x
",
    ),
    (
        &["FnDecl", "FnName", "Param", "Body", "Capability"],
        "\
fn add (x: Int) (y: Int) -> Int = x + y
fn io net fetch (url: Text) -> Unit = ()
fn _helper (n: Int) -> Int = n
fn noArgs () -> Int = 0
",
    ),
    (
        &["WhereClause", "ClassConstraint"],
        "fn showIt (x: a) -> Text where ToText a = ToText.toText x\n",
    ),
    (
        &[
            "ActorDecl",
            "ActorBody",
            "ActorMember",
            "StateDecl",
            "InitDecl",
            "CapList",
            "ParamList",
            "Block",
            "OnHandler",
            "HandlerName",
        ],
        "\
actor Counter =
    state count: Int = 0
    init (start: Int) =
        count <- start
    on increment =
        count <- count + 1
    on io get -> Int =
        count
",
    ),
    (
        &[
            "ClassDecl",
            "ClassBody",
            "MethodSig",
            "FunDeps",
            "FunDep",
            "SuperList",
        ],
        "\
class ToText a =
    toText (x: a) -> Text
class Ord a where Eq a =
    compare (x: a) (y: a) -> Ordering
class Tagged q p | q -> p =
    tagWith (tag: p) (x: q) -> q
",
    ),
    (
        &["InstanceDecl", "InstanceBody", "MethodDef"],
        "\
instance ToText Bool =
    toText (b: Bool) -> Text = \"b\"
instance Encode (List a) where Encode a =
    encode (xs: List a) -> Text = \"e\"
",
    ),
    (
        &[
            "Expr",
            "LetExpr",
            "VarDecl",
            "AssignExpr",
            "IfExpr",
            "ReturnExpr",
        ],
        "\
fn demo (flag: Bool) -> Int =
    let x = 1
    var y = 2
    y <- y + 1
    if flag then
        return y
    else
        ()
    if x > 0 then y else 0
",
    ),
    (
        &["Pattern", "AsPattern", "PatternAtom"],
        "\
fn asPat (u: User) -> Text =
    match u
        admin @ User { role = Admin } -> \"admin\"
        other -> \"other\"
",
    ),
    (
        &[
            "MatchExpr",
            "MatchArm",
            "OrPattern",
            "WildcardPattern",
            "LiteralPattern",
            "VarPattern",
            "ConstructorPattern",
            "FieldPatternList",
            "FieldPattern",
            "TuplePattern",
            "ListConsPattern",
            "PatternArg",
        ],
        "\
fn classify (x: Shape) (n: Int) (xs: List Int) (t: (Int, Int)) -> Text =
    match x
        Circle r -> \"circle\"
        Rectangle w h -> \"rect\"
        Login { userId, at } -> \"login\"
        _ -> \"other\"
    match n
        0 -> \"zero\"
        1 | 2 | 3 -> \"few\"
        m when m < 10 -> \"small\"
        _ -> \"big\"
    match xs
        head :: rest -> head
        _ -> 0
    match t
        (a, b) -> a
",
    ),
    (
        &[
            "Expr1", "Expr2", "Expr3", "Expr4", "Expr5", "Expr6", "Expr7", "Expr8", "Expr9",
            "Expr10", "Expr11",
        ],
        "\
fn ops (a: Int) (b: Int) (xs: List Int) -> Bool =
    let p = xs |> List.sum
    let q = a > 0 || b < 0 && a == b
    let r = a + b * 2 - 4 / 2 % 3 ^ 2
    let s = a :: xs ++ xs
    let t = -a
    let u = max a b
    a != b
",
    ),
    (
        &["Expr12", "AskExpr", "QualifiedName"],
        "\
fn dotted (h: Handle Counter) (u: User) -> Unit =
    let name = u.profile.name
    let reply = h ?> get name
    h ! ping ()
",
    ),
    (
        &[
            "ExprAtom",
            "Literal",
            "UnitLiteral",
            "ListLiteral",
            "ExprList",
            "TupleLiteral",
            "InterpolatedText",
            "FieldAccessorFn",
            "LambdaExpr",
            "LambdaParam",
            "InnerFnExpr",
            "RecordConstruct",
            "FieldInit",
            "WithExpr",
            "SpawnExpr",
            "PropagateExpr",
        ],
        "\
fn atoms (m: User) -> Unit =
    let a = 42
    let b = 3.14
    let c = true
    let d = \"hello\"
    let e = ()
    let f = [1, 2, 3]
    let g = (1, \"two\", 3.0)
    let h = $\"val ${a} end\"
    let accessor = List.map (.name)
    let k = fn x -> x * 2
    let l = fn (x: Int) (y: Int) -> x + y
    fn inner (n: Int) -> Int = n + 1
    let built = User { name = \"a\", age = 30 }
    let short = User { name, age }
    let updated = m with { age = 31 }
    let child = spawn Counter 1 2
    let got = fetch a ?
    ()
",
    ),
    (
        &["TryExpr", "GuardExpr"],
        "\
fn tg (id: Int) -> Result Unit Error =
    guard (id > 0) else
        return Err (bad id)
    let outcome = try
        let a = step1 ?
        Ok a
    Ok ()
",
    ),
];

/// Grammar productions the parser corpus intentionally does not target: lexical
/// (lexer-level) productions, and a few structural aliases exercised implicitly
/// by every module. Each name must still be a real production in the grammar.
const EXEMPT: &[&str] = &[
    // Lexical — owned by the lexer, covered by its own tests/snapshots.
    "DIGIT",
    "LOWER",
    "UPPER",
    "LETTER",
    "ALNUM",
    "ID_CHAR",
    "HEX_DIGIT",
    "BIN_DIGIT",
    "OCT_DIGIT",
    "KEYWORD",
    "CAPABILITY_KW",
    "LOWER_IDENT",
    "UPPER_IDENT",
    "IDENT",
    "PRIV_IDENT",
    "INT_LIT",
    "DEC_LIT",
    "BIN_LIT",
    "OCT_LIT",
    "HEX_LIT",
    "FLOAT_LIT",
    "DECIMAL_LIT",
    "BOOL_LIT",
    "UNIT_LIT",
    "TEXT_CHAR",
    "TEXT_LIT",
    "INTERP_START",
    "INTERP_TEXT",
    "INTERP_EXPR_START",
    "INTERP_EXPR_END",
    "INTERP_END",
    "LINE_COMMENT",
    "DOC_COMMENT",
];

/// Grammar-legal forms the parser does not accept yet. Asserted to still fail so
/// that a change which starts accepting one is noticed and the form promoted
/// into [`CORPUS`].
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[];

/// Extract the left-hand-side production names from the grammar, in file order.
fn grammar_production_names() -> Vec<String> {
    let mut names = Vec::new();
    for line in GRAMMAR.lines() {
        // A production definition is `Name   ::= ...` at column 0 (no leading
        // space, so continuation lines and comments are skipped).
        if line.starts_with(|c: char| c.is_ascii_alphabetic()) {
            if let Some((lhs, _)) = line.split_once("::=") {
                let name = lhs.trim();
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

#[test]
fn every_corpus_snippet_parses_clean() {
    for (tags, src) in CORPUS {
        let result = parse_source(src);
        assert!(
            result.lex_errors.is_empty(),
            "lex errors for {tags:?}:\n{src}\n{:#?}",
            result.lex_errors
        );
        assert!(
            result.errors.is_empty(),
            "parse errors for {tags:?}:\n{src}\n{:#?}",
            result.errors
        );
    }
}

#[test]
fn known_divergences_still_diverge() {
    for (desc, src) in KNOWN_DIVERGENCES {
        let result = parse_source(src);
        assert!(
            !result.errors.is_empty() || !result.lex_errors.is_empty(),
            "known divergence now parses — promote it into CORPUS: {desc}\n{src}"
        );
    }
}

#[test]
fn corpus_tags_reference_real_productions() {
    let productions: BTreeSet<String> = grammar_production_names().into_iter().collect();
    for (tags, _) in CORPUS {
        for tag in *tags {
            assert!(
                productions.contains(*tag),
                "corpus tag `{tag}` is not a production in docs/grammar.ebnf (renamed or typo?)"
            );
        }
    }
    for name in EXEMPT {
        assert!(
            productions.contains(*name),
            "EXEMPT entry `{name}` is not a production in docs/grammar.ebnf (renamed or typo?)"
        );
    }
}

#[test]
fn corpus_covers_every_syntactic_production() {
    let productions = grammar_production_names();
    let covered: BTreeSet<&str> = CORPUS
        .iter()
        .flat_map(|(tags, _)| tags.iter().copied())
        .chain(EXEMPT.iter().copied())
        .collect();

    let missing: Vec<&String> = productions
        .iter()
        .filter(|p| !covered.contains(p.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "grammar productions with no corpus snippet (add a CORPUS entry or an EXEMPT justification): {missing:?}"
    );
}
