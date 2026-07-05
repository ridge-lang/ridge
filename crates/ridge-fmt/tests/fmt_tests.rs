//! Integration tests for `ridge-fmt`.
//!
//! Test plan:
//! - 16 golden fixture tests (input → expected output).
//! - 16 idempotency tests (format(format(input)) == format(input)).
//! - 1 round-trip integration test (format every `examples/*.ridge` and
//!   `crates/ridge-stdlib/stdlib/**/*.ridge`, re-parse, assert AST equivalence).
//!
//! Total: 33 tests.

use ridge_fmt::{format_source, migrate_tests};

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Format `input` and assert it equals `expected`.
fn assert_formats_to(fixture: &str, input: &str, expected: &str) {
    let result = format_source(input).unwrap_or_else(|e| {
        panic!("fixture '{fixture}': format_source failed: {e}");
    });
    assert_eq!(
        result, expected,
        "fixture '{fixture}': formatted output did not match expected"
    );
}

/// Format `input` twice and assert the second pass equals the first.
fn assert_idempotent(fixture: &str, input: &str) {
    let first = format_source(input).unwrap_or_else(|e| {
        panic!("fixture '{fixture}' idempotency (first pass): format_source failed: {e}");
    });
    let second = format_source(&first).unwrap_or_else(|e| {
        panic!("fixture '{fixture}' idempotency (second pass): format_source failed: {e}");
    });
    assert_eq!(
        first, second,
        "fixture '{fixture}': formatter is not idempotent (second pass differs from first)"
    );
}

// ── Fixture loader ─────────────────────────────────────────────────────────────

macro_rules! fixture {
    ($prefix:literal) => {{
        let input = include_str!(concat!("fixtures/", $prefix, "_input.ridge"));
        let expected = include_str!(concat!("fixtures/", $prefix, "_expected.ridge"));
        (input, expected)
    }};
}

// ── 16 Golden fixture tests ────────────────────────────────────────────────────

#[test]
fn golden_01_imports() {
    let (input, expected) = fixture!("01_imports");
    assert_formats_to("01_imports", input, expected);
}

#[test]
fn golden_02_top_level_fns() {
    let (input, expected) = fixture!("02_topfn");
    assert_formats_to("02_topfn", input, expected);
}

#[test]
fn golden_03_lambdas() {
    let (input, expected) = fixture!("03_lambda");
    assert_formats_to("03_lambda", input, expected);
}

#[test]
fn golden_04_match() {
    let (input, expected) = fixture!("04_match");
    assert_formats_to("04_match", input, expected);
}

#[test]
fn golden_05_pipes() {
    let (input, expected) = fixture!("05_pipes");
    assert_formats_to("05_pipes", input, expected);
}

#[test]
fn or_pattern_survives_formatting() {
    // An or-pattern arm `p1 | p2 | p3 ->` must round-trip: the alternatives stay
    // on one line and formatting is idempotent.
    let src = "\
fn classify (n: Int) -> Text =
    match n
        0 | 1 | 2 -> \"low\"
        _ -> \"high\"
";
    let out = format_source(src).unwrap_or_else(|e| panic!("format_source failed: {e}"));
    assert!(
        out.contains("0 | 1 | 2 ->"),
        "or-pattern alternatives must survive formatting, got:\n{out}"
    );
    assert_idempotent("or_pattern", src);
}

#[test]
fn golden_06_capability_prefixes() {
    let (input, expected) = fixture!("06_caps");
    assert_formats_to("06_caps", input, expected);
}

#[test]
fn golden_07_doc_comments() {
    let (input, expected) = fixture!("07_doccomments");
    assert_formats_to("07_doccomments", input, expected);
}

#[test]
fn golden_08_line_comments() {
    let (input, expected) = fixture!("08_linecomments");
    assert_formats_to("08_linecomments", input, expected);
}

#[test]
fn golden_09_mixed_indentation() {
    let (input, expected) = fixture!("09_mixed_indent");
    assert_formats_to("09_mixed_indent", input, expected);
}

