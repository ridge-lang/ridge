//! Exhaustiveness witness types: [`MatchWitness`], [`WitnessPat`], [`WitnessKind`].
//!
//! These are the canonical definitions moved here from `ridge-typecheck::lib.rs`
//! (T1 scaffold). `ridge-typecheck::lib.rs` will re-export from this module.

// ── MatchWitness ──────────────────────────────────────────────────────────────

/// A witness for a missing or redundant match arm, produced by Maranget's
/// exhaustiveness algorithm (T12).
///
/// The `example` field contains a human-readable counter-example pattern;
/// `kind` distinguishes a coverage gap from a redundant arm.
#[derive(Debug, Clone)]
pub struct MatchWitness {
    /// A human-readable example pattern that is missing or redundant.
    pub example: WitnessPat,
    /// Whether this witness represents a missing or a redundant arm.
    pub kind: WitnessKind,
}

// ── WitnessKind ───────────────────────────────────────────────────────────────

/// Categorises a [`MatchWitness`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessKind {
    /// A constructor / value not covered by any arm.
    Missing,
    /// An arm that is fully subsumed by earlier arms.
    Redundant,
}

// ── WitnessPat ────────────────────────────────────────────────────────────────

/// A pattern example used in exhaustiveness witnesses.
///
/// Rendered by `ridge-typecheck::render` (T16) as a human-readable string;
/// for now each variant carries the minimal structural information needed for
/// the OQ-T009 capped witness set (`MAX_WITNESSES = 3`).
#[derive(Debug, Clone)]
pub enum WitnessPat {
    /// A wildcard `_`.
    Wild,
    /// A literal value (rendered as a string — e.g. `"0"`, `"true"`).
    Lit(String),
    /// A constructor applied to sub-patterns.
    Ctor {
        /// Constructor name, e.g. `"Some"`, `"Circle"`.
        name: String,
        /// Sub-pattern arguments.
        args: Vec<Self>,
    },
    /// A tuple pattern.
    Tuple(Vec<Self>),
    /// A record construction pattern.
    Record {
        /// Constructor / type name.
        ctor: String,
        /// Field patterns in declaration order.
        fields: Vec<(String, Self)>,
    },
}
