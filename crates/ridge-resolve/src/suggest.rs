//! "Did you mean X?" suggestion engine (plan §4.10).
//!
//! Single concern: given a target string (a name the user typed that didn't
//! resolve) and a set of candidate strings (the in-scope, visibility-filtered
//! names that *could* resolve at the same site), produce up to
//! [`MAX_RESULTS`] closest matches by **Damerau-Levenshtein distance** with
//! a hard distance cutoff of [`MAX_DISTANCE`].
//!
//! The suggestion engine **does not perform any visibility filtering** of its
//! own — that is the caller's responsibility (per plan §11 risk R14).  Passing
//! a `_private` or `pub(internal)` name to [`suggest`] would happily surface
//! it; `imports.rs` / `walker.rs` / `qualified.rs` filter their candidate sets
//! before calling.
//!
//! ## Cost guard
//!
//! Per plan §4.10 the cost of running the DP per candidate is capped at
//! [`CANDIDATE_CAP`].  When a callsite would feed more than that many
//! candidates (e.g., scanning every public symbol of a 1 000-module workspace),
//! we conservatively skip suggestion generation and return the empty `Vec` —
//! resolution still succeeds, only the "did you mean" hint is lost.
//!
//! ## Determinism
//!
//! Results are sorted by `(distance, name)` — distance first (closest wins),
//! ties broken alphabetically — so identical inputs always produce identical
//! suggestion lists across runs.  Snapshot tests rely on this.

// ── Tunables (plan §4.10) ─────────────────────────────────────────────────────

/// Maximum Damerau-Levenshtein distance for a candidate to qualify.
///
/// Distance 2 catches one transposition + one substitution, single-char
/// inserts/deletes/swaps, and small typos.  Distance 3+ produces too many
/// false-positive suggestions.
pub const MAX_DISTANCE: usize = 2;

/// Maximum number of suggestions to return.
///
/// Plan §4.10: "keep the ≤ 3 closest" — limiting noise in error renders.
pub const MAX_RESULTS: usize = 3;

/// Hard cap on the candidate set size before suggestion generation is skipped.
///
/// Plan §4.10: "Cap the computation at 500 candidates ... beyond that, skip
/// suggestions".  Keeps per-error cost O(500 × `name_len`²) worst-case.
pub const CANDIDATE_CAP: usize = 500;