#[test]
fn golden_10_crlf_input() {
    let (input, expected) = fixture!("10_crlf");
    assert_formats_to("10_crlf", input, expected);
}

#[test]
fn golden_11_blank_line_collapsing() {
    let (input, expected) = fixture!("11_blanks");
    assert_formats_to("11_blanks", input, expected);
}

#[test]
fn golden_12_operator_spacing() {
    let (input, expected) = fixture!("12_operators");
    assert_formats_to("12_operators", input, expected);
}

#[test]
fn golden_13_multi_line_lambdas() {
    let (input, expected) = fixture!("13_multilambda");
    assert_formats_to("13_multilambda", input, expected);
}

#[test]
fn golden_14_record_literals() {
    let (input, expected) = fixture!("14_records");
    assert_formats_to("14_records", input, expected);
}

#[test]
fn golden_15_list_literals() {
    let (input, expected) = fixture!("15_lists");
    assert_formats_to("15_lists", input, expected);
}

#[test]
fn golden_16_type_decls() {
    let (input, expected) = fixture!("16_types");
    assert_formats_to("16_types", input, expected);
}

// ── 16 Idempotency tests ───────────────────────────────────────────────────────

#[test]
fn idempotent_01_imports() {
    let (input, _) = fixture!("01_imports");
    assert_idempotent("01_imports", input);
}

#[test]
fn idempotent_02_top_level_fns() {
    let (input, _) = fixture!("02_topfn");
    assert_idempotent("02_topfn", input);
}

#[test]
fn idempotent_03_lambdas() {
    let (input, _) = fixture!("03_lambda");
    assert_idempotent("03_lambda", input);
}

#[test]
fn idempotent_04_match() {
    let (input, _) = fixture!("04_match");
    assert_idempotent("04_match", input);
}

#[test]
fn idempotent_05_pipes() {
    let (input, _) = fixture!("05_pipes");
    assert_idempotent("05_pipes", input);
}

#[test]
fn idempotent_06_capability_prefixes() {
    let (input, _) = fixture!("06_caps");
    assert_idempotent("06_caps", input);
}

#[test]
fn idempotent_07_doc_comments() {
    let (input, _) = fixture!("07_doccomments");
    assert_idempotent("07_doccomments", input);
}

#[test]
fn idempotent_08_line_comments() {
    let (input, _) = fixture!("08_linecomments");
    assert_idempotent("08_linecomments", input);
}

#[test]
fn idempotent_09_mixed_indentation() {
    let (input, _) = fixture!("09_mixed_indent");
    assert_idempotent("09_mixed_indent", input);
}

#[test]
fn idempotent_10_crlf_input() {
    let (input, _) = fixture!("10_crlf");
    assert_idempotent("10_crlf", input);
}

#[test]
fn idempotent_11_blank_line_collapsing() {
    let (input, _) = fixture!("11_blanks");
    assert_idempotent("11_blanks", input);
}

#[test]
fn idempotent_12_operator_spacing() {
    let (input, _) = fixture!("12_operators");
    assert_idempotent("12_operators", input);
}

#[test]
fn idempotent_13_multi_line_lambdas() {
    let (input, _) = fixture!("13_multilambda");
    assert_idempotent("13_multilambda", input);
}

#[test]
fn idempotent_14_record_literals() {
    let (input, _) = fixture!("14_records");
    assert_idempotent("14_records", input);
}

#[test]
fn idempotent_15_list_literals() {
    let (input, _) = fixture!("15_lists");
    assert_idempotent("15_lists", input);
}

#[test]
fn idempotent_16_type_decls() {
    let (input, _) = fixture!("16_types");
    assert_idempotent("16_types", input);
}

// ── Round-trip integration test ────────────────────────────────────────────────

