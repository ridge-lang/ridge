//! Literal values in the IR.
// OQ-IR003: IrLit is #[non_exhaustive] — see expr.rs for rationale.

/// A literal value in the Ridge Core IR.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum IrLit {
    /// An integer literal.
    Int(i64),
    /// A floating-point literal.
    Float(f64),
    /// A boolean literal.
    Bool(bool),
    /// A text (string) literal.
    Text(String),
    /// The unit value `()`.
    Unit,
    /// `[]` and `[x, y, z]` are `IrExpr::ListLit` (not literals); this variant
    /// exists for empty-list-as-pattern (`[]`).
    EmptyList,
}
