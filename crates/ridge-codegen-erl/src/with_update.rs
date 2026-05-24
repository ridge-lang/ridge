//! OQ-CG004 — `with`-expression peephole detector (§4.12 support).
//!
//! T5 ships this module as a pure IR-level helper: given the field slice of a
//! `Construct { ctor: Record, .. }`, it decides whether the fields encode a
//! `with`-update pattern and, if so, returns the base-local name and the set of
//! *actually changed* fields so that `lower_construct` can emit `MapUpdate`
//! instead of a full `MapLit`.
//!
//! **Detection contract** (per OQ-CG004):
//!
//! 1. Identify a candidate base local name `B` — it is the most-frequent `name`
//!    appearing in fields whose value is `IrExpr::Field { base: Local(name=B),
//!    field == key }` (a forwarding projection where the projected field name
//!    matches the construct key).
//! 2. The peephole fires iff there is at least one forwarding field **and** at
//!    least one non-forwarding field (i.e. a genuinely updated value).
//! 3. If every field is forwarding (no-op `with`) OR no field is forwarding
//!    (fresh construction), return `None` so the caller falls back to `MapLit`.

// T5 helper, consumed by expr.rs.  Unused-code lint is irrelevant here since
// the module is pub(crate) and exercised from tests.
#![allow(dead_code)]
// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it anyway for explicitness per plan §2.2 — suppress the lint here.
#![allow(clippy::redundant_pub_crate)]

use ridge_ir::IrExpr;

/// Peephole result for a `with`-encoded `Construct` field slice.
///
/// The `base_name` is the Ridge local variable name of the source map; the
/// `updates` slice is the subset of `(key, value)` pairs that are NOT simple
/// field-forwarding projections from that base.
pub(crate) struct WithPeephole<'a> {
    /// The local variable name used as the map base (`__with_base` in Phase-5
    /// synthesised IR, but the peephole is name-agnostic).
    pub(crate) base_name: &'a str,
    /// The fields that carry new values (not just `base.field` projections).
    pub(crate) updates: Vec<(&'a str, &'a IrExpr)>,
}

/// Attempt to detect the `with`-update pattern in a Record `Construct`'s field
/// slice.
///
/// Returns `Some(WithPeephole { base_name, updates })` when the pattern fires,
/// or `None` when the caller should fall back to a full `MapLit`.
///
/// # Detection algorithm
///
/// For each `(key, value)` pair:
/// - A **forwarding** entry is one where `value` is exactly
///   `IrExpr::Field { base: IrExpr::Local { name: B }, field: same-as-key }`.
/// - We tally, for each candidate base name `B`, how many forwarding entries
///   reference it.
/// - The *winner* `B` is whichever name has the most forwarding entries (ties
///   resolved by first-seen order — stable across Phase-5 output).
/// - The peephole fires iff `winner` has ≥ 1 forwarding entry AND there is ≥ 1
///   non-forwarding entry for that winner.
pub(crate) fn detect_with_peephole(fields: &[(String, IrExpr)]) -> Option<WithPeephole<'_>> {
    if fields.is_empty() {
        return None;
    }

    // ── Step 1: for each field, determine whether it is a forwarding projection
    // and, if so, which base local it forwards from. ───────────────────────────

    // is_forwarding[i] = Some(base_name) if field[i] is a forwarding projection
    let forwarding: Vec<Option<&str>> = fields
        .iter()
        .map(|(key, value)| forwarding_base(key, value))
        .collect();

    // ── Step 2: tally forwarding counts by base name. ─────────────────────────

    // (base_name, count) in first-seen order.
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for base in forwarding.iter().flatten() {
        if let Some(entry) = counts.iter_mut().find(|(b, _)| *b == *base) {
            entry.1 += 1;
        } else {
            counts.push((base, 1));
        }
    }

    // ── Step 3: pick the winner. ─────────────────────────────────────────────

    let (winner, fwd_count) = counts.into_iter().max_by_key(|(_, c)| *c)?;

    // Guard: at least one forwarding entry for the winner.
    if fwd_count == 0 {
        return None;
    }

    // ── Step 4: collect non-forwarding entries relative to the winner. ────────

    let updates: Vec<(&str, &IrExpr)> = fields
        .iter()
        .zip(forwarding.iter())
        .filter_map(|((key, value), opt_base)| {
            // Non-forwarding: either no base, or a different base.
            match opt_base {
                Some(b) if *b == winner => None,
                _ => Some((key.as_str(), value)),
            }
        })
        .collect();

    // Guard: at least one non-forwarding entry (otherwise it's a no-op `with`).
    if updates.is_empty() {
        return None;
    }

    Some(WithPeephole {
        base_name: winner,
        updates,
    })
}

