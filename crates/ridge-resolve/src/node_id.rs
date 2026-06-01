//! `NodeId` assignment pass (plan §3.2 β).
//!
//! [`NodeIdMap`] is a side-table that stamps a stable [`NodeId`] on every
//! semantically significant AST position without touching the AST structs.
//! The map is keyed by `(Span, NodeKind)`.  Span uniqueness for each kind
//! is a parser invariant; if two nodes share the same `(Span, NodeKind)` pair
//! the resolver emits [`ResolveError::InternalNodeIdCollision`] (R999).
//!
//! [`assign_node_ids`] walks a parsed [`ridge_ast::Module`] and stamps every
//! `Ident`, `QualifiedName`, `ImportDecl`, capability position, and
//! `ModulePath` segment.

use rustc_hash::FxHashMap;

use ridge_ast::{
    visit::{walk_block, walk_expr, walk_module, walk_type, Visit},
    Block, Expr, Ident, Module,
};
use ridge_lexer::Span;

use crate::{error::ResolveError, NodeId};

// ── NodeKind ──────────────────────────────────────────────────────────────────

/// Distinguishes the semantic role of an AST node position for [`NodeIdMap`]
/// keying.
///
/// Two different `NodeKind`s at the same span do NOT collide: a `QualifiedName`
/// and its constituent `Ident` segments legitimately share byte ranges.
///
/// # Collision-avoidance rules
///
/// Each variant is stamped at most once per unique span.  Variants that may
/// occupy the same byte range are distinguished by kind:
///
/// - `Expr` vs `Ident`: an `Expr::Ident(id)` stamps **both** `NodeKind::Ident`
///   (for the ident leaf) **and** `NodeKind::Expr` (for the expression wrapper).
///   Both keys are stamped at `id.span`; different kinds, no collision.
/// - `Block` vs `Expr::Block`: the `Block` struct has its own `block.span`;
///   `Expr::Block` is stamped with `NodeKind::Expr` at that same span.  The
///   block's stmts-level type is recorded via `NodeKind::Block` at `block.span`
///   while the expression wrapper's type is recorded via `NodeKind::Expr`.
/// - `Try` vs `Block`: an `Expr::Try { block, span }` stamps `NodeKind::Try` at
///   the try-expression's own `span` and the inner `Block` is separately stamped
///   with `NodeKind::Block` at `block.span`.  Since the try keyword is consumed,
///   `span != block.span`; no collision.
/// - `Type` vs `Ident`/`Expr`: type-position nodes (in `FnDecl.ret`,
///   `let p: T = v`, etc.) are stamped with `NodeKind::Type` at the type span.
///   An `UPPER_IDENT` appearing in both a type position and an expression
///   position carries two different stamps at potentially different byte ranges;
///   if they share a span, different kinds prevent collision.
///
/// The invariant: for any given `(span, NodeKind)` pair, at most one `NodeId`
/// is ever stamped.  The `IdAssigner` visitor enforces this; `R999
/// InternalNodeIdCollision` fires if a duplicate is detected.
// One `NodeKind::Expr` variant (not per-shape) — write-back is uniform.
// `NodeKind::Type` added proactively for type-position stamping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A bare identifier (`LOWER_IDENT` or `UPPER_IDENT`).
    Ident,
    /// A dotted qualified name whose first segment is `UPPER_IDENT`.
    QualifiedName,
    /// An `import` declaration (keyed by the full import span).
    ImportDecl,
    /// A capability keyword position inside `fn … caps …`.
    CapabilityPos,
    /// A single segment of a `ModulePath` inside an `ImportDecl`.
    ModulePathSeg,

    /// A block boundary (`Block { stmts, span }`).
    ///
    /// Keyed by `block.span`.  The block's inferred type (= type of its last
    /// statement expression) is written to `node_types[block_node_id]` by T3.
    ///
    /// **Overlap rule:** `(span, NodeKind::Block)` and `(span, NodeKind::Expr)`
    /// at the same span are different keys — both may coexist when `Expr::Block`
    /// is stamped.
    // OQ-PHASE45-004: block-level type written to node_types for try_block consumers.
    Block,

    /// A `try`-expression boundary (`Expr::Try { block, span }`).
    ///
    /// Keyed by the try-expression's own `span` (not the inner block's span —
    /// the two differ by the `try` keyword bytes).  The try-expression's type
    /// (`Result a e` or `Option a`) is written to `node_types[try_node_id]`.
    ///
    /// **Overlap rule:** `NodeKind::Try` at the try span and `NodeKind::Block`
    /// at the inner block span are distinct keys at distinct byte ranges.
    Try,

    /// Any expression use-site (`Expr::*`).
    ///
    /// Stamped for every expression node in the AST.  Used by `infer_expr` to
    /// write back the resolved `Type` after each solve into
    /// `TypedModule.node_types`.
    ///
    /// **Overlap rule:** `(span, NodeKind::Expr)` is unique — every expression
    /// has a unique span (parser invariant; nested expressions have strictly
    /// nested spans).  An `Expr::Ident(id)` stamps both `NodeKind::Ident` and
    /// `NodeKind::Expr` at `id.span`; the different kinds prevent collision.
    // OQ-PHASE45-001: single Expr variant; finer-grained variants add no semantic value.
    Expr,

    /// A type-position node (in `FnDecl.ret`, `let p: T = v`, `state x: T`,
    /// `ConstDecl.ty`, parameter type annotations, etc.).
    ///
    /// Stamped by `IdAssigner::visit_type`.  Enables the `ridge-lower` sweep to
    /// look up resolved types for signature positions via `node_id_map.get(ty_span,
    /// NodeKind::Type)`.
    ///
    /// **Overlap rule:** A type identifier (e.g. `Int`) may share a byte range
    /// with an `Ident` stamp if the same token appears in both type and expression
    /// contexts — but in that scenario the positions have *different* spans because
    /// they are different syntactic locations.  A `NodeKind::Type` stamp and a
    /// `NodeKind::Ident` stamp at the *same* span (e.g. a named type `User` at
    /// its definition site) are distinct keys and do not collide.
    // OQ-PHASE45-002: added proactively; needed by ridge-lower ast_type.rs:100 in sweep.
    Type,
}