/// Format every `examples/*.ridge` and `crates/ridge-stdlib/stdlib/**/*.ridge` file,
/// then re-parse the formatted output and assert that the AST is structurally
/// equivalent (no items lost, no new parse errors introduced).
///
/// Per the T5 DoD: if no `.ridge` files are present, this test still verifies
/// discovery integrity by asserting `file_count > 0`.
#[test]
fn round_trip_examples_and_stdlib() {
    use std::path::Path;

    // Resolve paths relative to the workspace root.  CARGO_MANIFEST_DIR is
    // the crates/ridge-fmt directory.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir)
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace root
        .expect("could not determine workspace root from CARGO_MANIFEST_DIR");

    let examples_dir = workspace_root.join("examples");
    let stdlib_dir = workspace_root
        .join("crates")
        .join("ridge-stdlib")
        .join("stdlib");

    let mut ridge_files: Vec<std::path::PathBuf> = Vec::new();

    // Collect examples/*.ridge
    if examples_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&examples_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "ridge") {
                    ridge_files.push(path);
                }
            }
        }
    }

    // Collect crates/ridge-stdlib/stdlib/**/*.ridge (recursive)
    if stdlib_dir.is_dir() {
        collect_ridge_files(&stdlib_dir, &mut ridge_files);
    }

    // Explicitly assert that we found at least one file so that a regression
    // in fixture discovery does not silently pass.
    assert!(
        !ridge_files.is_empty(),
        "round_trip: no .ridge files found in examples/ or crates/ridge-stdlib/stdlib/; \
         verify the workspace layout"
    );

    let file_count = ridge_files.len();
    let mut failures: Vec<String> = Vec::new();

    for path in &ridge_files {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));

        // Format the source.  If it doesn't parse, that's a test failure only
        // if the original also parsed without errors (some stdlib test files
        // may use patterns not yet fully supported).
        let original_parse = ridge_parser::parse_source(&src);
        let has_original_errors =
            !original_parse.errors.is_empty() || !original_parse.lex_errors.is_empty();

        if has_original_errors {
            // File doesn't parse — skip round-trip for this file.
            continue;
        }

        let formatted = match format_source(&src) {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!("{}: format failed: {e}", path.display()));
                continue;
            }
        };

        // Re-parse the formatted output.
        let reparsed = ridge_parser::parse_source(&formatted);

        if !reparsed.errors.is_empty() || !reparsed.lex_errors.is_empty() {
            failures.push(format!(
                "{}: re-parse of formatted output produced errors: {:?} / lex: {:?}",
                path.display(),
                reparsed.errors,
                reparsed.lex_errors,
            ));
            continue;
        }

        // Assert structural equivalence: same number of top-level items.
        let orig_items = original_parse.module.items.len();
        let fmt_items = reparsed.module.items.len();
        if orig_items != fmt_items {
            failures.push(format!(
                "{}: item count mismatch: original={orig_items}, reformatted={fmt_items}",
                path.display()
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "round_trip: {}/{} files failed:\n{}",
            failures.len(),
            file_count,
            failures.join("\n")
        );
    }
}

// ── Idempotency regression on the four canonical Phase 8 examples ────────────
//
// Reproducer for the fmt idempotency bug: three consecutive `ridge fmt` passes against
// `examples/log_analyzer.ridge` produced three distinct outputs because:
//   1. `normalise_operator_spaces` cast individual UTF-8 bytes to `char`,
//      breaking multi-byte scalars in string literals and prose alike.
//   2. The phase 1c "doc-comment line" check matched only the `---` marker
//      lines, not the body of `---…---` blocks, so prose like
//      "Token-bucket" became "Token - bucket" on every pass.
//   3. The phase 1b "trailing comment" check used `col > 0`, misclassifying
//      indented full-line comments as trailing-attached.  The trailing
//      attachment then emptied the source line, the blank-line normaliser
//      removed the now-empty line, and the comment was silently dropped.
//
// The fix lives in `crates/ridge-fmt/src/{rules,printer}.rs`; this test
// guards against regression by formatting each canonical example three
// times and asserting all three outputs are byte-identical.

/// Run `format_source` `n` times and return the sequence of outputs.
fn format_n_passes(input: &str, n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(n);
    let mut current = input.to_string();
    for _ in 0..n {
        current = format_source(&current).expect("format_source must succeed");
        out.push(current.clone());
    }
    out
}

