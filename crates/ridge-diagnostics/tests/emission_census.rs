//! Which registered codes can a Ridge program actually produce?
//!
//! `registry_census.rs` reconciles *declaration*: every code a `code()` arm
//! returns has an entry, and no entry outlives its variant. That leaves the
//! next question unasked — a `code()` arm can exist for a variant nothing ever
//! builds, and `docs/diagnostics.md` publishes it beside the live ones with
//! nothing to tell them apart.
//!
//! So this reads the workspace for *constructions*: an occurrence of the
//! variant in value position, outside a test module. The codes with none are
//! listed below with what is known about each, and the two tests hold that list
//! in both directions — a new one cannot arrive unnoticed, and one that gets
//! wired up cannot stay on the list.
//!
//! # Reading the list is not the same as fixing it
//!
//! The entries are four different problems and want four different answers.
//! Nothing here decides which; the list exists so the count is known and does
//! not grow quietly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ridge_diagnostics::REGISTRY;

// ── The list ──────────────────────────────────────────────────────────────────

/// Every code no production path builds, and what is known about each.
///
/// Publishing a code a program cannot produce is a documentation bug at best
/// and, for the last group, a missing check. The reason strings are what a
/// reader needs before deciding: several of these are recorded nowhere else but
/// a fixture comment.
const UNEMITTABLE: &[(&str, &str)] = &[
    // Subsumed: something else fires first, so this one never gets the chance.
    // T013 says more than the T001 that arrives in its place, so it is listed
    // here rather than retired. Whether it is ever wired waits on #466: if a
    // signature is allowed to carry polymorphic recursion, the failure T013
    // reports stops existing and it is retired instead.
    ("T013", "unreachable from inferred code; reported as T001"),
    // Shadowed by a namesake. Each of these shares its variant name with a
    // variant in another crate that *is* constructed, so both read as live
    // until the two are told apart by owner. #444 holds that no code is
    // declared by two crates; nothing says the same about variant names.
    (
        "L108",
        "nothing lowers a `with` on a non-record; the name is shared with T006, which is constructed",
    ),
    (
        "P026",
        "nothing parses a refutable slice element; the name is shared with L109, which is constructed",
    ),
    // Defensive invariants: the variant exists, the check that would build it
    // does not, so the invariant it names is not enforced.
    ("L997", "no production path checks for an unsolved type in the IR"),
    ("L998", "no production path checks for a capability variable in the IR"),
    ("P999", "no production path checks the layout invariant"),
    ("T024", "no production path checks for a row variable leaking"),
    (
        "E008",
        "no production path checks for a capability token in Core Erlang (E007 next door is wired, via audit_type_error_at)",
    ),
    // No fixture, no construction, no comment: whether the check is missing or
    // the condition became impossible is not recorded anywhere.
    ("C002", "undetermined"),
    ("E205", "undetermined"),
    ("P025", "undetermined"),
    ("R019", "undetermined"),
    ("R020", "undetermined"),
    ("T028", "undetermined"),
];

/// Codes whose construction the scan must find, or the scan is broken.
///
/// Every entry is a code an ordinary program hits, several of them verified by
/// running the compiler. If the scan stops seeing these it has stopped reading
/// source correctly, and every other answer it gives is worthless — including
/// the empty diff that would otherwise look like good news.
const MUST_BE_CONSTRUCTED: &[&str] = &[
    "C001", "C005", "E001", "L001", "M001", "P001", "R001", "T001", "T009", "T029",
];

// ── Blanking ──────────────────────────────────────────────────────────────────