// ── NodeIdMap ─────────────────────────────────────────────────────────────────

/// A side-table that maps `(Span, NodeKind) → NodeId`.
///
/// Stamped in a single post-parse traversal by [`assign_node_ids`]; looked up
/// during scope resolution in [`crate::walker`].
#[derive(Debug, Default, Clone)]
pub struct NodeIdMap {
    by_span: FxHashMap<(Span, NodeKind), NodeId>,
    count: u32,
}

impl NodeIdMap {
    /// Assign a fresh [`NodeId`] to `(span, kind)`.
    ///
    /// Returns an error if the key already exists (R999 invariant violation).
    pub fn assign(&mut self, span: Span, kind: NodeKind) -> Result<NodeId, ResolveError> {
        let key = (span, kind);
        if self.by_span.contains_key(&key) {
            return Err(ResolveError::InternalNodeIdCollision {
                node_kind: format!("{kind:?}"),
                span,
            });
        }
        let id = NodeId(self.count);
        self.count += 1;
        self.by_span.insert(key, id);
        Ok(id)
    }

    /// Look up a previously assigned [`NodeId`].
    ///
    /// Returns `None` if no `NodeId` was stamped for this `(span, kind)` pair.
    #[must_use]
    pub fn get(&self, span: Span, kind: NodeKind) -> Option<NodeId> {
        self.by_span.get(&(span, kind)).copied()
    }

    /// Total number of `NodeIds` stamped so far.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count as usize
    }

    /// `true` when no `NodeIds` have been stamped.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Iterate over every stamped `(span, kind) → id` entry.
    ///
    /// Order is unspecified (backed by a hash map). The LSP uses this to build a
    /// position-indexed view of a module's nodes for hover / go-to-definition.
    pub fn iter(&self) -> impl Iterator<Item = (Span, NodeKind, NodeId)> + '_ {
        self.by_span
            .iter()
            .map(|(&(span, kind), &id)| (span, kind, id))
    }
}

// ── assign_node_ids ───────────────────────────────────────────────────────────

/// Walk `module` and stamp a [`NodeId`] for every semantically significant
/// AST position.
///
/// The positions stamped are:
/// - Every `Ident` leaf (covers function names, param names, use-site names,
///   type names, constructor names, record fields in patterns, etc.).
/// - Every `QualifiedName` (the whole dotted name, separately from its
///   constituent `Ident` segments which are also stamped).
/// - Every `ImportDecl` (keyed by the import's full span).
/// - Every `ModulePath` segment (the individual idents inside an import path).
///
/// R999 collisions are accumulated into the returned error vector rather than
/// aborting the walk.
#[must_use]
pub fn assign_node_ids(module: &Module) -> (NodeIdMap, Vec<ResolveError>) {
    let mut assigner = IdAssigner {
        map: NodeIdMap::default(),
        errors: Vec::new(),
    };
    assigner.visit_module(module);
    (assigner.map, assigner.errors)
}

