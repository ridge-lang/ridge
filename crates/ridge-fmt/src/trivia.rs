//! Trivia attachment and lookup utilities.
//!
//! This module provides helpers for looking up trivia items (line comments and
//! blank lines) that belong to a particular source line.  The formatter uses
//! these to re-insert trivia at the correct positions during emission.

use ridge_parser::Trivia;

/// A view into the trivia list that answers two questions efficiently:
///
/// 1. "What trivia items appear on source line `L`?"
/// 2. "Does a blank line appear between source lines `A` and `B`?"
///
/// Built once from the trivia vector returned by
/// [`ridge_parser::parse_module_with_trivia`] and shared across the printer.
pub struct TriviaMap {
    /// All trivia items, sorted by line number (ascending).
    items: Vec<Trivia>,
}

impl TriviaMap {
    /// Build a `TriviaMap` from an ordered trivia slice.
    #[must_use]
    pub fn new(trivia: &[Trivia]) -> Self {
        Self {
            items: trivia.to_vec(),
        }
    }

    /// Return all trivia items on line `line` (0-based).
    pub fn on_line(&self, line: u32) -> impl Iterator<Item = &Trivia> {
        self.items.iter().filter(move |t| match t {
            Trivia::LineComment { line: l, .. } => *l == line,
            Trivia::BlankLine { line: l } => *l == line,
        })
    }

    /// Return all line comments on line `line` (0-based).
    pub fn line_comments_on(&self, line: u32) -> impl Iterator<Item = &Trivia> {
        self.items
            .iter()
            .filter(move |t| matches!(t, Trivia::LineComment { line: l, .. } if *l == line))
    }

    /// Return `true` if there are any blank lines strictly between source
    /// lines `from` (exclusive) and `to` (exclusive).
    ///
    /// This is used to decide whether a blank line should be preserved between
    /// two declarations (the formatter always emits exactly one blank line
    /// between top-level decls regardless of how many blank lines the input
    /// had).
    pub fn has_blank_between(&self, from: u32, to: u32) -> bool {
        self.items.iter().any(|t| {
            if let Trivia::BlankLine { line } = t {
                *line > from && *line < to
            } else {
                false
            }
        })
    }

    /// Return all line comments that fall strictly between source lines `from`
    /// (exclusive) and `to` (exclusive).  These are "free-floating" comments
    /// that are not attached to a specific token.
    pub fn comments_between(&self, from: u32, to: u32) -> impl Iterator<Item = &Trivia> {
        self.items.iter().filter(move |t| {
            if let Trivia::LineComment { line, .. } = t {
                *line > from && *line < to
            } else {
                false
            }
        })
    }
}