/// Replace comments, strings and char literals with spaces, byte for byte.
///
/// Byte-for-byte matters twice over. Offsets have to survive so a hit maps back
/// to the original text, and 213 files under `crates/*/src` hold non-ASCII —
/// collapsing a three-byte character to one space would slide every offset
/// after it.
///
/// Char literals are blanked for one reason: `'"'` occurs in the parser, and a
/// scanner that reads its quote as a string opener blanks everything up to the
/// next quote in the file. That erases `#[cfg(test)]` attributes and the braces
/// [`test_regions`] matches on, and the file then reports no test modules at
/// all. Lifetimes (`'a`) are left alone, which is why the shape is checked
/// rather than the quote counted.
fn blanked(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;

    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to.min(b.len())] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };

    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = find(b, i + 2, b"*/").map_or(b.len(), |p| p + 2);
                blank(&mut out, i, end);
                i = end;
            }
            b'r' | b'b' if raw_string_at(b, i).is_some() => {
                let end = raw_string_at(b, i).unwrap_or(b.len());
                blank(&mut out, i, end);
                i = end;
            }
            b'\'' if char_literal_len(b, i).is_some() => {
                let end = i + char_literal_len(b, i).unwrap_or(1);
                blank(&mut out, i, end);
                i = end;
            }
            b'"' => {
                let end = plain_string_end(b, i);
                blank(&mut out, i, end);
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

/// Index of `needle` in `hay` at or after `from`.
fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// End of a raw string starting at `i` (`r"…"`, `r#"…"#`, `br#"…"#`), if one is.
///
/// 113 files under `crates/*/src` contain raw strings, and one holding a quote
/// would otherwise open a string that never closes where the scanner thinks.
fn raw_string_at(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    if b.get(j) == Some(&b'b') {
        j += 1;
    }
    if b.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let hashes = b[j..].iter().take_while(|&&c| c == b'#').count();
    j += hashes;
    if b.get(j) != Some(&b'"') {
        return None;
    }
    let mut close = vec![b'"'];
    close.extend(std::iter::repeat_n(b'#', hashes));
    Some(find(b, j + 1, &close).map_or(b.len(), |p| p + close.len()))
}

/// Byte length of a char literal at `i` (`'x'`, `'\n'`, `'\u{1F}'`), if one is.
fn char_literal_len(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'\'') {
        return None;
    }
    if b.get(i + 1) == Some(&b'\\') {
        // An escape runs to the next quote; `'\u{1F600}'` is the long case.
        let close = b[i + 2..].iter().position(|&c| c == b'\'')?;
        return Some(i + 2 + close + 1 - i);
    }
    // One character, however many bytes it takes, then a closing quote.
    let width = utf8_width(*b.get(i + 1)?);
    (b.get(i + 1 + width) == Some(&b'\'')).then_some(width + 2)
}

