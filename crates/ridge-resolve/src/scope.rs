//! Lexical scope stack for intra-module name resolution (T8, plan §3.3 / §4.5).
//!
//! [`ScopeStack`] is a `Vec<Scope>` maintained by the [`crate::walker`] as it
//! descends into function bodies, lambda expressions, match arms, actor blocks,
//! etc.  Lookup walks from the innermost scope outward; the first hit wins.
//!
//! # Shadowing rules (plan §4.8)
//!
//! - Cross-scope shadowing is **permitted**. An inner `let x` hides an outer
//!   `x` without error.
//! - Same-scope duplicate bindings are **R011 `DuplicateLocal`**.  The caller
//!   is responsible for emitting that error when [`ScopeStack::add_local`]
//!   returns `Err`.

use rustc_hash::FxHashMap;

use ridge_lexer::Span;

// ── ScopeKind ─────────────────────────────────────────────────────────────────

/// The kind of lexical scope that was pushed.
///
/// Used for diagnostic context (e.g. distinguishing actor-body state fields
/// from function parameters) and for the R017 state-shadow check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// The implicit module-level scope (one per module, always at the bottom).
    Module,
    /// A `fn` body (top-level or inner-fn).
    FnBody,
    /// A lambda body `fn params -> body`.
    Lambda,
    /// A single `match` arm (pattern + guard + body).
    MatchArm,
    /// A curly-brace block or indented block sequence.
    Block,
    /// The body of an `init` block inside an actor.
    InitBlock,
    /// The body of an `on` message handler inside an actor.
    OnHandler,
    /// The implicit scope that wraps all members of an `actor` declaration.
    ActorBody,
    /// A `try` expression body.
    TryBlock,
    /// The `else` block of a `guard` expression.
    GuardElse,
}

// ── LocalKind ─────────────────────────────────────────────────────────────────

/// How a local binding was introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// `let x = …` — immutable binding.
    LetImmutable,
    /// `var x = …` — mutable binding.
    VarMutable,
    /// A parameter of a top-level or inner `fn` declaration.
    FnParam,
    /// A parameter of a lambda expression.
    LambdaParam,
    /// A variable bound by a pattern (match arm, let-destructuring).
    PatternBinding,
    /// A state field declared inside an actor body.
    StateField,
    /// A parameter of an `on` message handler.
    HandlerParam,
    /// A parameter of an `init` block.
    InitParam,
    /// An alias pattern `name @`.
    AsAlias,
}

// ── LocalId ───────────────────────────────────────────────────────────────────

/// Uniquely identifies a local within the scope stack during resolution.
///
/// Monotonically increasing across the entire module walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

// ── LocalEntry ────────────────────────────────────────────────────────────────

/// A single local binding recorded in a scope.
#[derive(Debug, Clone)]
pub struct LocalEntry {
    /// Stable identifier for this binding.
    pub id: LocalId,
    /// The source name of the binding.
    pub name: String,
    /// Source span of the definition site.
    pub def_span: Span,
    /// How this local was introduced.
    pub kind: LocalKind,
}

// ── Scope ─────────────────────────────────────────────────────────────────────

/// A single lexical scope frame.
#[derive(Debug)]
pub struct Scope {
    /// The syntactic context that introduced this scope.
    pub kind: ScopeKind,
    /// Locals in insertion order (used for R011 first-span).
    pub locals: Vec<LocalEntry>,
    /// Fast name → `LocalId` index.  Maps to the *first* binding of this name
    /// in this scope; a second binding at the same name produces R011.
    pub index: FxHashMap<String, LocalId>,
}

impl Scope {
    fn new(kind: ScopeKind) -> Self {
        Self {
            kind,
            locals: Vec::new(),
            index: FxHashMap::default(),
        }
    }
}

// ── ScopeStack ────────────────────────────────────────────────────────────────

/// The lexical scope chain maintained during the intra-function walk.
///
/// Implemented as a flat `Vec<Scope>` indexed by depth.  The outermost scope
/// is at index 0; the innermost (current) scope is at `stack.last()`.  Lookup
/// walks from the top (innermost) to the bottom (outermost).
#[derive(Debug, Default)]
pub struct ScopeStack {
    /// All active scopes, innermost last.
    pub stack: Vec<Scope>,
    /// Monotone counter used to allocate fresh [`LocalId`]s.
    pub local_counter: u32,
}

