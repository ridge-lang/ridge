//! Property-based fuzzing of the parser front-end (lex -> layout -> parse).
//!
//! The parser is a hard guarantee, not a best effort: for *any* input —
//! including malformed, adversarial, or pure-garbage bytes — `parse_source`
//! must return a (possibly error-laden) result and never panic, abort, or
//! overflow the native stack. These tests drive `parse_source` with two
//! generators and assert that guarantee plus a few cheap structural invariants:
//!
//! 1. random Unicode text weighted toward the lexer's significant characters
//!    (brackets, quotes, `$`, `--`, operators, layout whitespace), and
//! 2. grammar-biased "token soup" — sequences drawn from the real Ridge lexeme
//!    vocabulary, which reaches far deeper into the parser's recovery paths than
//!    pure-random text usually can.
//!
//! A separate deterministic suite pins the recursion-depth guarantee: deeply
//! nested types, patterns, and expressions must report `P028` rather than
//! overflow the stack.
//!
//! Everything runs on a thread with a deliberately large stack so that the
//! parser's own depth limit (`MAX_PARSE_DEPTH`) is what stops descent, not the
//! test harness's smaller default stack. A found counterexample is shrunk by
//! `proptest` and persisted under `proptest-regressions/` as a permanent
//! regression.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items
)]

use proptest::prelude::*;
use ridge_parser::{parse_source, ParseResult};

// ── Big-stack runner ──────────────────────────────────────────────────────────

/// Run `f` on a thread with a 64 MiB stack and propagate any panic.
///
/// `libtest` runs each `#[test]` on a thread whose default stack (≈2 MiB) is too
/// small to hold the parser's full `MAX_PARSE_DEPTH` (256) recursion in an
/// unoptimised build, so a legitimately bounded-but-deep input could overflow
/// the *test* thread before the parser's own guard fires. A generous stack makes
/// the guard the limiting factor, which is the property under test.
fn on_big_stack(f: impl FnOnce() + Send + 'static) {
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .name("ridge-parser-fuzz".to_string())
        .spawn(f)
        .expect("failed to spawn fuzz thread");
    if let Err(payload) = handle.join() {
        // Re-raise the property failure (with its shrunk counterexample) on the
        // main thread so the test reports it.
        std::panic::resume_unwind(payload);
    }
}

// ── Invariants ────────────────────────────────────────────────────────────────

/// Assert the structural invariants every parse must uphold, regardless of how
/// malformed the input is.
fn check_invariants(src: &str, r: &ParseResult) {
    let len = u32::try_from(src.len()).unwrap_or(u32::MAX);

    let module = r.module.span;
    assert!(
        module.start <= module.end,
        "module span is inverted: {}..{}",
        module.start,
        module.end
    );
    assert!(
        module.end <= len,
        "module span end {} exceeds source length {len}",
        module.end
    );

    for e in &r.errors {
        let sp = e.span();
        assert!(
            sp.start <= sp.end,
            "error {} span is inverted: {}..{}",
            e.code(),
            sp.start,
            sp.end
        );
        assert!(
            sp.end <= len,
            "error {} span end {} exceeds source length {len}",
            e.code(),
            sp.end
        );
    }
}

/// Parse `src` and assert the parser is deterministic: a second parse of the
/// same bytes yields the same error and item counts.
fn check_parse(src: &str) {
    let first = parse_source(src);
    check_invariants(src, &first);

    let second = parse_source(src);
    assert_eq!(
        first.errors.len(),
        second.errors.len(),
        "parse error count is non-deterministic"
    );
    assert_eq!(
        first.module.items.len(),
        second.module.items.len(),
        "parsed item count is non-deterministic"
    );
}

// ── Strategy 1: random significant-character text ─────────────────────────────

/// Characters that carry syntactic meaning to the lexer/parser. Weighting the
/// generator toward these reaches real productions far more often than uniform
/// Unicode would.
const STRUCTURAL_CHARS: &[char] = &[
    '(', ')', '[', ']', '{', '}', ',', ':', '.', '=', '-', '>', '<', '|', '+', '*', '/', '%', '^',
    '&', '!', '?', '@', '_', '"', '$', '\\', ';', '~', '`', '#',
];

/// Layout-significant whitespace (an offside-rule language cares about these).
const WS_CHARS: &[char] = &[' ', '\n', '\t', '\r'];

fn arb_char() -> BoxedStrategy<char> {
    prop_oneof![
        30 => prop::sample::select(STRUCTURAL_CHARS).boxed(),
        15 => prop::sample::select(WS_CHARS).boxed(),
        10 => (b'a'..=b'z').prop_map(char::from).boxed(),
        8 => (b'A'..=b'Z').prop_map(char::from).boxed(),
        5 => (b'0'..=b'9').prop_map(char::from).boxed(),
        // Arbitrary scalar values: control characters, emoji, CJK, the lot.
        3 => any::<char>().boxed(),
    ]
    .boxed()
}

fn arb_text() -> impl Strategy<Value = String> {
    prop::collection::vec(arb_char(), 0..300).prop_map(|cs| cs.into_iter().collect())
}

// ── Strategy 2: grammar-biased token soup ─────────────────────────────────────