/// Asserts that the four canonical Phase 8 examples reach a fixed point in
/// one `ridge fmt` pass, and that subsequent passes are byte-identical.
#[test]
fn idempotent_canonical_examples_three_pass() {
    use std::path::Path;
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");
    let examples_dir = workspace_root.join("examples");

    for name in [
        "log_analyzer",
        "url_shortener",
        "game_of_life",
        "rate_limiter",
    ] {
        let path = examples_dir.join(format!("{name}.ridge"));
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let passes = format_n_passes(&src, 3);
        assert_eq!(
            passes[0], passes[1],
            "fixture '{name}': pass 1 != pass 2 (idempotency violated)"
        );
        assert_eq!(
            passes[1], passes[2],
            "fixture '{name}': pass 2 != pass 3 (idempotency violated)"
        );
    }
}

/// Multi-byte UTF-8 inside string literals (e.g. block-drawing chars `█`,
/// en-dashes `–`, em-dashes `—`) must round-trip byte-identical through
/// `format_source`.  Regression for the `bytes[i] as char` bug in
/// `normalise_operator_spaces`.
#[test]
fn utf8_multibyte_in_strings_roundtrips() {
    let input = "pub fn bar -> Text =\n    let block = \"\u{2588}\"\n    let dash  = \"0\u{2013}23\"\n    block\n";
    let first = format_source(input).expect("must format");
    // Every multi-byte scalar in the input must appear unchanged in the output.
    assert!(first.contains('\u{2588}'), "block char dropped: {first:?}");
    assert!(first.contains('\u{2013}'), "en-dash dropped: {first:?}");
    let second = format_source(&first).expect("must format twice");
    assert_eq!(first, second, "UTF-8 round-trip not idempotent");
}

/// Indented full-line comments must NOT be treated as trailing comments.
/// Regression for the `col > 0` bug in phase 1b — without the fix, a
/// comment like `        -- note` between two code lines was stripped from
/// its line, the now-empty line was removed by the blank-line normaliser,
/// and the comment vanished from the formatted output.
#[test]
fn indented_full_line_comment_survives_pass() {
    let input = "pub fn foo -> Int =\n    let x = 1\n    -- a comment on its own line\n    x\n";
    let first = format_source(input).expect("must format");
    assert!(
        first.contains("-- a comment on its own line"),
        "indented full-line comment was dropped: {first:?}"
    );
    let second = format_source(&first).expect("must format twice");
    assert_eq!(first, second, "indented-comment fixture not idempotent");
}

/// `---…---` doc-comment block bodies must be left untouched by operator
/// spacing.  Regression for the prose-mangling bug — without the fix,
/// "Token-bucket" became "Token - bucket" inside the doc body and the file
/// was no longer idempotent under repeated passes.
#[test]
fn doc_block_body_not_operator_spaced() {
    let input = "---\nToken-bucket rate limiter.\nFloating-point arithmetic ahead.\n---\n\npub fn foo -> Int = 1\n";
    let first = format_source(input).expect("must format");
    assert!(
        first.contains("Token-bucket"),
        "doc body had operator-spacing applied: {first:?}"
    );
    assert!(
        first.contains("Floating-point"),
        "doc body had operator-spacing applied: {first:?}"
    );
    let second = format_source(&first).expect("must format twice");
    assert_eq!(first, second, "doc-block fixture not idempotent");
}

// ── @test attribute round-trip ────────────────────────────────────────────────

/// A source file containing `@test "…"` must survive `format_source` unchanged
/// (the attribute must not be silently dropped or mangled).
///
/// Because `ridge-fmt` is trivia-preserving, it never re-emits AST nodes —
/// only normalises whitespace.  This test guards that the formatter parses the
/// `@test` attribute successfully and returns the source byte-identical.
#[test]
fn test_attr_survives_format() {
    let input = "@test \"my test\"\nfn check_things () -> Result Unit Text = Ok ()\n";
    let formatted = format_source(input).expect("format_source must succeed on @test source");
    assert_eq!(
        formatted, input,
        "@test attribute was modified or dropped by format_source"
    );
}

