//! Reconcile [`ridge_diagnostics::REGISTRY`] against the codes the workspace
//! actually declares.
//!
//! The registry cannot check itself: nothing in it knows whether `T001` is
//! still returned by a `code()` somewhere, or whether some crate has started
//! returning `T099` that nobody wrote a line about. So this reads the source.
//!
//! Scanning text is a blunt instrument, and it is used here on purpose. The
//! alternative — every crate registering its own codes at link time — would
//! mean `ridge-diagnostics` depending on all ten of them, which is the
//! dependency cycle that put the error adapters in `ridge-driver` to begin
//! with.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ridge_diagnostics::{REGISTRY, RETIRED};

/// Every `Self::Variant { .. } => "X001"` arm, or-patterns included.
///
/// Deliberately not a general search for `"[A-Z]\d{3}"`: a message that
/// mentions a code, a language-server quick-fix tagged with the diagnostic it
/// repairs, and a test fixture all match that, and none of them declares
/// anything. Counting them is how a census reports its own fixtures as
/// findings.
///
/// Three shapes have to be told apart, and all three open with `Self::`:
///
/// ```text
/// Self::A { .. } => "T021",                    // declares
/// Self::A { .. } | Self::B { .. } => "T021",   // declares, twice
/// Self::Workspace(e) => e.code(),              // forwards, declares nothing
/// ```
///
/// An or-pattern also wraps across lines, so pending variants accumulate until
/// an arrow resolves them — and a forwarding arrow discards them, or the
/// variants above it get credited with the next code down the match.
fn declaring_arms(src: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut pending: Vec<String> = Vec::new();

    for line in src.lines() {
        let trimmed = line.trim_start();
        if !(trimmed.starts_with("Self::") || trimmed.starts_with('|')) {
            pending.clear();
            continue;
        }
        pending.extend(variants_on(trimmed));
        match code_literal_after_arrow(trimmed) {
            Some(code) => out.push((code, std::mem::take(&mut pending))),
            None if trimmed.contains("=>") => pending.clear(),
            None => {}
        }
    }
    out
}

fn variants_on(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("Self::") {
        rest = &rest[at + "Self::".len()..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if end > 0 {
            out.push(rest[..end].to_owned());
        }
        rest = &rest[end..];
    }
    out
}

fn code_literal_after_arrow(line: &str) -> Option<String> {
    let after = line.split_once("=>")?.1.trim();
    let (code, _) = after.strip_prefix('"')?.split_once('"')?;
    is_code(code).then(|| code.to_owned())
}

fn is_code(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && s.len() == 4
        && chars.all(|c| c.is_ascii_digit())
}

fn rust_sources(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// What one code is declared by: the crates that claim it, and the variants.
#[derive(Default)]
struct Claim {
    owners: Vec<String>,
    variants: Vec<String>,
}

/// Every code the workspace declares, and who declares it.
fn declared() -> BTreeMap<String, Claim> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    // An unreadable crates directory yields an empty map rather than a panic
    // here: `the_scan_still_sees_the_workspace` is the one test that exists to
    // report a broken scan, and it says so in terms a reader can act on. A
    // panic in a shared helper would fail all five with the same message.
    let mut found: BTreeMap<String, Claim> = BTreeMap::new();
    let Ok(dirs) = std::fs::read_dir(&crates) else {
        return found;
    };
    for dir in dirs.flatten() {
        let krate = dir.file_name().to_string_lossy().into_owned();
        let mut files = Vec::new();
        rust_sources(&dir.path().join("src"), &mut files);
        for file in files {
            let Ok(src) = std::fs::read_to_string(&file) else {
                continue;
            };
            for (code, variants) in declaring_arms(&src) {
                let claim = found.entry(code).or_default();
                if !claim.owners.contains(&krate) {
                    claim.owners.push(krate.clone());
                }
                for v in variants {
                    if !claim.variants.contains(&v) {
                        claim.variants.push(v);
                    }
                }
            }
        }
    }
    found
}

/// The census found nothing, which means it stopped reading the workspace
/// rather than that the workspace stopped declaring codes.
#[test]
fn the_scan_still_sees_the_workspace() {
    let found = declared();
    assert!(
        found.len() > 200,
        "only {} codes found by scanning source — the scan broke, not the compiler",
        found.len()
    );
}

#[test]
fn every_declared_code_is_in_the_registry() {
    let registered: BTreeMap<_, _> = REGISTRY.iter().map(|e| (e.code, e)).collect();
    let missing: Vec<String> = declared()
        .iter()
        .filter(|(code, _)| !registered.contains_key(code.as_str()))
        .map(|(code, claim)| {
            format!(
                "{code} ({}::{})",
                claim.owners.join("+"),
                claim.variants.join(" | ")
            )
        })
        .collect();

    assert!(
        missing.is_empty(),
        "declared but unregistered — add a line to `registry.rs` saying what each means:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn every_registered_code_is_still_declared() {
    let found = declared();
    let stale: Vec<&str> = REGISTRY
        .iter()
        .filter(|e| !found.contains_key(e.code))
        .map(|e| e.code)
        .collect();

    assert!(
        stale.is_empty(),
        "registered but no longer declared by any `code()` — the variant went away, \
         so the entry should too:\n  {}",
        stale.join("\n  ")
    );
}

/// A retired code is declared nowhere.
///
/// The mirror of the test above. That one catches an entry outliving its
/// variant; this one catches a variant outliving its retirement — a `code()`
/// arm returning a number the retired table says nothing emits. Between them
/// each table has one rule: a registered code must be declared somewhere, a
/// retired one nowhere.
#[test]
fn no_retired_code_is_still_declared() {
    let found = declared();
    let alive: Vec<&str> = RETIRED
        .iter()
        .filter(|r| found.contains_key(r.code))
        .map(|r| r.code)
        .collect();

    assert!(
        alive.is_empty(),
        "retired, yet still returned by a `code()` — either the arm goes or the \
         entry moves back to the registry:\n  {}",
        alive.join("\n  ")
    );
}

#[test]
fn the_registry_names_the_variants_and_crate_that_declare_each_code() {
    let found = declared();
    let wrong: Vec<String> = REGISTRY
        .iter()
        .filter_map(|e| {
            let claim = found.get(e.code)?;
            let same_owner = claim.owners == [e.owner];
            let same_variants = claim.variants == e.variants;
            (!same_owner || !same_variants).then(|| {
                format!(
                    "{}: registry says {}::{}, source says {}::{}",
                    e.code,
                    e.owner,
                    e.variants.join(" | "),
                    claim.owners.join("+"),
                    claim.variants.join(" | ")
                )
            })
        })
        .collect();

    assert!(wrong.is_empty(), "\n  {}", wrong.join("\n  "));
}

/// The fix that #410 landed, held in place.
///
/// Two crates declaring one code is how `T001` came to mean both a type
/// mismatch and an FFI arity mismatch, and how a lookup by code silently kept
/// whichever definition was read last.
#[test]
fn no_code_is_declared_by_two_crates() {
    let shared: Vec<String> = declared()
        .iter()
        .filter(|(_, claim)| claim.owners.len() > 1)
        .map(|(code, claim)| format!("{code}: {}", claim.owners.join(" | ")))
        .collect();

    assert!(
        shared.is_empty(),
        "one code, more than one meaning:\n  {}",
        shared.join("\n  ")
    );
}