/// Byte length of a UTF-8 sequence from its leading byte.
const fn utf8_width(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// End of the plain string opening at `i`, escapes honoured.
fn plain_string_end(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

// ── Test regions ──────────────────────────────────────────────────────────────

/// Byte spans of everything `#[cfg(test)]` gates.
///
/// The attribute is not a region marker on its own: it also sits on struct
/// fields and enum variants — `ridge-codegen-erl/src/error.rs` carries one on a
/// variant — and treating any occurrence as opening a region marks the rest of
/// the file as tests, production constructions included. A region is the item
/// the attribute is attached to, and its end comes from matching braces.
fn test_regions(b: &[u8]) -> Vec<(usize, usize)> {
    const ATTR: &[u8] = b"#[cfg(test)]";
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(start) = find(b, at, ATTR) {
        at = start + ATTR.len();
        let mut j = skip_space(b, at);
        // Further attributes may sit between the cfg and the item.
        while b.get(j) == Some(&b'#') {
            j = skip_space(b, skip_group(b, skip_space(b, j + 1)));
        }
        if let Some(open) = item_body_start(b, j) {
            spans.push((start, skip_group(b, open)));
        }
    }
    spans
}

/// Where the body of the item at `i` opens, for the item kinds an error crate
/// puts a `#[cfg(test)]` on: a module, or a function.
fn item_body_start(b: &[u8], i: usize) -> Option<usize> {
    let head = b.get(i..(i + 160).min(b.len()))?;
    let text = String::from_utf8_lossy(head);
    let rest = text
        .strip_prefix("pub ")
        .map_or(text.as_ref(), str::trim_start);
    let is_item = rest.starts_with("mod ")
        || rest.starts_with("fn ")
        || rest.starts_with("const fn ")
        || rest.starts_with("pub(crate) fn ");
    if !is_item {
        return None;
    }
    (i..(i + 400).min(b.len())).find(|&k| b[k] == b'{')
}

fn skip_space(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Index just past the group opening at `i`, or `i` if none opens there.
fn skip_group(b: &[u8], i: usize) -> usize {
    let close = match b.get(i) {
        Some(b'{') => b'}',
        Some(b'(') => b')',
        Some(b'[') => b']',
        _ => return i,
    };
    let mut stack = vec![close];
    let mut j = i + 1;
    while j < b.len() && !stack.is_empty() {
        match b[j] {
            b'{' => stack.push(b'}'),
            b'(' => stack.push(b')'),
            b'[' => stack.push(b']'),
            c if Some(&c) == stack.last() => {
                stack.pop();
            }
            _ => {}
        }
        j += 1;
    }
    j
}

// ── Pattern or value ──────────────────────────────────────────────────────────

/// Whether the variant spanning `start..end` is being matched or being built.
///
/// Told apart by what follows the variant's own argument group, because a match
/// arm is `<pattern> =>`, a guard is `<pattern> if`, and an or-pattern is
/// `<pattern> |`. Closing delimiters are stepped over first, so the pattern in
/// `Err(E::V { .. }) =>` still reads as one. What *precedes* the variant says
/// nothing: `=>` comes before an arm's body, not its pattern.
///
/// `matches!` and `let` bindings are the two shapes the forward look misses,
/// and both are checked backwards. Anything still unresolved counts as a
/// construction — a scanner that guesses "dead" turns a mistake into a failing
/// build, and this one is only ever read for what it calls dead.
fn is_construction(b: &[u8], start: usize, end: usize) -> bool {
    if in_matches_macro(b, start) || follows_a_let_binding(b, start) {
        return false;
    }
    let mut i = skip_group(b, skip_space(b, end));
    for _ in 0..8 {
        i = skip_space(b, i);
        if matches!(b.get(i), Some(b')' | b']')) {
            i += 1;
        } else {
            break;
        }
    }
    i = skip_space(b, i);
    let rest = b.get(i..(i + 3).min(b.len())).unwrap_or_default();
    !(rest.starts_with(b"=>")
        || rest.starts_with(b"|")
        || (rest.starts_with(b"if") && rest.get(2).is_some_and(u8::is_ascii_whitespace)))
}

/// Whether `i` sits inside the argument list of a `matches!` invocation.
fn in_matches_macro(b: &[u8], i: usize) -> bool {
    let from = i.saturating_sub(400);
    let mut depth = 0i32;
    let mut j = i;
    while j > from {
        j -= 1;
        match b[j] {
            b')' => depth += 1,
            b'(' if depth == 0 => return b[..j].ends_with(b"matches!"),
            b'(' => depth -= 1,
            b';' | b'{' | b'}' if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

/// Whether the statement containing `i` opens with `let` — the binding side of
/// `if let`, `while let`, or a destructuring `let`, all of them patterns.
fn follows_a_let_binding(b: &[u8], i: usize) -> bool {
    let from = i.saturating_sub(300);
    let window = String::from_utf8_lossy(&b[from..i]);
    let statement = window.rsplit([';', '{', '}']).next().unwrap_or_default();
    // `let PAT = expr` — only the part before `=` is a pattern.
    statement
        .split_once("let ")
        .is_some_and(|(_, after)| !after.contains('='))
}

// ── The scan ──────────────────────────────────────────────────────────────────

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

/// Every registry variant name, and the codes it could answer.
///
/// Codes, plural: five names are declared by two enums each — `Io` is `C012` in
/// the driver and `E203` in codegen, `WithOnNonRecord` is `L108` in lowering and
/// `T006` in typecheck. Keying one name to one code lets the second entry
/// overwrite the first, and every construction then credits whichever won,
/// leaving the other reading as dead. [`credit`] is what tells them apart.
fn variants() -> BTreeMap<&'static str, Vec<(&'static str, &'static str)>> {
    let mut out: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for entry in REGISTRY {
        for name in entry.variants {
            out.entry(name).or_default().push((entry.code, entry.owner));
        }
    }
    out
}

/// Which codes a construction found in `krate` counts towards.
///
/// A name declared once is unambiguous wherever it is built — errors do get
/// constructed outside the crate that owns them, so the crate is not a filter
/// in that case.
///
/// A name declared twice is resolved by the owning crate, which separates four
/// of the five. The fifth, `ErlcNotFound`, is `E003` and `E201` in the same
/// crate and stays ambiguous: both are credited. That can only hide a dead code,
/// never invent one — the direction to fail in, for a list whose whole job is to
/// say what is dead.
fn credit(candidates: &[(&'static str, &'static str)], krate: &str) -> Vec<&'static str> {
    if candidates.len() == 1 {
        return candidates.iter().map(|(code, _)| *code).collect();
    }
    let owned: Vec<&str> = candidates
        .iter()
        .filter(|(_, owner)| *owner == krate)
        .map(|(code, _)| *code)
        .collect();
    if owned.is_empty() {
        candidates.iter().map(|(code, _)| *code).collect()
    } else {
        owned
    }
}

/// Codes with at least one construction outside a test module.
fn constructed() -> BTreeSet<&'static str> {
    let by_variant = variants();
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    // An unreadable crates directory yields an empty set rather than a panic:
    // `the_scan_still_finds_constructions` is the test that exists to report a
    // broken scan, and it says so in terms a reader can act on.
    let mut found = BTreeSet::new();
    let Ok(dirs) = std::fs::read_dir(&crates) else {
        return found;
    };
    for dir in dirs.flatten() {
        let krate = dir.file_name().to_string_lossy().into_owned();
        let mut files = Vec::new();
        rust_sources(&dir.path().join("src"), &mut files);
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let b = blanked(&text);
            let regions = test_regions(&b);
            for (start, end) in paths_in(&b) {
                let Ok(name) = std::str::from_utf8(&b[start..end]) else {
                    continue;
                };
                let Some(candidates) = by_variant.get(name) else {
                    continue;
                };
                if regions.iter().any(|&(a, z)| a <= start && start < z) {
                    continue;
                }
                if is_construction(&b, start, end) {
                    found.extend(credit(candidates, &krate));
                }
            }
        }
    }
    found
}

/// Spans of the last segment of every `…::Ident` path.
///
/// The last segment, not a `Ident::Ident` pair: on `crate::error::E::Variant` a
/// pair match consumes `error::E`, and the next scan starts at `::Variant` with
/// no leading identifier left — so the construction is never seen, and the code
/// reads as dead.
fn paths_in(b: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(at) = find(b, i, b"::") {
        let start = skip_space(b, at + 2);
        i = at + 2;
        if !b.get(start).is_some_and(u8::is_ascii_uppercase) {
            continue;
        }
        let end = (start..b.len())
            .find(|&k| !(b[k].is_ascii_alphanumeric() || b[k] == b'_'))
            .unwrap_or(b.len());
        out.push((start, end));
        i = end;
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The scan can still read the workspace.
///
/// Without this the other two pass loudest when the scanner is most broken: a
/// scan that finds nothing calls every code unemittable, and a scan that finds
/// everything calls the list stale. This is the one that says which happened.
#[test]
fn the_scan_still_finds_constructions() {
    let built = constructed();
    let missing: Vec<&str> = MUST_BE_CONSTRUCTED
        .iter()
        .filter(|c| !built.contains(**c))
        .copied()
        .collect();

    assert!(
        missing.is_empty(),
        "the scan lost sight of codes an ordinary program hits — it stopped reading \
         source correctly, and nothing else this file reports means anything:\n  {}",
        missing.join(", ")
    );
}

/// A code that nothing can emit does not arrive quietly.
#[test]
fn no_unlisted_code_is_unemittable() {
    let built = constructed();
    let listed: BTreeSet<&str> = UNEMITTABLE.iter().map(|(c, _)| *c).collect();

    let unlisted: Vec<&str> = REGISTRY
        .iter()
        .map(|e| e.code)
        .filter(|c| !built.contains(c) && !listed.contains(c))
        .collect();

    assert!(
        unlisted.is_empty(),
        "registered, documented, and built by nothing — a reader can look these up \
         and will never see them. Wire the check up, or add it to `UNEMITTABLE` \
         with what is known about it:\n  {}",
        unlisted.join("\n  ")
    );
}

/// The list does not outlive the problem it records.
#[test]
fn the_list_names_only_codes_that_are_still_unemittable() {
    let built = constructed();
    let fixed: Vec<&str> = UNEMITTABLE
        .iter()
        .map(|(c, _)| *c)
        .filter(|c| built.contains(c))
        .collect();

    assert!(
        fixed.is_empty(),
        "listed as unemittable, but something builds them now. If a check was wired \
         up, drop the line from `UNEMITTABLE` — the count is the point of the list:\n  {}",
        fixed.join("\n  ")
    );
}

/// Every listed code is real, and says something about itself.
#[test]
fn the_list_is_well_formed() {
    let registered: BTreeSet<&str> = REGISTRY.iter().map(|e| e.code).collect();
    for (code, why) in UNEMITTABLE {
        assert!(
            registered.contains(code),
            "{code} is on the list but not in the registry"
        );
        assert!(
            !why.is_empty(),
            "{code} is listed with no reason — `undetermined` is an answer, blank is not"
        );
    }

    let mut seen = BTreeSet::new();
    for (code, _) in UNEMITTABLE {
        assert!(seen.insert(*code), "{code} is listed twice");
    }
}
