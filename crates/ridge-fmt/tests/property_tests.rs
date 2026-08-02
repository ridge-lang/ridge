//! Property-based tests for `ridge-fmt` (round-trip + idempotency).
//!
//! The golden fixtures pin *specific* rewrites; these properties pin the
//! formatter's two global guarantees for *any* input that parses:
//!
//! 1. **Idempotency** — `format(format(x)) == format(x)`.  The formatter has
//!    regressed here before (three distinct outputs across three passes), so
//!    this is the property under the most scrutiny.
//! 2. **Round-trip** — re-parsing the formatted output must succeed and
//!    produce the same top-level items as the original parse.  Spans are not
//!    compared: formatting moves things, so byte offsets legitimately change.
//!    What must survive is the item stream itself (kinds and names, in order).
//!
//! A third property covers malformed input: `format_source` must return
//! `Err(C101)` and never panic, no matter how hostile the bytes are.
//!
//! Inputs come from two generators:
//!
//! - the real `.ridge` corpus shipped in this workspace (`examples/`,
//!   `crates/ridge-stdlib/stdlib/`, `dogfood/`), optionally perturbed with
//!   whitespace mutations (blank lines, doubled spaces, trailing spaces,
//!   extra indentation) that leave the program meaningful but stress the
//!   pretty-printer far more than the clean corpus does, and
//! - grammar-biased token soup for the no-panic guarantee.
//!
//! Perturbed inputs that no longer parse are rejected via `prop_assume!`:
//! the properties only promise anything for parseable programs.
//!
//! Everything runs on a thread with a large stack so the parser's own depth
//! guard, not the test harness's default stack, is what bounds recursion.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items
)]

use proptest::prelude::*;
use ridge_fmt::format_source;

// ── Big-stack runner ──────────────────────────────────────────────────────────

/// Run `f` on a thread with a 64 MiB stack and propagate any panic.
///
/// Same rationale as `crates/ridge-parser/tests/fuzz.rs`: the formatter goes
/// through the parser, and a legitimately bounded-but-deep input must be
/// stopped by the parser's `MAX_PARSE_DEPTH` guard, not by the test thread's
/// smaller default stack.
fn on_big_stack(f: impl FnOnce() + Send + 'static) {
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .name("ridge-fmt-property".to_string())
        .spawn(f)
        .expect("failed to spawn property-test thread");
    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

// ── Corpus ────────────────────────────────────────────────────────────────────

/// A corpus entry: `(display path, source text)`.
type CorpusEntry = (String, String);

/// The workspace `.ridge` corpus, loaded once per test binary.
fn corpus() -> &'static [CorpusEntry] {
    static CORPUS: std::sync::OnceLock<Vec<CorpusEntry>> = std::sync::OnceLock::new();
    CORPUS.get_or_init(|| {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::Path::new(manifest_dir)
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .expect("could not determine workspace root from CARGO_MANIFEST_DIR");

        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "dogfood"] {
            collect_ridge_files(&workspace_root.join(dir), &mut files);
        }
        collect_ridge_files(
            &workspace_root
                .join("crates")
                .join("ridge-stdlib")
                .join("stdlib"),
            &mut files,
        );
        files.sort();

        assert!(
            !files.is_empty(),
            "corpus: no .ridge files found under examples/, dogfood/, or \
             crates/ridge-stdlib/stdlib/; verify the workspace layout"
        );

        files
            .into_iter()
            .map(|path| {
                let display = path
                    .strip_prefix(workspace_root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                let src = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
                (display, src)
            })
            .collect()
    })
}

/// Recursively collect all `.ridge` files under `dir`.
fn collect_ridge_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_ridge_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "ridge") {
            out.push(path);
        }
    }
}

// ── Perturbation ──────────────────────────────────────────────────────────────

/// Apply a batch of whitespace perturbations to `src`.
///
/// Each op is `(kind, arg)`; `kind % 4` selects the mutation and `arg` is
/// reduced modulo the current line count to pick a target line.  All four
/// mutations only add whitespace, so a file that still parses afterwards is
/// the same program wearing worse whitespace — exactly the input class the
/// formatter exists for.
fn perturb(src: &str, ops: &[(u8, usize)]) -> String {
    let mut lines: Vec<String> = src.lines().map(String::from).collect();
    if lines.is_empty() {
        return src.to_string();
    }
    for (kind, arg) in ops {
        let n = lines.len();
        let i = arg % n;
        match kind % 4 {
            // Insert a blank line before line `i`.
            0 => lines.insert(i, String::new()),
            // Append trailing spaces to line `i`.
            1 => lines[i].push_str("   "),
            // Double the first interior space run on line `i`.
            2 => {
                if let Some(pos) = lines[i].find(' ') {
                    lines[i].insert(pos, ' ');
                }
            }
            // Add two leading spaces to line `i` (breaks layout more often
            // than not; unparseable results are filtered by `prop_assume!`).
            _ => lines[i].insert_str(0, "  "),
        }
    }
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    out
}

// ── Structural fingerprints ───────────────────────────────────────────────────

