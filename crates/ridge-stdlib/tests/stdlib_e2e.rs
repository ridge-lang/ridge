//! Slow-CI lane: compile and execute the embedded stdlib, then reconcile every
//! reported test with an inventory parsed from the versioned sources.
//!
//! Gated behind `#[cfg(feature = "stdlib-e2e")]` so per-PR `cargo test --all`
//! does not pay the multi-second BEAM compile + execute cost.
//!
//! ## First run
//!
//! Build `ridge` with `cargo build -p ridge-cli --locked` before running this lane.

#![cfg(feature = "stdlib-e2e")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

use assert_cmd::Command;
use ridge_ast::{Attribute, Item, Visibility};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

// ── Thresholds ────────────────────────────────────────────────────────────────

/// Floor for the number of stdlib tests that must run and pass. A sanity check
/// that the suite actually compiled and executed; kept below the suite size so
/// legitimate churn does not trip it, but a wholesale failure to compile does.
const MIN_PASSING: u64 = 200;

// ── Harness ─────────────────────────────────────────────────────────────��─────

/// Compare the versioned source inventory with one real embedded-stdlib run.
/// Keep exit status, outcome partition and suite-size checks as separate oracles.
#[test]
fn stdlib_e2e_runs_all_tests() {
    let expected = source_inventory(&Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib"))
        .unwrap_or_else(|error| panic!("stdlib-e2e: {error}"));
    // ── 1. Compile + run the embedded stdlib's own test suite on BEAM ─────────
    // `ridge test --stdlib` unpacks the sources the compiler carries and compiles
    // them AS the standard library (permitting `@ffi`, taking the reconciled
    // types and base codec instances from source), then runs every `.test.ridge`
    // function in a fresh BEAM process. It is self-contained, so no on-disk
    // workspace fixture is needed. This exercises the whole stdlib self-compile
    // path — the modern reconciled data layer included.
    let output = Command::cargo_bin("ridge")
        .expect("ridge binary not found — run `cargo build -p ridge-cli` first")
        .arg("test")
        .arg("--stdlib")
        .output()
        .expect("failed to spawn ridge process");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert!(
        output.status.success(),
        "stdlib-e2e: ridge test exited with {}.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        output.status
    );

    // ── 4. Parse the summary line and assert gate ─────────────────────────────
    // Expected format (per crates/ridge-cli/src/cmd/test.rs summary printer):
    // "Tests: <P> passed, <F> failed, <R> rejected (<T>ms)"
    let (passed, failed, rejected) = parse_summary(&stdout).unwrap_or_else(|| {
        panic!(
            "stdlib-e2e: could not parse summary line from ridge test output.\n\
             --- stdout ---\n{stdout}\n\
             --- stderr ---\n{stderr}"
        )
    });

    assert_eq!(
        failed, 0,
        "stdlib-e2e: {failed} test(s) failed.\n\
         --- stdout ---\n{stdout}\n\
         --- stderr ---\n{stderr}"
    );

    assert_eq!(
        rejected, 0,
        "stdlib-e2e: {rejected} test(s) rejected.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );

    assert!(
        passed >= MIN_PASSING,
        "stdlib-e2e: expected >= {MIN_PASSING} passing tests, got {passed}.\n\
         --- stdout ---\n{stdout}\n\
         --- stderr ---\n{stderr}"
    );

    execution_inventory(&stdout, passed)
        .and_then(|actual| reconcile(&expected, &actual))
        .unwrap_or_else(|error| {
            panic!("stdlib-e2e: {error}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}")
        });
}

// ── Helper ────────────────────────────────────────────────────────────────────

/// Parse the `ridge test` summary line.
///
/// Requires the complete passed/failed/rejected partition and elapsed time.
fn parse_summary(output: &str) -> Option<(u64, u64, u64)> {
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Tests: ") {
            let (passed, rest) = rest.split_once(" passed, ")?;
            let (failed, rest) = rest.split_once(" failed, ")?;
            let (rejected, elapsed) = rest.split_once(" rejected (")?;
            elapsed.strip_suffix("ms)")?.parse::<u128>().ok()?;
            return Some((
                passed.parse().ok()?,
                failed.parse().ok()?,
                rejected.parse().ok()?,
            ));
        }
    }
    None
}

type Inventory = BTreeMap<String, usize>;

/// Read the source tree, not the embedded table or the runner's typed discovery:
/// a missing embedded file or discovered function must not shrink expectations.
fn source_inventory(root: &Path) -> Result<Inventory, String> {
    fn visit(dir: &Path, inventory: &mut Inventory) -> Result<(), String> {
        let entries =
            std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                visit(&path, inventory)?;
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "ridge") {
                let source = std::fs::read_to_string(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                inventory_source(&path, &source, inventory)?;
            }
        }
        Ok(())
    }
    let mut inventory = Inventory::new();
    visit(root, &mut inventory)?;
    if inventory.is_empty() {
        return Err(format!("no source tests found under {}", root.display()));
    }
    Ok(inventory)
}