impl ScopeStack {
    /// Push a new empty scope of the given kind.
    pub fn push(&mut self, kind: ScopeKind) {
        self.stack.push(Scope::new(kind));
    }

    /// Pop the innermost scope frame.
    ///
    /// Panics in debug builds if the stack is empty (indicates a walker bug).
    pub fn pop(&mut self) {
        debug_assert!(!self.stack.is_empty(), "ScopeStack::pop on empty stack");
        self.stack.pop();
    }

    /// Add a local to the **current** (innermost) scope.
    ///
    /// # Returns
    ///
    /// - `Ok(LocalId)` — the binding was added successfully.
    /// - `Err((existing_id, existing_span))` — a binding with this name
    ///   already exists **in the same scope**.  The caller must emit
    ///   `R011 DuplicateLocal`; the *existing* entry is not modified.
    pub fn add_local(
        &mut self,
        name: String,
        span: Span,
        kind: LocalKind,
    ) -> Result<LocalId, (LocalId, Span)> {
        if self.stack.is_empty() {
            // Defensive: no scope frame open.  Allocate a synthetic module scope.
            self.stack.push(Scope::new(ScopeKind::Module));
        }
        // SAFETY: we just ensured the stack is non-empty.
        let Some(scope) = self.stack.last_mut() else {
            // Unreachable: guarded above.
            return Ok(LocalId(0));
        };

        if let Some(&existing_id) = scope.index.get(&name) {
            // R011: same-scope duplicate.
            let existing_span = scope
                .locals
                .iter()
                .find(|e| e.id == existing_id)
                .map_or(Span::point(0), |e| e.def_span);
            return Err((existing_id, existing_span));
        }

        let id = LocalId(self.local_counter);
        self.local_counter += 1;
        scope.locals.push(LocalEntry {
            id,
            name: name.clone(),
            def_span: span,
            kind,
        });
        scope.index.insert(name, id);
        Ok(id)
    }

    /// Look up a name by walking the scope chain from innermost to outermost.
    ///
    /// Returns the first [`LocalEntry`] whose `name` matches, or `None` if the
    /// name is not in scope.
    #[must_use]
    pub fn lookup_local(&self, name: &str) -> Option<&LocalEntry> {
        for scope in self.stack.iter().rev() {
            if let Some(&id) = scope.index.get(name) {
                if let Some(entry) = scope.locals.iter().find(|e| e.id == id) {
                    return Some(entry);
                }
            }
        }
        None
    }