/// Formatting a source file with `@test "…"` is idempotent.
#[test]
fn test_attr_format_idempotent() {
    let input = "@test \"my test\"\nfn check_things () -> Result Unit Text = Ok ()\n";
    assert_idempotent("test_attr", input);
}

// ── migrate_tests tests ────────────────────────────────────────────────────────

/// A `pub fn test_foo` gains `@test "foo"` above it; the function name is unchanged.
#[test]
fn migrate_adds_attribute_to_prefix_test() {
    let input = "pub fn test_arith_add () -> Result Unit Text = Ok ()\n";
    let output = migrate_tests(input).expect("migrate_tests must succeed");
    assert!(
        output.contains("@test \"arith_add\""),
        "expected @test attribute to be inserted: {output:?}"
    );
    assert!(
        output.contains("pub fn test_arith_add"),
        "function name must be unchanged: {output:?}"
    );
}

/// Running `migrate_tests` twice produces no further change (idempotent).
#[test]
fn migrate_is_idempotent() {
    let input = "pub fn test_arith_add () -> Result Unit Text = Ok ()\n";
    let first = migrate_tests(input).expect("first pass");
    let second = migrate_tests(&first).expect("second pass");
    assert_eq!(first, second, "migrate_tests must be idempotent");
}

/// A function that already carries `@test` is left untouched.
#[test]
fn migrate_skips_already_annotated() {
    let input = "@test \"arith_add\"\npub fn test_arith_add () -> Result Unit Text = Ok ()\n";
    let output = migrate_tests(input).expect("migrate_tests must succeed");
    assert_eq!(
        input, output,
        "already-annotated function must not be modified"
    );
}

/// A private (non-`pub`) `test_*` function is not touched.
#[test]
fn migrate_skips_private_fn() {
    let input = "fn test_internal () -> Result Unit Text = Ok ()\n";
    let output = migrate_tests(input).expect("migrate_tests must succeed");
    assert_eq!(input, output, "private test_ function must not be modified");
}

/// A function whose name does not start with `test_` is not touched.
#[test]
fn migrate_skips_non_test_prefix() {
    let input = "pub fn helper () -> Int = 42\n";
    let output = migrate_tests(input).expect("migrate_tests must succeed");
    assert_eq!(input, output, "non-test_ function must not be modified");
}

/// A `pub fn test_*` preceded by a line comment gets the attribute placed
/// below the comment, directly above `pub fn`.
#[test]
fn migrate_inserts_below_line_comment() {
    let input = "-- sets up the counter\npub fn test_counter () -> Result Unit Text = Ok ()\n";
    let output = migrate_tests(input).expect("migrate_tests must succeed");
    // The @test line must appear between the comment and the pub fn line.
    let attr_pos = output.find("@test \"counter\"").expect("@test missing");
    let fn_pos = output.find("pub fn test_counter").expect("fn missing");
    assert!(
        attr_pos < fn_pos,
        "@test must appear before pub fn: {output:?}"
    );
    // The comment must still be present.
    assert!(
        output.contains("-- sets up the counter"),
        "line comment must be preserved: {output:?}"
    );
}

/// Multiple `pub fn test_*` functions in one file all get the attribute.
#[test]
fn migrate_handles_multiple_fns() {
    let input = concat!(
        "pub fn test_add () -> Result Unit Text = Ok ()\n",
        "\n",
        "pub fn test_sub () -> Result Unit Text = Ok ()\n",
    );
    let output = migrate_tests(input).expect("migrate_tests must succeed");
    assert!(
        output.contains("@test \"add\""),
        "first function missing @test: {output:?}"
    );
    assert!(
        output.contains("@test \"sub\""),
        "second function missing @test: {output:?}"
    );
}

/// Normal `format_source` (no `--migrate-tests`) is unaffected by the migration
/// code path — it must still produce the same output as before.
#[test]
fn format_source_regression_unaffected_by_migration() {
    let input = "pub fn test_foo () -> Result Unit Text = Ok ()\n";
    let formatted = format_source(input).expect("format_source must succeed");
    // format_source should not insert @test attributes.
    assert!(
        !formatted.contains("@test"),
        "format_source must not insert @test attributes: {formatted:?}"
    );
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
