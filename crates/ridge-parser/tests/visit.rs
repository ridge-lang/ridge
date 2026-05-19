//! Integration tests for the `ridge_ast::visit::Visit` trait.
//!
//! Tests parse `examples/log_analyzer.ridge` and walk the resulting AST with
//! sample visitors to verify that `walk_*` helpers recurse correctly.

use ridge_ast::{
    visit::{walk_expr, walk_module, Visit},
    Expr, Pattern,
};
use ridge_parser::parse_source;

const LOG_ANALYZER: &str = include_str!("../../../examples/log_analyzer.ridge");

// ── ExprCounter ──────────────────────────────────────────────────────────────

/// Counts every [`Expr`] node visited in the tree.
struct ExprCounter {
    count: usize,
}

impl<'ast> Visit<'ast> for ExprCounter {
    fn visit_expr(&mut self, e: &'ast Expr) {
        self.count += 1;
        walk_expr(self, e); // recurse into children
    }
}

/// Counts both expressions and patterns.
struct NodeCounter {
    expr_count: usize,
    pattern_count: usize,
}

impl<'ast> Visit<'ast> for NodeCounter {
    fn visit_expr(&mut self, e: &'ast Expr) {
        self.expr_count += 1;
        walk_expr(self, e);
    }

    fn visit_pattern(&mut self, p: &'ast Pattern) {
        self.pattern_count += 1;
        ridge_ast::visit::walk_pattern(self, p);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Parses `log_analyzer.ridge` and asserts the expression count meets the
/// empirical minimum of 50 (per T14 definition of done).
#[test]
fn visit_expr_count_log_analyzer() {
    let result = parse_source(LOG_ANALYZER);
    assert!(
        result.errors.is_empty(),
        "expected no parse errors in log_analyzer.ridge: {:#?}",
        result.errors
    );
    assert!(
        result.lex_errors.is_empty(),
        "expected no lex errors in log_analyzer.ridge: {:#?}",
        result.lex_errors
    );

    let mut counter = ExprCounter { count: 0 };
    walk_module(&mut counter, &result.module);

    assert!(
        counter.count >= 50,
        "expected >= 50 Expr nodes in log_analyzer.ridge, got {}",
        counter.count
    );
}

/// Parses `log_analyzer.ridge` and asserts both expression count (≥ 50) and
/// pattern count (≥ 5) are met.
#[test]
fn visit_pattern_count_log_analyzer() {
    let result = parse_source(LOG_ANALYZER);
    assert!(
        result.errors.is_empty(),
        "expected no parse errors in log_analyzer.ridge: {:#?}",
        result.errors
    );

    let mut counter = NodeCounter {
        expr_count: 0,
        pattern_count: 0,
    };
    walk_module(&mut counter, &result.module);

    assert!(
        counter.expr_count >= 50,
        "expected >= 50 Expr nodes in log_analyzer.ridge, got {}",
        counter.expr_count
    );
    assert!(
        counter.pattern_count >= 5,
        "expected >= 5 Pattern nodes in log_analyzer.ridge, got {}",
        counter.pattern_count
    );
}

/// Smoke test: `visit_module` default impl walks the tree without panicking.
#[test]
fn default_visit_module_traverses_without_panic() {
    struct Noop;
    impl Visit<'_> for Noop {}

    let result = parse_source(LOG_ANALYZER);
    assert!(result.errors.is_empty());

    let mut noop = Noop;
    noop.visit_module(&result.module);
}

/// Verify that `dyn Visit` works (trait is object-safe).
#[test]
fn visit_trait_is_object_safe() {
    struct Counter {
        count: usize,
    }
    impl<'ast> Visit<'ast> for Counter {
        fn visit_expr(&mut self, e: &'ast Expr) {
            self.count += 1;
            walk_expr(self, e);
        }
    }

    let result = parse_source("fn main = 1 + 2\n");
    assert!(result.errors.is_empty());

    let mut counter = Counter { count: 0 };
    let v: &mut dyn Visit<'_> = &mut counter;
    v.visit_module(&result.module);

    // 1 + 2 yields: Binary { lhs: 1, rhs: 2 } = 3 Exprs
    assert!(
        counter.count >= 3,
        "expected >= 3 Expr nodes, got {}",
        counter.count
    );
}

/// Verify that `walk_module` on an empty module doesn't visit any nodes.
#[test]
fn walk_empty_module_visits_nothing() {
    let result = parse_source("");
    assert!(result.errors.is_empty());

    let mut counter = ExprCounter { count: 0 };
    walk_module(&mut counter, &result.module);
    assert_eq!(
        counter.count, 0,
        "empty module should produce 0 Expr visits"
    );
}

/// A second expression counter, defined at module scope to avoid
/// `items_after_statements` clippy lint in `visit_module_default_and_walk_module_agree`.
struct ViaVisit {
    count: usize,
}

impl<'ast> Visit<'ast> for ViaVisit {
    fn visit_expr(&mut self, e: &'ast Expr) {
        self.count += 1;
        walk_expr(self, e);
    }
}

/// Verify that the default `visit_module` implementation walks the same nodes
/// as calling `walk_module` directly.
#[test]
fn visit_module_default_and_walk_module_agree() {
    let result = parse_source(LOG_ANALYZER);
    assert!(result.errors.is_empty());

    let mut direct = ExprCounter { count: 0 };
    walk_module(&mut direct, &result.module);

    let mut via = ViaVisit { count: 0 };
    via.visit_module(&result.module);

    assert_eq!(
        direct.count, via.count,
        "walk_module and visit_module (default) must visit the same number of Exprs"
    );
}

/// Verify the visitor correctly walks into nested module items (const, fn, type, actor).
#[test]
fn visit_counts_match_across_programs() {
    let srcs = [
        include_str!("../../../examples/url_shortener.ridge"),
        include_str!("../../../examples/rate_limiter.ridge"),
        include_str!("../../../examples/game_of_life.ridge"),
    ];

    for src in srcs {
        let result = parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:#?}",
            result.errors
        );

        let mut counter = ExprCounter { count: 0 };
        walk_module(&mut counter, &result.module);

        assert!(
            counter.count >= 10,
            "expected >= 10 Expr nodes in example, got {}",
            counter.count
        );
    }
}

/// Explicit test: `visit_module` is callable with a known-count program.
#[test]
fn visit_module_single_fn() {
    // fn f = 1 produces exactly 1 expr (the literal 1).
    // fn g x = x produces exactly 1 expr (the ident x).
    let result = parse_source("fn f = 1\nfn g x = x\n");
    assert!(result.errors.is_empty());

    let mut counter = ExprCounter { count: 0 };
    walk_module(&mut counter, &result.module);
    // 2 functions, each with 1 body Expr.
    assert_eq!(counter.count, 2, "expected exactly 2 Expr nodes");
}

/// Validates that patterns inside `match` arms are counted.
#[test]
fn visit_counts_patterns_in_match() {
    let src = "fn classify x =\n    match x\n        Some v -> v\n        None -> 0\n";
    let result = parse_source(src);
    assert!(
        result.errors.is_empty(),
        "unexpected errors: {:#?}",
        result.errors
    );

    let mut counter = NodeCounter {
        expr_count: 0,
        pattern_count: 0,
    };
    walk_module(&mut counter, &result.module);

    assert!(
        counter.pattern_count >= 2,
        "expected >= 2 Pattern nodes, got {}",
        counter.pattern_count
    );
}