    /// Find the nearest enclosing scope of the given kind.
    ///
    /// Used by the walker to detect whether we are inside an actor body (for
    /// R017 state-field shadowing checks).
    #[must_use]
    pub fn enclosing_kind(&self, kind: ScopeKind) -> Option<&Scope> {
        self.stack.iter().rev().find(|s| s.kind == kind)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(byte: u32) -> Span {
        Span::new(byte, byte + 1)
    }

    // Test 1: empty stack → lookup returns None.
    #[test]
    fn empty_stack_lookup_none() {
        let stack = ScopeStack::default();
        assert!(stack.lookup_local("x").is_none());
    }

    // Test 2: push module scope, add "x", lookup returns Some.
    #[test]
    fn push_add_lookup() {
        let mut stack = ScopeStack::default();
        stack.push(ScopeKind::Module);
        let id = stack
            .add_local("x".into(), sp(), LocalKind::LetImmutable)
            .unwrap();
        let entry = stack.lookup_local("x").expect("x must be found");
        assert_eq!(entry.id, id);
        assert_eq!(entry.name, "x");
    }

    // Test 3: inner scope shadows outer — lookup returns inner entry.
    #[test]
    fn inner_scope_shadows_outer() {
        let mut stack = ScopeStack::default();
        stack.push(ScopeKind::FnBody);
        let outer_id = stack
            .add_local("x".into(), sp_at(0), LocalKind::FnParam)
            .unwrap();

        stack.push(ScopeKind::Block);
        let inner_id = stack
            .add_local("x".into(), sp_at(5), LocalKind::LetImmutable)
            .unwrap();

        let found = stack.lookup_local("x").expect("found");
        // Inner (most recently pushed scope) wins.
        assert_eq!(found.id, inner_id);
        assert_ne!(found.id, outer_id);
    }

    // Test 4: after pop, outer "x" is visible again.
    #[test]
    fn after_pop_outer_visible() {
        let mut stack = ScopeStack::default();
        stack.push(ScopeKind::FnBody);
        let outer_id = stack
            .add_local("x".into(), sp_at(0), LocalKind::FnParam)
            .unwrap();

        stack.push(ScopeKind::Block);
        let _inner_id = stack
            .add_local("x".into(), sp_at(5), LocalKind::LetImmutable)
            .unwrap();
        stack.pop();

        let found = stack.lookup_local("x").expect("found");
        assert_eq!(found.id, outer_id);
    }

    // Test 5: same-scope duplicate → Err (R011 data).
    #[test]
    fn same_scope_duplicate_returns_err() {
        let mut stack = ScopeStack::default();
        stack.push(ScopeKind::FnBody);
        let first_id = stack
            .add_local("x".into(), sp_at(0), LocalKind::FnParam)
            .unwrap();
        let result = stack.add_local("x".into(), sp_at(5), LocalKind::FnParam);
        let (existing, _existing_span) = result.expect_err("should be Err for duplicate");
        assert_eq!(existing, first_id);
    }

    // Test 6: locals in different ScopeKinds do not interfere with each other.
    #[test]
    fn different_scope_kinds_no_interference() {
        let mut stack = ScopeStack::default();
        stack.push(ScopeKind::FnBody);
        let fn_id = stack
            .add_local("y".into(), sp_at(0), LocalKind::FnParam)
            .unwrap();

        stack.push(ScopeKind::Lambda);
        let lam_id = stack
            .add_local("z".into(), sp_at(10), LocalKind::LambdaParam)
            .unwrap();

        // z visible.
        let found_z = stack.lookup_local("z").expect("z found");
        assert_eq!(found_z.id, lam_id);
        // y visible through enclosing FnBody.
        let found_y = stack.lookup_local("y").expect("y found");
        assert_eq!(found_y.id, fn_id);
    }

    // Test 7: local_counter increments monotonically.
    #[test]
    fn local_counter_monotone() {
        let mut stack = ScopeStack::default();
        stack.push(ScopeKind::FnBody);
        let id0 = stack
            .add_local("a".into(), sp_at(0), LocalKind::FnParam)
            .unwrap();
        let id1 = stack
            .add_local("b".into(), sp_at(1), LocalKind::FnParam)
            .unwrap();
        let id2 = stack
            .add_local("c".into(), sp_at(2), LocalKind::FnParam)
            .unwrap();
        assert_eq!(id0.0, 0);
        assert_eq!(id1.0, 1);
        assert_eq!(id2.0, 2);
    }

    // Test 8: lookup walks all stack levels.
    #[test]
    fn lookup_walks_all_levels() {
        let mut stack = ScopeStack::default();
        stack.push(ScopeKind::Module);
        let root_id = stack
            .add_local("root".into(), sp_at(0), LocalKind::LetImmutable)
            .unwrap();

        stack.push(ScopeKind::FnBody);
        stack.push(ScopeKind::Block);
        stack.push(ScopeKind::MatchArm);

        // "root" defined four frames down — should still be found.
        let found = stack.lookup_local("root").expect("root visible");
        assert_eq!(found.id, root_id);
    }

    // Test 9: enclosing_kind finds the nearest scope of the given kind.
    #[test]
    fn enclosing_kind_finds_nearest() {
        let mut stack = ScopeStack::default();
        stack.push(ScopeKind::ActorBody);
        stack.push(ScopeKind::OnHandler);

        let actor_scope = stack.enclosing_kind(ScopeKind::ActorBody);
        assert!(actor_scope.is_some(), "ActorBody must be found");
        assert_eq!(actor_scope.unwrap().kind, ScopeKind::ActorBody);
    }

    // Test 10: enclosing_kind returns None when the kind is not in the stack.
    #[test]
    fn enclosing_kind_none_when_absent() {
        let mut stack = ScopeStack::default();
        stack.push(ScopeKind::FnBody);
        assert!(stack.enclosing_kind(ScopeKind::ActorBody).is_none());
    }
}