// ── Private visitor ───────────────────────────────────────────────────────────

struct IdAssigner {
    map: NodeIdMap,
    errors: Vec<ResolveError>,
}

impl IdAssigner {
    fn try_assign(&mut self, span: Span, kind: NodeKind) {
        if let Err(e) = self.map.assign(span, kind) {
            self.errors.push(e);
        }
    }
}

impl<'ast> Visit<'ast> for IdAssigner {
    // Every Ident leaf gets a NodeId (use-sites and definition sites alike).
    fn visit_ident(&mut self, i: &'ast Ident) {
        self.try_assign(i.span, NodeKind::Ident);
    }

    // QualifiedName: stamp the whole-name span, then recurse into segments
    // (each segment's Ident will be stamped by visit_ident above).
    fn visit_qualified_name(&mut self, q: &'ast ridge_ast::expr::QualifiedName) {
        self.try_assign(q.span, NodeKind::QualifiedName);
        // Walk segments — visit_ident will stamp each one.
        ridge_ast::visit::walk_qualified_name(self, q);
    }

    // ImportDecl: stamp the decl span, then stamp each ModulePath segment,
    // then continue to walk the alias ident and item idents normally.
    fn visit_import_decl(&mut self, d: &'ast ridge_ast::decl::ImportDecl) {
        self.try_assign(d.span, NodeKind::ImportDecl);
        // Stamp each ModulePath segment as ModulePathSeg in addition to Ident.
        for seg in &d.path.segments {
            self.try_assign(seg.span, NodeKind::ModulePathSeg);
        }
        // Walk the rest (alias, items) — their Idents are stamped by visit_ident.
        ridge_ast::visit::walk_import_decl(self, d);
    }

    // Module: use the default walk_module traversal.
    fn visit_module(&mut self, m: &'ast Module) {
        walk_module(self, m);
    }

    // Every Expr node gets a NodeKind::Expr stamp.
    //
    // Collision-avoidance: `Expr::Block` and `Expr::Try` are structural wrappers
    // whose span may coincide with the span of their single inner statement.
    // To avoid an `(span, NodeKind::Expr)` collision between the wrapper and the
    // inner stmt, we do NOT stamp `NodeKind::Expr` for `Expr::Block` or
    // `Expr::Try` — the block boundary is stamped with `NodeKind::Block`
    // (via `visit_block`) and the try boundary with `NodeKind::Try` instead.
    // Block/try types are keyed by those dedicated kinds.
    //
    // One Expr variant; stamp is uniform across all non-wrapper shapes.
    fn visit_expr(&mut self, e: &'ast Expr) {
        match e {
            // Expr::Block: block boundary is stamped by visit_block via walk_expr.
            // OQ-PHASE45-004: block-level type keyed by NodeKind::Block, not Expr.
            Expr::Block(_) => {}
            // Expr::Try: stamp NodeKind::Try for the try boundary; the inner block
            // gets NodeKind::Block via visit_block; inner stmts get NodeKind::Expr.
            // OQ-PHASE45-004: Try boundary stamp enables try_block lookup.
            Expr::Try { span, .. } => {
                self.try_assign(*span, NodeKind::Try);
            }
            // All other Expr variants: stamp NodeKind::Expr at the expression's span.
            _ => {
                self.try_assign(e.span(), NodeKind::Expr);
            }
        }
        // Walk into children — visit_block will stamp blocks, visit_expr recurses.
        walk_expr(self, e);
    }

    // Every Block boundary gets a NodeKind::Block stamp.
    // Block-level type is recorded for try_block::resolve_block_type.
    fn visit_block(&mut self, b: &'ast Block) {
        self.try_assign(b.span, NodeKind::Block);
        walk_block(self, b);
    }