/// Return `Some(base_name)` iff `value` is a forwarding projection
/// `IrExpr::Field { base: IrExpr::Local { name }, field == key }` AND the
/// base local is one of the synthesised `__with_base_N` names produced by
/// the Phase-5 `with`-lowering.
///
/// The synthetic-name check is what keeps the peephole from misfiring on
/// user code like `Response { status = 200, body = req.body }`: there `req`
/// is a user-named local pointing at a different record type, but the
/// field/key pair matches by chance, and the unchecked detector used to
/// rewrite the whole construction as `req with { status = 200 }`, silently
/// dropping `body` and changing the result's record type.
fn forwarding_base<'a>(key: &str, value: &'a IrExpr) -> Option<&'a str> {
    if let IrExpr::Field { base, field, .. } = value {
        if field == key {
            if let IrExpr::Local { name, .. } = base.as_ref() {
                if is_with_base_local(name) {
                    return Some(name.as_str());
                }
            }
        }
    }
    None
}

/// True iff `name` is one of the `__with_base_N` synthetic locals minted by
/// `ridge-lower::with_update::lower_with_expr`.  The convention is
/// `__with_base` followed by an optional `_<digit-suffix>` (per
/// `LowerCtx::fresh_local`).
fn is_with_base_local(name: &str) -> bool {
    name.starts_with("__with_base")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::Span;
    use ridge_ir::{IrExpr, IrLit, IrNodeId};

    fn sp() -> Span {
        Span::point(0)
    }

    fn node() -> IrNodeId {
        IrNodeId(0)
    }

    fn lit_int(n: i64) -> IrExpr {
        IrExpr::Lit {
            id: node(),
            value: IrLit::Int(n),
            span: sp(),
        }
    }

    fn local(name: &str) -> IrExpr {
        IrExpr::Local {
            id: node(),
            name: name.into(),
            span: sp(),
        }
    }

    /// Build a forwarding field: value = `IrExpr::Field { base: Local(base_name), field: key }`.
    fn forwarding_field(key: &str, base_name: &str) -> (String, IrExpr) {
        (
            key.into(),
            IrExpr::Field {
                id: node(),
                base: Box::new(local(base_name)),
                field: key.into(),
                span: sp(),
            },
        )
    }

    // ── peephole_detects_typical_with_shape ──────────────────────────────────

    #[test]
    fn peephole_detects_typical_with_shape() {
        // Simulates: `r with { b = 99 }` over a record with fields a, b.
        // Field "a" is a forwarding projection from "__with_base".
        // Field "b" is a fresh value (Int 99).
        let fields = vec![
            forwarding_field("a", "__with_base"),
            ("b".into(), lit_int(99)),
        ];
        let result = detect_with_peephole(&fields);
        assert!(result.is_some(), "peephole should fire");
        let ph = result.unwrap();
        assert_eq!(ph.base_name, "__with_base");
        assert_eq!(ph.updates.len(), 1);
        assert_eq!(ph.updates[0].0, "b");
        assert!(matches!(
            ph.updates[0].1,
            IrExpr::Lit {
                value: IrLit::Int(99),
                ..
            }
        ));
    }

    // ── peephole_no_base_returns_none ────────────────────────────────────────

    #[test]
    fn peephole_no_base_returns_none() {
        // All fields are fresh values — no Field-projection forwarding at all.
        let fields = vec![("a".into(), lit_int(1)), ("b".into(), lit_int(2))];
        let result = detect_with_peephole(&fields);
        assert!(
            result.is_none(),
            "peephole should not fire when all fields are fresh"
        );
    }

    // ── peephole_all_forwarding_returns_none ─────────────────────────────────

    #[test]
    fn peephole_all_forwarding_returns_none() {
        // Every field forwards — this is a no-op `with` (degenerate case).
        // Peephole must NOT fire so the caller falls back to MapLit.
        let fields = vec![
            forwarding_field("a", "__with_base"),
            forwarding_field("b", "__with_base"),
        ];
        let result = detect_with_peephole(&fields);
        assert!(
            result.is_none(),
            "peephole should not fire for a no-op with"
        );
    }

    // ── peephole_empty_fields_returns_none ───────────────────────────────────

    #[test]
    fn peephole_empty_fields_returns_none() {
        let result = detect_with_peephole(&[]);
        assert!(result.is_none());
    }

    // ── peephole_multiple_updates ─────────────────────────────────────────────

    #[test]
    fn peephole_multiple_updates() {
        // Record with three fields: a forwards, b and c are updated.
        let fields = vec![
            forwarding_field("a", "__with_base"),
            ("b".into(), lit_int(10)),
            ("c".into(), lit_int(20)),
        ];
        let result = detect_with_peephole(&fields);
        assert!(result.is_some());
        let ph = result.unwrap();
        assert_eq!(ph.base_name, "__with_base");
        assert_eq!(ph.updates.len(), 2);
        let update_keys: Vec<&str> = ph.updates.iter().map(|(k, _)| *k).collect();
        assert!(update_keys.contains(&"b"));
        assert!(update_keys.contains(&"c"));
    }

    // ── peephole_ignores_user_named_base ──────────────────────────────────────

    /// `Response { status = 200, body = req.body }` written in user code lands
    /// here as `[("status", Lit 200), ("body", Field { base: Local("req"),
    /// field: "body" })]`.  Before the synthetic-name guard the detector
    /// happily reported a `with`-update of `req`, the caller emitted a
    /// `MapUpdate` that took `req` (a Request) as the base, and BEAM's type
    /// checker later rejected the resulting bytecode with
    /// `bad_type {needed t_map, actual any}`.  The peephole must stay clear
    /// of any base local that is not one of the synthetic `__with_base_*`
    /// names produced by Phase-5 `with`-lowering.
    #[test]
    fn peephole_ignores_user_named_base() {
        let fields = vec![
            ("status".into(), lit_int(200)),
            (
                "body".into(),
                IrExpr::Field {
                    id: node(),
                    base: Box::new(local("req")),
                    field: "body".into(),
                    span: sp(),
                },
            ),
        ];
        let result = detect_with_peephole(&fields);
        assert!(
            result.is_none(),
            "peephole must not fire when the base local is a user name, \
             only when it matches the synthetic `__with_base_N` prefix"
        );
    }

    /// Negative sibling: `__with_base_0` (the actual Phase-5 prefix) keeps
    /// firing the peephole, so genuine `with`-update lowerings still benefit
    /// from the optimisation.
    #[test]
    fn peephole_fires_for_suffixed_synthetic_base() {
        let fields = vec![
            forwarding_field("a", "__with_base_0"),
            ("b".into(), lit_int(99)),
        ];
        let result = detect_with_peephole(&fields);
        assert!(
            result.is_some(),
            "synthetic `__with_base_0` must still fire"
        );
        let ph = result.unwrap();
        assert_eq!(ph.base_name, "__with_base_0");
    }

    // ── peephole_field_key_mismatch_is_not_forwarding ─────────────────────────

    #[test]
    fn peephole_field_key_mismatch_is_not_forwarding() {
        // A field that projects a DIFFERENT field name is not forwarding.
        // e.g. ("a", Field { base: Local("__with_base"), field: "x" }) — key "a" != field "x"
        let non_forwarding_field = (
            "a".into(),
            IrExpr::Field {
                id: node(),
                base: Box::new(local("__with_base")),
                field: "x".into(), // mismatch: field "x" != key "a"
                span: sp(),
            },
        );
        let fields = vec![
            non_forwarding_field,
            forwarding_field("b", "__with_base"),
            ("c".into(), lit_int(5)),
        ];
        let result = detect_with_peephole(&fields);
        // "b" forwards, "a" and "c" are updates.
        assert!(result.is_some());
        let ph = result.unwrap();
        assert_eq!(ph.base_name, "__with_base");
        // "a" (wrong field name, not forwarding) and "c" (fresh) are both updates.
        assert_eq!(ph.updates.len(), 2);
    }
}