/// The real Ridge lexeme vocabulary: every keyword, a sample of identifiers and
/// literals, all operators and punctuation, plus newline / indent. Sequencing
/// these (rather than random characters) keeps the lexer producing valid tokens,
/// so the parser advances deep into declaration, expression, and recovery paths.
const LEXEMES: &[&str] = &[
    // keywords
    "fn", "type", "let", "var", "const", "import", "match", "if", "then", "else", "actor", "on",
    "init", "state", "spawn", "with", "try", "guard", "when", "true", "false", "as", "in", "where",
    "catch", "class", "deriving", "instance", "pub", "opaque", "return",
    // identifiers, capabilities, literals
    "x", "y", "foo", "Foo", "Bar", "io", "fs", "0", "1", "42", "3.14", "\"s\"", "r\"s\"", "_",
    // operators and punctuation
    "(", ")", "[", "]", "{", "}", ",", ":", "::", ".", "=", "->", "<-", "=>", "|", "|>", "?", "?>",
    "!", "@", "..", "++", "+", "-", "*", "/", "%", "^", "&&", "||", "==", "!=", "<", ">", "<=",
    ">=", "$\"", "${", // layout
    "\n", "  ",
];

fn arb_token_soup() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(LEXEMES), 0..200).prop_map(|toks| toks.join(" "))
}

// ── Property tests ────────────────────────────────────────────────────────────

proptest! {
    // 1024 cases per property; persist any shrunk counterexample under
    // `proptest-regressions/` so it replays as a permanent regression. The path
    // is set explicitly because the default `SourceParallel` strategy cannot
    // locate a crate root from an integration-test file.
    #![proptest_config(ProptestConfig {
        cases: 1024,
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(
                "proptest-regressions/fuzz.txt",
            ),
        )),
        ..ProptestConfig::default()
    })]

    /// Random significant-character text never breaks the parser.
    #[test]
    fn arbitrary_text_never_panics(src in arb_text()) {
        on_big_stack(move || check_parse(&src));
    }

    /// Grammar-biased token sequences never break the parser.
    #[test]
    fn token_soup_never_panics(src in arb_token_soup()) {
        on_big_stack(move || check_parse(&src));
    }
}

// ── Deterministic recursion-depth regressions ─────────────────────────────────

/// `open` × `depth`, then `core`, then `close` × `depth`.
fn wrap(open: &str, core: &str, close: &str, depth: usize) -> String {
    format!("{}{core}{}", open.repeat(depth), close.repeat(depth))
}

fn deep_paren_type(depth: usize) -> String {
    format!("const x: {} = 0\n", wrap("(", "Int", ")", depth))
}

fn deep_list_type(depth: usize) -> String {
    format!("const x: {} = 0\n", wrap("[", "Int", "]", depth))
}

fn deep_arrow_type(depth: usize) -> String {
    format!("const x: {}Int = 0\n", "Int -> ".repeat(depth))
}

fn deep_paren_expr(depth: usize) -> String {
    format!("const x: Int = {}\n", wrap("(", "0", ")", depth))
}

fn deep_let_pattern(depth: usize) -> String {
    format!("fn f x =\n  let {} = x\n  y\n", wrap("(", "y", ")", depth))
}

fn deep_lambda_pattern(depth: usize) -> String {
    format!("const g: Int = (\\{} -> z)\n", wrap("(", "z", ")", depth))
}

/// Assert the parse reported the `P028` depth limit (and, by returning at all,
/// did not overflow the stack).
fn assert_p028(src: &str) {
    let r = parse_source(src);
    let codes: Vec<&str> = r
        .errors
        .iter()
        .map(ridge_parser::ParseError::code)
        .collect();
    assert!(
        codes.contains(&"P028"),
        "expected a P028 depth-limit error, got {codes:?}"
    );
}

#[test]
fn deeply_nested_syntax_reports_p028_without_overflow() {
    on_big_stack(|| {
        // Depths far past `MAX_PARSE_DEPTH` (256) and past what would overflow an
        // unguarded recursive descent (~thousands of levels). Each must stop at
        // the guard and report P028 instead of aborting the process.
        for depth in [1_000usize, 100_000] {
            assert_p028(&deep_paren_type(depth));
            assert_p028(&deep_list_type(depth));
            assert_p028(&deep_arrow_type(depth));
            assert_p028(&deep_paren_expr(depth));
            assert_p028(&deep_let_pattern(depth));
            assert_p028(&deep_lambda_pattern(depth));
        }
    });
}

#[test]
fn deeply_nested_match_pattern_does_not_overflow() {
    on_big_stack(|| {
        // A deeply nested pattern inside a `match` arm: the arm scanner bails
        // before the pattern guard fires, so the surfaced code differs, but the
        // parser must still return cleanly rather than overflow.
        for depth in [1_000usize, 100_000] {
            let src = format!("fn f x = match x\n  {} -> 0\n", wrap("[", "", "]", depth));
            let r = parse_source(&src);
            check_invariants(&src, &r);
            assert!(
                !r.errors.is_empty(),
                "a malformed deep pattern should error"
            );
        }
    });
}