/// Fingerprint the top-level item stream of a parsed module.
///
/// Each item reduces to its kind plus its defining name (module path for
/// imports).  The formatted output must yield the exact same sequence; spans
/// and everything below the item level are deliberately out of scope.
fn item_fingerprints(module: &ridge_ast::Module) -> Vec<String> {
    module
        .items
        .iter()
        .map(|item| match item {
            ridge_ast::Item::Import(d) => {
                let path = d
                    .path
                    .segments
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                format!("import {path}")
            }
            ridge_ast::Item::Const(d) => format!("const {}", d.name.text),
            ridge_ast::Item::Type(d) => {
                let opaque = if d.opaque { "opaque " } else { "" };
                format!("{opaque}type {}", d.name.text)
            }
            ridge_ast::Item::Fn(d) => format!("fn {}", d.name.text),
            ridge_ast::Item::Actor(d) => format!("actor {}", d.name.text),
            ridge_ast::Item::ClassDecl(d) => format!("class {}", d.name.text),
            ridge_ast::Item::InstanceDecl(d) => format!("instance {}", d.class.text),
        })
        .collect()
}

// ── Property helpers ──────────────────────────────────────────────────────────

/// Assert the round-trip property for one input: formatting must succeed,
/// the formatted output must re-parse cleanly, and the item stream must be
/// preserved.
fn assert_round_trip(label: &str, src: &str) {
    let formatted =
        format_source(src).unwrap_or_else(|e| panic!("{label}: format_source failed: {e}"));

    let original = ridge_parser::parse_source(src);
    let reparsed = ridge_parser::parse_source(&formatted);

    assert!(
        reparsed.errors.is_empty() && reparsed.lex_errors.is_empty(),
        "{label}: formatted output does not re-parse: {:?} / lex: {:?}",
        reparsed.errors,
        reparsed.lex_errors,
    );

    let before = item_fingerprints(&original.module);
    let after = item_fingerprints(&reparsed.module);
    assert_eq!(
        before, after,
        "{label}: formatting changed the top-level item stream"
    );
}

/// Assert the idempotency property for one input: a second format pass must
/// be a fixed point of the first.
fn assert_idempotent(label: &str, src: &str) {
    let first = format_source(src)
        .unwrap_or_else(|e| panic!("{label}: format_source (first pass) failed: {e}"));
    let second = format_source(&first)
        .unwrap_or_else(|e| panic!("{label}: format_source (second pass) failed: {e}"));
    assert_eq!(
        first, second,
        "{label}: formatter is not idempotent (second pass differs from first)"
    );
}

// ── Strategies ────────────────────────────────────────────────────────────────

/// A random member of the workspace corpus.
fn corpus_entry() -> impl Strategy<Value = CorpusEntry> {
    proptest::sample::select(corpus().to_vec())
}

/// A corpus member plus a batch of whitespace perturbations.
fn perturbed_entry() -> impl Strategy<Value = CorpusEntry> {
    (
        corpus_entry(),
        prop::collection::vec((any::<u8>(), any::<usize>()), 1..12),
    )
        .prop_map(|((path, src), ops)| {
            let perturbed = perturb(&src, &ops);
            (format!("{path} (perturbed with {ops:?})"), perturbed)
        })
        // Reject perturbations that broke parseability — the properties only
        // cover parseable programs.
        .prop_filter("perturbed input must still parse", |(_, src)| {
            let r = ridge_parser::parse_source(src);
            r.errors.is_empty() && r.lex_errors.is_empty()
        })
}

/// Grammar-biased token soup for the no-panic property.
fn token_soup() -> impl Strategy<Value = String> {
    let tokens: Vec<&'static str> = vec![
        "fn",
        "pub",
        "type",
        "opaque",
        "import",
        "const",
        "actor",
        "state",
        "on",
        "class",
        "instance",
        "match",
        "if",
        "then",
        "else",
        "let",
        "do",
        "end",
        "where",
        "deriving",
        "migrate",
        "->",
        "=>",
        "|",
        "=",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        ",",
        ":",
        "$",
        "\"",
        "-- a note\n",
        "\n",
        "\n\n",
        "  ",
        "    ",
        "x",
        "foo_bar",
        "Foo",
        "Option",
        "0",
        "42",
        "3.14",
        "\"text\"",
        "$\"interp ${x}\"",
        "true",
        "Ok",
        "@test",
    ];
    prop::collection::vec(proptest::sample::select(tokens), 1..48).prop_map(|parts| parts.join(" "))
}

// ── Properties ────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Idempotency over the clean corpus.
    #[test]
    fn prop_idempotent_corpus((path, src) in corpus_entry()) {
        on_big_stack(move || assert_idempotent(&path, &src));
    }

    /// Idempotency over whitespace-perturbed corpus members.
    #[test]
    fn prop_idempotent_perturbed((path, src) in perturbed_entry()) {
        on_big_stack(move || assert_idempotent(&path, &src));
    }

    /// Round-trip over the clean corpus.
    #[test]
    fn prop_round_trip_corpus((path, src) in corpus_entry()) {
        on_big_stack(move || assert_round_trip(&path, &src));
    }

    /// Round-trip over whitespace-perturbed corpus members.
    #[test]
    fn prop_round_trip_perturbed((path, src) in perturbed_entry()) {
        on_big_stack(move || assert_round_trip(&path, &src));
    }

    /// Hostile input must produce `Ok` or `Err(C101)` — never a panic.
    #[test]
    fn prop_never_panics(soup in token_soup()) {
        on_big_stack(move || {
            // A panic here fails the test; the return value itself carries no
            // property beyond "the formatter came back".
            let _ = format_source(&soup);
        });
    }
}
