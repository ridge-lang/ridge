//! The `Block` AST node: a sequence of expressions separated by `NEWLINE`
//! inside an `INDENT`/`DEDENT` pair (grammar §5.3 line 517, §3.10).
//!
//! A `Block` is the body of `if`/`else` branches, `try`, `guard else`,
//! `init`, and multi-statement function bodies.  The *value* of a block is
//! its last expression; all preceding expressions are sequenced.
//!
//! See the parser's `parse_block` for the layout contract (§4.4).

use crate::{Expr, Span};

/// A multi-statement expression block delimited by `INDENT`/`DEDENT`.
///
/// # Layout contract
///
/// ```text
/// INDENT
///   Expr NEWLINE
///   Expr NEWLINE
///   Expr            -- no trailing NEWLINE before DEDENT required
/// DEDENT
/// ```
///
/// The `stmts` vec holds all expressions in order.  The block value is the
/// last element; all prior elements are sequenced and must return `Unit`
/// (enforced in Phase 4 by the type checker, not here).
///
/// An empty block (immediate `DEDENT` after `INDENT`) is a parse error
/// (`P014 EmptyBlock`); `stmts` is therefore never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The statements (expressions) comprising this block.  Never empty.
    pub stmts: Vec<Expr>,
    /// Span covering the full `INDENT … DEDENT` region.
    pub span: Span,
}
