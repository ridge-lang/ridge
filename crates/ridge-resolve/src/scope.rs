//! Lexical scope stack for intra-module name resolution (plan §3.3 / §4.5).
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

// ── ScopeNode / ScopeIndex ────────────────────────────────────────────────────

/// A persisted lexical scope: its byte range, its bound locals, and a pointer to
/// the enclosing scope.
///
/// Unlike [`Scope`] (a live frame discarded after the walk), a `ScopeNode` is
/// retained so the LSP can answer "which names are visible at this offset".
#[derive(Debug, Clone)]
pub struct ScopeNode {
    /// The syntactic context that introduced this scope.
    pub kind: ScopeKind,
    /// Byte range the scope covers in the source.
    pub range: Span,
    /// Index of the enclosing scope in [`ScopeIndex::nodes`]; `None` for the
    /// module root.
    pub parent: Option<u32>,
    /// Locals bound directly in this scope, in insertion order.
    pub locals: Vec<LocalEntry>,
}

/// A module's lexical scopes, flattened into a parent-linked tree.
///
/// Nodes are stored in pre-order (a parent always precedes its children), so a
/// `parent` index is always smaller than the child's own index. Empty unless
/// the walker was asked to record scopes (the LSP path).
#[derive(Debug, Default)]
pub struct ScopeIndex {
    /// All scopes, pre-order. Index `i` is referenced by children via `parent`.
    pub nodes: Vec<ScopeNode>,
}

impl ScopeIndex {
    /// Create an empty scope index.
    #[must_use]
    pub const fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// `true` when no scopes were recorded.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Return the locals visible at `offset`, innermost first.
    ///
    /// Walks from the narrowest scope containing `offset` up through its
    /// ancestors, collecting locals. A name bound in an inner scope shadows the
    /// same name in an outer scope, so only the inner binding is returned.
    #[must_use]
    pub fn visible_at(&self, offset: u32) -> Vec<&LocalEntry> {
        let mut cur = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.range.start <= offset && offset < n.range.end)
            .min_by_key(|(_, n)| n.range.end - n.range.start)
            .map(|(i, _)| i);

        let mut out: Vec<&LocalEntry> = Vec::new();
        while let Some(idx) = cur {
            let node = &self.nodes[idx];
            for local in &node.locals {
                if !out.iter().any(|e| e.name == local.name) {
                    out.push(local);
                }
            }
            cur = node.parent.map(|p| p as usize);
        }
        out
    }
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
    /// When `true`, [`push_with_start`](Self::push_with_start) /
    /// [`pop_into`](Self::pop_into) record each scope into [`retained`] so the
    /// LSP can query visible locals. The batch compiler leaves this `false`.
    record_scopes: bool,
    /// Persisted scope nodes, in pre-order. Filled only when `record_scopes`.
    retained: Vec<ScopeNode>,
    /// Node ids of the currently-open recorded scopes, used to thread parent
    /// links. Parallel to the live `stack` while recording.
    live_ids: Vec<u32>,
}

impl ScopeStack {
    /// Create a scope stack that records scopes for later LSP queries.
    #[must_use]
    pub fn with_recording(record: bool) -> Self {
        Self {
            record_scopes: record,
            ..Self::default()
        }
    }

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

    /// Push a scope, recording its start offset when recording is enabled.
    ///
    /// Behaves like [`push`](Self::push) for resolution; additionally allocates a
    /// retained [`ScopeNode`] (parented to the enclosing recorded scope) whose
    /// locals and end offset are filled in by [`pop_into`](Self::pop_into).
    pub fn push_with_start(&mut self, kind: ScopeKind, start: u32) {
        if self.record_scopes {
            let parent = self.live_ids.last().copied();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "scope count is bounded by program size"
            )]
            let id = self.retained.len() as u32;
            self.retained.push(ScopeNode {
                kind,
                range: Span::new(start, start),
                parent,
                locals: Vec::new(),
            });
            self.live_ids.push(id);
        }
        self.stack.push(Scope::new(kind));
    }

    /// Pop a scope, finalising its retained node (end offset + locals) when
    /// recording is enabled.
    pub fn pop_into(&mut self, end: u32) {
        debug_assert!(
            !self.stack.is_empty(),
            "ScopeStack::pop_into on empty stack"
        );
        let frame = self.stack.pop();
        if self.record_scopes {
            if let (Some(id), Some(frame)) = (self.live_ids.pop(), frame) {
                if let Some(node) = self.retained.get_mut(id as usize) {
                    node.range.end = end;
                    node.locals = frame.locals;
                }
            }
        }
    }

    /// Take the recorded scopes as a [`ScopeIndex`], leaving the stack's record
    /// empty (the index is empty when recording was disabled).
    pub fn take_scope_index(&mut self) -> ScopeIndex {
        ScopeIndex {
            nodes: std::mem::take(&mut self.retained),
        }
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

    // ── ScopeIndex.visible_at ──────────────────────────────────────────────────

    fn local(id: u32, name: &str) -> LocalEntry {
        LocalEntry {
            id: LocalId(id),
            name: name.to_owned(),
            def_span: sp_at(id),
            kind: LocalKind::LetImmutable,
        }
    }

    #[test]
    fn scope_index_visible_at_shadowing() {
        // Outer FnBody [0,100) binds `x`; inner Block [20,40) binds `y` and a
        // shadowing `x`.
        let index = ScopeIndex {
            nodes: vec![
                ScopeNode {
                    kind: ScopeKind::FnBody,
                    range: Span::new(0, 100),
                    parent: None,
                    locals: vec![local(0, "x")],
                },
                ScopeNode {
                    kind: ScopeKind::Block,
                    range: Span::new(20, 40),
                    parent: Some(0),
                    locals: vec![local(1, "y"), local(2, "x")],
                },
            ],
        };

        // Inside the inner block: `y` plus the inner `x` (shadowing the outer).
        let inner = index.visible_at(25);
        assert!(inner.iter().any(|e| e.name == "y"));
        assert_eq!(
            inner.iter().filter(|e| e.name == "x").count(),
            1,
            "x must appear once"
        );
        let x = inner.iter().find(|e| e.name == "x").unwrap();
        assert_eq!(x.id, LocalId(2), "inner x shadows outer x");

        // Outside the inner block: only the outer `x`.
        let outer = index.visible_at(5);
        assert_eq!(outer.len(), 1);
        assert_eq!(outer[0].id, LocalId(0));
    }

    #[test]
    fn scope_index_visible_at_outside_all_scopes() {
        let index = ScopeIndex {
            nodes: vec![ScopeNode {
                kind: ScopeKind::FnBody,
                range: Span::new(10, 20),
                parent: None,
                locals: vec![local(0, "a")],
            }],
        };
        assert!(index.visible_at(5).is_empty(), "before any scope");
        assert!(index.visible_at(50).is_empty(), "after any scope");
        assert_eq!(index.visible_at(15).len(), 1, "inside the scope");
    }

    #[test]
    fn scope_index_empty() {
        let index = ScopeIndex::new();
        assert!(index.is_empty());
        assert!(index.visible_at(0).is_empty());
    }
}