/// Well-known prelude-shorthand → fully-qualified stdlib form.
///
/// Ridge intentionally keeps the prelude narrow: `not`, `and`, `or`, `print`
/// and friends do NOT live at the top level — `Bool.not`, `Bool.and`,
/// `Bool.or`, `Io.println` do.  Users coming from Python, JS, Haskell, Elixir,
/// etc. routinely type the short form; the bare-Levenshtein suggester gives
/// them junk like `Int` / `Io` / `Set` instead of the actual function they
/// meant.  This table short-circuits the most common cases so the R010 error
/// at least points at the right symbol.
///
/// Returns `None` for any name not in the table; callers should fall back to
/// the regular Levenshtein candidates.
#[must_use]
pub fn well_known_shorthand(name: &str) -> Option<&'static str> {
    match name {
        "not" => Some("Bool.not"),
        "and" => Some("Bool.and"),
        "or" => Some("Bool.or"),
        "print" | "println" => Some("Io.println"),
        _ => None,
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Compute up to [`MAX_RESULTS`] "did you mean?" suggestions for `target`.
///
/// Returns the candidates with Damerau-Levenshtein distance ≤ [`MAX_DISTANCE`],
/// sorted by `(distance, name)`.  If `candidates` yields more than
/// [`CANDIDATE_CAP`] entries the function returns an empty `Vec` (cost cap).
///
/// `candidates` is consumed lazily; the cost cap is checked while collecting.
///
/// # Visibility
///
/// The caller MUST pre-filter `candidates` to names that are visible at the
/// error site.  This function performs no filtering of its own — passing a
/// `_private` name will happily surface it.  See plan §11 risk R14.
#[must_use]
pub fn suggest<I>(target: &str, candidates: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    // Drain into a buffer so we can enforce the cost cap deterministically.
    // We take CANDIDATE_CAP + 1 so we can detect overflow on the next item.
    let mut buf: Vec<String> = Vec::new();
    for c in candidates.into_iter().take(CANDIDATE_CAP + 1) {
        buf.push(c);
        if buf.len() > CANDIDATE_CAP {
            return Vec::new();
        }
    }

    let mut scored: Vec<(usize, String)> = buf
        .into_iter()
        .filter_map(|c| {
            let d = damerau_levenshtein(target, &c);
            if d <= MAX_DISTANCE {
                Some((d, c))
            } else {
                None
            }
        })
        .collect();

    // Stable order: closest first; alphabetical break for ties.
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    scored.truncate(MAX_RESULTS);
    scored.into_iter().map(|(_, s)| s).collect()
}

/// Optimal-string-alignment Damerau-Levenshtein edit distance between two
/// strings (handles single-step adjacent transpositions).
///
/// Operates on Unicode `char`s but every name in the Ridge resolve layer is
/// ASCII in 0.1.0 (`LOWER_IDENT` / `UPPER_IDENT` productions admit only ASCII
/// letters / digits / underscore).  See spec §3.3.
#[must_use]
pub fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let la = a.len();
    let lb = b.len();

    if la == 0 {
        return lb;
    }
    if lb == 0 {
        return la;
    }

    // dp[i][j] = edit distance between a[..i] and b[..j].
    let mut dp = vec![vec![0_usize; lb + 1]; la + 1];

    for (i, row) in dp.iter_mut().enumerate().take(la + 1) {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate().take(lb + 1) {
        *cell = j;
    }

    for i in 1..=la {
        for j in 1..=lb {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1) // deletion
                .min(dp[i][j - 1] + 1) // insertion
                .min(dp[i - 1][j - 1] + cost); // substitution

            // Adjacent transposition (Damerau extension).
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                dp[i][j] = dp[i][j].min(dp[i - 2][j - 2] + cost);
            }
        }
    }

    dp[la][lb]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    // ── damerau_levenshtein basics ────────────────────────────────────────────

    #[test]
    fn dl_identical_is_zero() {
        assert_eq!(damerau_levenshtein("map", "map"), 0);
    }

    #[test]
    fn dl_single_substitution_is_one() {
        assert_eq!(damerau_levenshtein("map", "mop"), 1);
    }

    #[test]
    fn dl_single_insert_is_one() {
        assert_eq!(damerau_levenshtein("map", "mapp"), 1);
    }

    #[test]
    fn dl_single_delete_is_one() {
        assert_eq!(damerau_levenshtein("mapp", "map"), 1);
    }

    #[test]
    fn dl_transposition_is_one() {
        // "amp" vs "map" — transposition of 'm' and 'a' counts as ONE edit
        // under Damerau, vs TWO under plain Levenshtein.
        assert_eq!(damerau_levenshtein("amp", "map"), 1);
    }

    #[test]
    fn dl_empty_to_nonempty() {
        assert_eq!(damerau_levenshtein("", "abc"), 3);
        assert_eq!(damerau_levenshtein("abc", ""), 3);
    }

    // ── suggest: distance-1 hit ───────────────────────────────────────────────

    #[test]
    fn suggest_distance_one_hit() {
        // "Li.map suggests List.map but not List.empty".
        // Here we test the head-replacement piece: typo "Li" against the
        // candidate set ["List", "Map", "Set"], distance(Li, List) = 2 → hit;
        // distance(Li, Map) = 3 → reject; distance(Li, Set) = 3 → reject.
        let out = suggest("Li", s(&["List", "Map", "Set"]));
        assert_eq!(out, vec!["List".to_owned()]);
    }

    // ── suggest: distance-2 hit ───────────────────────────────────────────────

    #[test]
    fn suggest_distance_two_hit() {
        // "mapp" vs "map" = 1, vs "max" = 2, vs "empty" = 5.
        let out = suggest("mapp", s(&["map", "max", "empty"]));
        // Closest first, alphabetical tiebreak.
        assert_eq!(out, vec!["map".to_owned(), "max".to_owned()]);
    }

    // ── suggest: nothing within distance 2 ────────────────────────────────────

    #[test]
    fn suggest_no_match_beyond_distance_two() {
        // distance("xyz", "completely_different") = ≫ 2.
        let out = suggest("xyz", s(&["completely_different", "another_long_one"]));
        assert!(out.is_empty(), "expected no suggestions, got {out:?}");
    }

    // ── suggest: top-3 cap ────────────────────────────────────────────────────

    #[test]
    fn suggest_truncates_to_three_results() {
        // Five distance-1 candidates — only 3 must come back.
        let out = suggest("foo", s(&["bar", "baz", "boo", "fox", "fou", "fop"]));
        assert!(out.len() <= MAX_RESULTS);
        // All returned must be distance ≤ 2.
        for s in &out {
            assert!(damerau_levenshtein("foo", s) <= MAX_DISTANCE);
        }
    }

    // ── suggest: 500-candidate cost cap (plan §4.10 risk R14 guard) ───────────

    #[test]
    fn suggest_skips_when_candidate_set_exceeds_cap() {
        // 501 candidates — exceeds CANDIDATE_CAP (500) → return empty.
        let candidates: Vec<String> = (0..=CANDIDATE_CAP).map(|i| format!("name{i}")).collect();
        assert_eq!(candidates.len(), CANDIDATE_CAP + 1);
        let out = suggest("name0", candidates);
        assert!(
            out.is_empty(),
            "candidate-cap overflow must skip suggestions; got {out:?}"
        );
    }

    // ── suggest: at-cap is fine (CANDIDATE_CAP exactly) ──────────────────────

    #[test]
    fn suggest_runs_when_at_cap_exactly() {
        // CANDIDATE_CAP candidates is the upper bound; one of them is "exactly_typo"
        // (distance 1 from "exactly typo" target).
        let mut candidates: Vec<String> =
            (0..CANDIDATE_CAP - 1).map(|i| format!("name{i}")).collect();
        candidates.push("target_name".into());
        assert_eq!(candidates.len(), CANDIDATE_CAP);
        let out = suggest("target_namee", candidates);
        assert!(
            out.contains(&"target_name".to_owned()),
            "must produce target_name suggestion; got {out:?}"
        );
    }

    // ── suggest: ordering is (distance, alphabetical) ────────────────────────

    #[test]
    fn suggest_orders_by_distance_then_alphabetical() {
        // "abc" vs "abc" = 0, vs "abd" = 1, vs "abx" = 1.
        // Order: distance-0 "abc" first, then alphabetical: "abd", "abx".
        let out = suggest("abc", s(&["abx", "abd", "abc"]));
        assert_eq!(
            out,
            vec!["abc".to_owned(), "abd".to_owned(), "abx".to_owned()]
        );
    }

    // ── suggest: caller-side visibility filter (R14 plan mitigation) ─────────

    #[test]
    fn suggest_does_not_filter_visibility_callers_must() {
        // The engine surfaces whatever it is given.  Callers are expected to
        // pre-filter — this test locks in that contract: a `_private` name
        // passed in WILL be returned (and so callers must NOT pass it).
        // Distance("_privat", "_private") = 1 (single insertion), well within
        // the cutoff.
        let out = suggest("_privat", s(&["_private"]));
        assert_eq!(
            out,
            vec!["_private".to_owned()],
            "engine MUST NOT filter; visibility is the caller's job"
        );
    }

    // ── well_known_shorthand ──────────────────────────────────────────────────

    #[test]
    fn shorthand_not_maps_to_bool_not() {
        assert_eq!(well_known_shorthand("not"), Some("Bool.not"));
    }

    #[test]
    fn shorthand_and_or_map_to_bool() {
        assert_eq!(well_known_shorthand("and"), Some("Bool.and"));
        assert_eq!(well_known_shorthand("or"), Some("Bool.or"));
    }

    #[test]
    fn shorthand_print_println_map_to_io_println() {
        assert_eq!(well_known_shorthand("print"), Some("Io.println"));
        assert_eq!(well_known_shorthand("println"), Some("Io.println"));
    }

    #[test]
    fn shorthand_returns_none_for_unrelated_names() {
        assert_eq!(well_known_shorthand("foo"), None);
        assert_eq!(well_known_shorthand("Map"), None);
        assert_eq!(well_known_shorthand(""), None);
    }
}