fn inventory_source(path: &Path, source: &str, inventory: &mut Inventory) -> Result<(), String> {
    let parsed = ridge_parser::parse_source(source);
    if !parsed.errors.is_empty() || !parsed.lex_errors.is_empty() {
        return Err(format!(
            "{}: cannot inventory source with parse errors: {:?}; lexical errors: {:?}",
            path.display(),
            parsed.errors,
            parsed.lex_errors
        ));
    }
    let module = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("invalid source filename: {}", path.display()))?;
    for item in parsed.module.items {
        let Item::Fn(function) = item else { continue };
        // Deliberately independent of the driver's discover_test helper. An
        // attribute takes precedence, including for a public test_* function.
        let name = match function.attrs.first() {
            Some(Attribute::Test { name, .. }) => name.as_str(),
            None if function.vis == Visibility::Pub && function.name.text.starts_with("test_") => {
                function.name.text.as_str()
            }
            None => continue,
        };
        let identity = format!("{module}.{name}");
        let count = inventory.entry(identity.clone()).or_default();
        *count += 1;
        if *count != 1 {
            return Err(format!(
                "ambiguous source test identity: {identity} ({})",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Require a complete, ordered progress stream as well as a single summary.
/// Ignore ordinary notices, but never silently drop a malformed result line.
fn execution_inventory(output: &str, passed: u64) -> Result<Inventory, String> {
    let mut inventory = Inventory::new();
    let mut completed = 0_u64;
    let mut summary_seen = false;
    for line in output.lines() {
        if line.trim().starts_with("Tests:") {
            if summary_seen || parse_summary(line) != Some((passed, 0, 0)) {
                return Err(format!("inconsistent or repeated summary: {line}"));
            }
            summary_seen = true;
        } else if let Some(progress) = line.strip_prefix('[') {
            let invalid = || format!("invalid test progress: {line}");
            if summary_seen {
                return Err(format!("test result after summary: {line}"));
            }
            let (position, result) = progress.split_once(']').ok_or_else(invalid)?;
            let (ordinal, total) = position.split_once('/').ok_or_else(invalid)?;
            let ordinal = ordinal.trim().parse::<u64>().map_err(|_| invalid())?;
            let total = total.parse::<u64>().map_err(|_| invalid())?;
            let name = result
                .strip_prefix(" ok   ")
                .filter(|name| !name.is_empty())
                .ok_or_else(invalid)?;
            if ordinal != completed + 1 || total != passed || ordinal > total {
                return Err(invalid());
            }
            completed += 1;
            *inventory.entry(name.to_owned()).or_default() += 1;
        }
    }
    if !summary_seen || completed == 0 || completed != passed {
        return Err(format!(
            "incomplete execution: {completed} results for {passed} passed tests"
        ));
    }
    Ok(inventory)
}

fn reconcile(expected: &Inventory, actual: &Inventory) -> Result<(), String> {
    let mut mismatches = Vec::new();
    for (name, count) in expected {
        let observed = actual.get(name).copied().unwrap_or(0);
        if observed != *count {
            mismatches.push(format!("{name}: expected {count}, observed {observed}"));
        }
    }
    for (name, count) in actual {
        if !expected.contains_key(name) {
            mismatches.push(format!("unexpected {name}: observed {count}"));
        }
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "test inventory mismatch:\n{}",
            mismatches.join("\n")
        ))
    }
}

#[test]
fn summary_parser_preserves_rejection_counts() {
    assert_eq!(
        parse_summary("progress\nTests: 200 passed, 1 failed, 3 rejected (42ms)\n"),
        Some((200, 1, 3))
    );
}

#[test]
fn summary_parser_requires_the_complete_partition() {
    for output in [
        "Tests: 200 passed, 0 failed, 0 skipped (42ms)",
        "Tests: 200 passed, 0 failed",
        "Tests: 200 passed, 0 failed, 0 rejected (unknown)",
        "notice: no tests discovered",
    ] {
        assert_eq!(parse_summary(output), None, "accepted {output}");
    }
}

#[test]
fn source_inventory_uses_attributes_and_public_legacy_functions_once() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    std::fs::write(
        root.path().join("nested/example.test.ridge"),
        r#"
-- @test "not a declaration"
@test "private display name"
fn arbitrary () -> Result Unit Text = Ok ()
@test "attribute wins"
pub fn test_both () -> Result Unit Text = Ok ()
pub fn test_legacy () -> Bool = True
fn test_private_helper () -> Bool = True
pub fn ordinary () -> Text = "@test is only text here"
"#,
    )
    .unwrap();
    // Tests can live in ordinary .ridge files, not only companions.
    std::fs::write(
        root.path().join("other.ridge"),
        "pub fn test_other () -> Bool = True\n",
    )
    .unwrap();
    std::fs::write(root.path().join("ignored.txt"), "@test \"not source\"").unwrap();
    assert_eq!(
        source_inventory(root.path()).unwrap(),
        Inventory::from([
            ("example.test.private display name".to_owned(), 1),
            ("example.test.attribute wins".to_owned(), 1),
            ("example.test.test_legacy".to_owned(), 1),
            ("other.test_other".to_owned(), 1),
        ])
    );
}

#[test]
fn source_inventory_rejects_missing_empty_invalid_and_ambiguous_sources() {
    let root = tempfile::tempdir().unwrap();
    assert!(source_inventory(&root.path().join("missing")).is_err());
    assert!(source_inventory(root.path()).is_err());
    std::fs::write(root.path().join("bad.ridge"), "pub fn (\n").unwrap();
    assert!(source_inventory(root.path())
        .unwrap_err()
        .contains("parse errors"));
    let mut inventory = Inventory::new();
    let source = "@test \"same name\"\nfn first () -> Bool = True\n@test \"same name\"\nfn second () -> Bool = True\n";
    let error =
        inventory_source(Path::new("ambiguous.test.ridge"), source, &mut inventory).unwrap_err();
    assert!(error.contains("ambiguous source test identity: ambiguous.test.same name"));
}

fn transcript(names: &[&str]) -> String {
    let mut output = String::from("notice: test run\n");
    for (index, name) in names.iter().enumerate() {
        writeln!(output, "[{:>3}/{}] ok   {name}", index + 1, names.len()).unwrap();
    }
    writeln!(
        output,
        "Tests: {} passed, 0 failed, 0 rejected (42ms)",
        names.len()
    )
    .unwrap();
    output
}

#[test]
fn execution_inventory_matches_names_independently_of_source_order() {
    let expected = Inventory::from([
        ("module.first".to_owned(), 1),
        ("module.second".to_owned(), 1),
    ]);
    let actual = execution_inventory(&transcript(&["module.second", "module.first"]), 2).unwrap();
    assert_eq!(reconcile(&expected, &actual), Ok(()));
}

#[test]
fn reconciliation_rejects_omission_and_duplicate_substitution_above_the_floor() {
    let names: Vec<_> = (0..MIN_PASSING + 2)
        .map(|index| format!("module.case{index}"))
        .collect();
    let mut selected: Vec<_> = names.iter().map(String::as_str).collect();
    let expected = names.iter().map(|name| (name.clone(), 1)).collect();
    selected.remove(0);
    let omitted = transcript(&selected);
    let (passed, _, _) = parse_summary(&omitted).unwrap();
    assert!(passed >= MIN_PASSING);
    let error = reconcile(&expected, &execution_inventory(&omitted, passed).unwrap()).unwrap_err();
    assert!(error.contains("module.case0: expected 1, observed 0"));

    selected.push(selected[0]);
    let substituted = transcript(&selected);
    assert_eq!(parse_summary(&substituted), Some((MIN_PASSING + 2, 0, 0)));
    let error = reconcile(
        &expected,
        &execution_inventory(&substituted, MIN_PASSING + 2).unwrap(),
    )
    .unwrap_err();
    assert!(error.contains("module.case0: expected 1, observed 0"));
    assert!(error.contains("module.case1: expected 1, observed 2"));
}

#[test]
fn reconciliation_rejects_unexpected_identity_with_the_same_result_count() {
    let expected = Inventory::from([("module.expected".to_owned(), 1)]);
    let actual = execution_inventory(&transcript(&["module.unexpected"]), 1).unwrap();
    let error = reconcile(&expected, &actual).unwrap_err();
    assert!(error.contains("module.expected: expected 1, observed 0"));
    assert!(error.contains("unexpected module.unexpected: observed 1"));
}

#[test]
fn execution_inventory_rejects_incomplete_or_inconsistent_progress() {
    let summary = "Tests: 2 passed, 0 failed, 0 rejected (42ms)\n";
    for progress in [
        "",
        "[1/2] ok   module.first\n",
        "[0/2] ok   module.first\n[1/2] ok   module.second\n",
        "[1/2] ok   module.first\n[1/2] ok   module.second\n",
        "[1/2] ok   module.first\n[3/2] ok   module.second\n",
        "[1/3] ok   module.first\n[2/3] ok   module.second\n",
        "[1/2] FAIL module.first\n[2/2] ok   module.second\n",
        "[1/2] ok   \n[2/2] ok   module.second\n",
        "[1/2 ok   module.first\n[2/2] ok   module.second\n",
    ] {
        let output = format!("{progress}{summary}");
        assert!(
            execution_inventory(&output, 2).is_err(),
            "accepted {output}"
        );
    }
}

#[test]
fn execution_inventory_requires_one_complete_summary_after_results() {
    let valid = transcript(&["module.first", "module.second"]);
    let (progress, summary) = valid.split_once("Tests:").unwrap();
    for output in [
        progress.to_owned(),
        format!("{valid}Tests:{summary}"),
        format!("Tests:{summary}{progress}"),
        valid.replace("2 passed", "3 passed"),
        valid.replace("0 failed", "1 failed"),
        valid.replace("0 rejected", "1 rejected"),
        valid.replace("rejected", "skipped"),
        valid.replace("42ms", "unknown"),
        "Tests: 0 passed, 0 failed, 0 rejected (42ms)\n".to_owned(),
    ] {
        assert!(
            execution_inventory(&output, 2).is_err(),
            "accepted {output}"
        );
    }
    assert!(execution_inventory("Tests: 0 passed, 0 failed, 0 rejected (42ms)\n", 0).is_err());
}