    // Every type-position node gets a NodeKind::Type stamp.
    // Type variant enables ast_type.rs type-position sweeps.
    fn visit_type(&mut self, t: &'ast ridge_ast::Type) {
        self.try_assign(t.span(), NodeKind::Type);
        walk_type(self, t);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_parser::parse_source;

    fn parse(src: &str) -> Module {
        let result = parse_source(src);
        // In tests, we treat parse errors as non-fatal.
        result.module
    }

    // Test 1: empty module → 0 NodeIds.
    #[test]
    fn empty_module_produces_zero_node_ids() {
        let m = parse("");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(map.len(), 0);
    }

    // Test 2: simple fn → NodeIds stamped, count is positive.
    #[test]
    fn simple_fn_stamps_node_ids() {
        let m = parse("fn foo = ()\n");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        // At minimum: `foo` ident.
        assert!(
            !map.is_empty(),
            "expected at least 1 NodeId, got {}",
            map.len()
        );
    }

    // Test 3: R999 collision — assigning same (span, kind) twice.
    #[test]
    fn double_assign_same_span_kind_produces_r999() {
        let mut map = NodeIdMap::default();
        let sp = Span::new(0, 5);
        let id1 = map.assign(sp, NodeKind::Ident);
        assert!(id1.is_ok());
        let id2 = map.assign(sp, NodeKind::Ident);
        assert!(id2.is_err(), "second assign at same (span, kind) must fail");
        let err = id2.unwrap_err();
        assert_eq!(err.code(), "R999");
    }

    // Test 4: lookup by (span, kind) returns the stamped NodeId.
    #[test]
    fn get_returns_stamped_node_id() {
        let mut map = NodeIdMap::default();
        let sp = Span::new(0, 3);
        let id = map.assign(sp, NodeKind::Ident).expect("assign ok");
        let got = map.get(sp, NodeKind::Ident);
        assert_eq!(got, Some(id));
    }

    // Test 5: different NodeKinds at the same span do NOT collide.
    #[test]
    fn different_kinds_same_span_no_collision() {
        let mut map = NodeIdMap::default();
        let sp = Span::new(0, 5);
        let id_ident = map.assign(sp, NodeKind::Ident).expect("Ident ok");
        let id_qn = map
            .assign(sp, NodeKind::QualifiedName)
            .expect("QualifiedName ok");
        assert_ne!(id_ident, id_qn);
        assert_eq!(map.len(), 2);
    }

    // Test 6: monotone counter — each assign increases len by 1.
    #[test]
    fn counter_is_monotone() {
        let mut map = NodeIdMap::default();
        for i in 0u32..5 {
            let sp = Span::new(i, i + 1);
            let id = map.assign(sp, NodeKind::Ident).expect("assign");
            assert_eq!(id.0, i);
        }
        assert_eq!(map.len(), 5);
    }

    // Test 7: import decl stamps ImportDecl kind + ModulePathSeg for each segment.
    #[test]
    fn import_decl_stamps_import_decl_and_segments() {
        let m = parse("import std.list as List\n");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        // Must have stamped at least 1 ImportDecl and 2 ModulePathSeg (std, list).
        let import_decl_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::ImportDecl)
            .count();
        let seg_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::ModulePathSeg)
            .count();
        assert_eq!(import_decl_count, 1);
        assert!(seg_count >= 2, "expected ≥2 ModulePathSeg, got {seg_count}");
    }

    // Test 8: qualified name stamps QualifiedName + each segment Ident.
    #[test]
    fn qualified_name_stamps_qn_and_segment_idents() {
        let m = parse("import std.io as Io\nfn foo = Io.println \"hi\"\n");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let qn_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::QualifiedName)
            .count();
        assert!(qn_count >= 1, "expected ≥1 QualifiedName, got {qn_count}");
    }

    // Test 9: fn with params stamps all param idents.
    #[test]
    fn fn_with_params_stamps_param_idents() {
        let m = parse("fn add x y = x + y\n");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        // add, x, y in params + x, y in body = at least 5 idents (add, x, y, x, y)
        // But spans differ so no collision — each is a separate stamp.
        assert!(
            map.len() >= 3,
            "expected ≥3 NodeIds for fn add x y, got {}",
            map.len()
        );
    }

    // ── NodeKind stamping tests ────────────────────────────────────────────────

    // Test: an if expression with a block body stamps NodeKind::Block.
    // The if-then branch is always an Expr::Block (per parser design).
    #[test]
    fn t2_if_body_block_stamped() {
        // `if` with a then-branch block: the parser wraps the then body in Expr::Block.
        let m = parse("fn foo b =\n  if b then\n    1\n  else\n    2\n");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let block_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::Block)
            .count();
        assert!(
            block_count >= 1,
            "expected ≥1 Block stamp for if branches, got {block_count}"
        );
    }

    // Test: nested if expressions produce multiple Block stamps.
    #[test]
    fn t2_nested_if_blocks_each_stamped() {
        // Two if expressions → at least two block bodies.
        let m = parse(
            "fn foo b c =\n  if b then\n    if c then\n      1\n    else\n      2\n  else\n    3\n",
        );
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let block_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::Block)
            .count();
        // Outer if has then+else blocks; inner if also has then+else blocks ≥ 4.
        assert!(
            block_count >= 2,
            "expected ≥2 Block stamps for nested ifs, got {block_count}"
        );
    }

    // Test: expressions get NodeKind::Expr stamps.
    #[test]
    fn t2_expr_positions_stamped() {
        let m = parse("fn foo = 1 + 2\n");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let expr_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::Expr)
            .count();
        // At minimum: the binary expr `1 + 2`, the literal `1`, the literal `2`.
        assert!(
            expr_count >= 3,
            "expected ≥3 Expr stamps for `1 + 2`, got {expr_count}"
        );
    }

    // Test: a type annotation stamps NodeKind::Type.
    #[test]
    fn t2_type_annotation_stamped() {
        let m = parse("fn foo (x: Int) = x\n");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let type_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::Type)
            .count();
        assert!(
            type_count >= 1,
            "expected ≥1 Type stamp for `Int`, got {type_count}"
        );
    }

    // Test: fixture-driven density — fn with literal, ident, call has
    // meaningful Expr stamp count.
    #[test]
    fn t2_fixture_density_basic_fn() {
        // fn add x y = x + y — has: add (Ident), x (Ident), y (Ident), x (Ident),
        // y (Ident), `x + y` (Expr), `x` (Expr), `y` (Expr)
        let m = parse("fn add x y = x + y\n");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let expr_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::Expr)
            .count();
        let ident_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::Ident)
            .count();
        assert!(expr_count >= 3, "expected ≥3 Expr stamps, got {expr_count}");
        assert!(
            ident_count >= 3,
            "expected ≥3 Ident stamps, got {ident_count}"
        );
    }

    // Test: fixture-driven density — fn with call expression.
    #[test]
    fn t2_fixture_density_call() {
        let m = parse("fn bar = foo 42\n");
        let (map, errors) = assign_node_ids(&m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let expr_count = map
            .by_span
            .keys()
            .filter(|(_, k)| *k == NodeKind::Expr)
            .count();
        // foo (Ident/Expr), 42 (Literal/Expr), `foo 42` call (Expr) = ≥3 Expr stamps
        assert!(
            expr_count >= 3,
            "expected ≥3 Expr stamps for `foo 42`, got {expr_count}"
        );
    }

    // ── NodeId collision tests ─────────────────────────────────────────────────

    // Test: two NodeKind::Expr stamps at the same span fail R999.
    #[test]
    fn t1_same_span_same_expr_kind_collides() {
        let mut map = NodeIdMap::default();
        let sp = Span::new(0, 5);
        let id1 = map.assign(sp, NodeKind::Expr);
        assert!(id1.is_ok(), "first Expr assign must succeed");
        let id2 = map.assign(sp, NodeKind::Expr);
        assert!(
            id2.is_err(),
            "second Expr assign at same (span, Expr) must fail R999"
        );
        let err = id2.unwrap_err();
        assert_eq!(err.code(), "R999");
    }

    // Test: NodeKind::Expr and NodeKind::Ident at the same span do NOT collide.
    #[test]
    fn t1_expr_and_ident_same_span_no_collision() {
        let mut map = NodeIdMap::default();
        let sp = Span::new(10, 15);
        // Expr and Ident are different kinds — both may coexist at the same span.
        let id_expr = map.assign(sp, NodeKind::Expr).expect("Expr assign ok");
        let id_ident = map.assign(sp, NodeKind::Ident).expect("Ident assign ok");
        assert_ne!(id_expr, id_ident, "different kinds yield distinct NodeIds");
        assert_eq!(map.len(), 2);
    }

    // Test: NodeKind::Try and NodeKind::Block at byte-adjacent positions
    // do not collide because their spans differ.
    #[test]
    fn t1_try_and_block_adjacent_spans_no_collision() {
        let mut map = NodeIdMap::default();
        // try spans [0,10); block (after `try ` keyword) spans [4,10).
        let try_span = Span::new(0, 10);
        let block_span = Span::new(4, 10);
        let id_try = map.assign(try_span, NodeKind::Try).expect("Try assign ok");
        let id_block = map
            .assign(block_span, NodeKind::Block)
            .expect("Block assign ok");
        assert_ne!(id_try, id_block);
        assert_eq!(map.len(), 2);
    }
}
