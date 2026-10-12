//! Reachability of an expression's continuation, independent of its value type.

/// Outcomes needed when composing expressions in the current return boundary.
/// Explicit returns stop the continuation; propagation can also exit a `try`.
#[derive(Clone, Copy, Debug)]
pub struct Flow {
    /// At least one path produces a value for the following expression.
    pub(crate) falls_through: bool,
    /// At least one path leaves through `?`, caught by an enclosing `try`.
    propagates: bool,
}

impl Default for Flow {
    fn default() -> Self {
        Self::NEXT
    }
}

impl Flow {
    pub(crate) const NEXT: Self = Self {
        falls_through: true,
        propagates: false,
    };
    pub(crate) const RETURN: Self = Self {
        falls_through: false,
        propagates: false,
    };
    pub(crate) const PROPAGATE: Self = Self {
        falls_through: true,
        propagates: true,
    };

    /// Evaluate the right region only along paths that reach it.
    pub(crate) const fn then(self, next: Self) -> Self {
        Self {
            falls_through: self.falls_through && next.falls_through,
            propagates: self.propagates || (self.falls_through && next.propagates),
        }
    }

    /// Either branch may be selected.
    pub(crate) const fn either(self, other: Self) -> Self {
        Self {
            falls_through: self.falls_through || other.falls_through,
            propagates: self.propagates || other.propagates,
        }
    }

    /// A propagation failure becomes a result of this `try`, not a return.
    pub(crate) const fn capture(self) -> Self {
        Self {
            falls_through: self.falls_through || self.propagates,
            propagates: false,
        }
    }
}
