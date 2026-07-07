//! Pattern parsing (grammar §7).
//!
//! Two public(crate) entry points:
//!
//! - [`parse_pattern`] — handles top-level operators (`::` right-associative,
//!   `@` alias).
//! - [`parse_pattern_atom`] — parses a single atomic pattern form.
//!
//! # Right-associativity of `::`
//!
//! `parse_pattern` recurses into itself for the RHS of `::`, giving:
//!
//! ```text
//! a :: b :: rest  →  Cons { head: a, tail: Cons { head: b, tail: rest } }
//! ```
//!
//! # `@` binding
//!
//! `x @ Pattern` — the LHS must be a `Var`; `@` binds tighter than `::`.
//! After parsing an atom, if the atom is a `Var` and the next token is `@`,
//! we consume `@` and parse another atom for the inner pattern, producing
//! `As { name, inner }`.  That combined result then participates in any
//! following `::` chain.
//!
//! # Inline record patterns
//!
//! A bare `{` in pattern position produces `Pattern::Record` (inline record
//! pattern).  P018 (`BareRecordPattern`) was retired in 0.2.12 when bare
//! record patterns became fully supported.

// These functions are called by tests in this file.  They will be called from
// production code (match/let/lambda).  Suppress dead_code until then.
#![allow(dead_code)]
#![allow(clippy::redundant_pub_crate)]

use ridge_ast::{pattern::ListPatElem, FieldPattern, Ident, Pattern};
use ridge_lexer::Token;

use crate::{
    cursor::{Cursor, DepthGuard},
    error::ParseError,
    expr::parse_literal,
};

// ── parse_pattern ─────────────────────────────────────────────────────────────

/// Parse a full pattern including `::` (right-assoc) and `@` (alias).
///
/// Grammar §7:
/// ```ebnf
/// Pattern   ::= AsPattern | PatternAtom ;
/// AsPattern ::= LOWER_IDENT "@" PatternAtom ;
/// ListConsPattern ::= PatternAtom "::" Pattern ;
/// ```
///
/// Operator precedence (tightest first):
/// 1. `@` — only when the LHS is a `Var`; inner is an atom.
/// 2. `::` — right-associative; the whole pattern (including possible `@`)
///    is the head.
pub(crate) fn parse_pattern(cur: &mut Cursor<'_>) -> Result<Pattern, ParseError> {
    // Bound the descent; `::` cons, `(…)`/`[…]` groups, and tuple elements all
    // recurse back through here, so one guard at the entry caps the whole pattern.
    let guard = DepthGuard::enter(cur)?;
    let cur = &mut *guard.cur;

    // Parse the first atom.
    let mut left = parse_pattern_atom(cur)?;

    // Handle `@` alias — only valid when the LHS is a bare Var.
    if cur.peek() == &Token::At {
        match left {
            Pattern::Var {
                ref name,
                span: var_span,
            } => {
                let name = name.clone();
                cur.bump(); // consume `@`
                let inner = parse_pattern_atom(cur)?;
                let full_span = var_span.merge(inner.span());
                left = Pattern::As {
                    name,
                    inner: Box::new(inner),
                    span: full_span,
                };
            }
            _ => {
                // `@` without a leading variable name: P001.
                return Err(ParseError::Expected {
                    span: cur.span(),
                    expected: "<identifier>",
                    found: format!(
                        "`@` requires a lower-case name on the left, found `{}`",
                        cur.peek()
                    ),
                });
            }
        }
    }

    // Handle `::` — right-associative by recursing into parse_pattern.
    if cur.peek() == &Token::ColonColon {
        let start_span = left.span();
        cur.bump(); // consume `::`
        let tail = parse_pattern(cur)?; // right-recursive
        let full_span = start_span.merge(tail.span());
        return Ok(Pattern::Cons {
            head: Box::new(left),
            tail: Box::new(tail),
            span: full_span,
        });
    }

    Ok(left)
}

/// Parse a match-arm pattern, including a top-level or-pattern `p1 | p2 | …`.
///
/// Or-patterns are valid only at the root of a match arm (grammar §6.4
/// `MatchArm`). The `|` separator is the loosest pattern operator, so each
/// alternative is a full [`parse_pattern`] (which already binds `@` and `::`
/// tighter). A single alternative is returned unwrapped; two or more wrap into
/// [`Pattern::Or`]. Because only this entry point consumes `|`, a `|` inside a
/// parenthesised, cons, or constructor sub-pattern is left for its caller —
/// which rejects it — so nested or-patterns do not parse.
pub(crate) fn parse_match_pattern(cur: &mut Cursor<'_>) -> Result<Pattern, ParseError> {
    let first = parse_pattern(cur)?;
    if cur.peek() != &Token::Pipe {
        return Ok(first);
    }

    let start = first.span();
    let mut alts = vec![first];
    while cur.peek() == &Token::Pipe {
        cur.bump(); // consume `|`
        alts.push(parse_pattern(cur)?);
    }
    let end = alts.last().map_or(start, Pattern::span);
    Ok(Pattern::Or {
        alts,
        span: start.merge(end),
    })
}

// ── parse_pattern_atom ────────────────────────────────────────────────────────

/// Parse a single atomic pattern (grammar §7 `PatternAtom`).
///
/// Dispatch table:
///
/// | Peek token | Result |
/// |---|---|
/// | `_` | `Pattern::Wildcard` |
/// | `LOWER_IDENT` | `Pattern::Var` |
/// | literal tokens | `Pattern::Literal` |
/// | `{` | `Pattern::Record` (inline record pattern) |
/// | `(` | tuple (≥2 elems), paren (1 elem), or P002 for `()` |
/// | `UPPER_IDENT` | `Pattern::Constructor` |
/// | anything else | `P002 UnexpectedToken` |
pub(crate) fn parse_pattern_atom(cur: &mut Cursor<'_>) -> Result<Pattern, ParseError> {
    let span = cur.span();

    match cur.peek() {
        // ── Wildcard `_` ──────────────────────────────────────────────────────
        Token::Underscore => {
            cur.bump();
            Ok(Pattern::Wildcard { span })
        }

        // ── Variable (lower-case or private `_foo`) ───────────────────────────
        // The lexer emits `_foo` as `LowerIdent`; bare `_` is `Underscore`.
        Token::LowerIdent(_) => {
            let text = match cur.bump() {
                Token::LowerIdent(s) => s.clone(),
                _ => unreachable!(),
            };
            Ok(Pattern::Var {
                name: Ident::new(text, span),
                span,
            })
        }

        // ── Literals ──────────────────────────────────────────────────────────
        Token::IntDec(_)
        | Token::IntBin(_)
        | Token::IntOct(_)
        | Token::IntHex(_)
        | Token::Float(_)
        | Token::DecimalLit(_)
        | Token::KwTrue
        | Token::KwFalse
        | Token::TextLit(_) => {
            let lit = parse_literal(cur)?;
            let lit_span = lit.span();
            Ok(Pattern::Literal {
                lit,
                span: lit_span,
            })
        }

        // ── Bracketed list pattern `[…]` ─────────────────────────────────────
        //
        // Handles the full bracketed list pattern form (D258):
        //   `[]`              → Pattern::ListNil
        //   `[a, b, c]`       → Pattern::List { elements: [Elem(a), Elem(b), Elem(c)] }
        //   `[a, ..]`         → Pattern::List { elements: [Elem(a), Rest { bind: None }] }
        //   `[a, rest @ ..]`  → Pattern::List { elements: [Elem(a), Rest { bind: Some(rest) }] }
        Token::LBrack => {
            cur.bump(); // consume `[`

            // Empty list `[]`.
            if cur.peek() == &Token::RBrack {
                let end_span = cur.span();
                cur.bump();
                return Ok(Pattern::ListNil {
                    span: span.merge(end_span),
                });
            }

            let (elements, has_rest) = parse_list_pattern_elements(cur)?;

            let end_span = cur.expect(&Token::RBrack)?;
            let full_span = span.merge(end_span);

            // A list pattern with no elements and no rest is `[]` — handled
            // above; reaching here with an empty elements vec is defensive.
            if elements.is_empty() && !has_rest {
                return Ok(Pattern::ListNil { span: full_span });
            }

            Ok(Pattern::List {
                elements,
                span: full_span,
            })
        }

        // ── Inline record pattern `{ field, … }` or `{ field, .. }` ──────────
        Token::LBrace => parse_inline_record_pattern(cur),

        // ── Parenthesised / tuple ─────────────────────────────────────────────
        Token::LParen => parse_paren_or_tuple_pattern(cur),

        // ── Constructor pattern `UPPER_IDENT …` ───────────────────────────────
        Token::UpperIdent(_) => parse_constructor_pattern(cur),

        // ── Reserved keyword used as a binding name (`let init = …`) ─────────
        tok if tok.keyword_text().is_some() => {
            let keyword = tok.keyword_text().unwrap_or("?");
            Err(ParseError::ReservedKeywordAsIdent {
                span,
                keyword,
                position: "a pattern",
            })
        }

        // ── Everything else ───────────────────────────────────────────────────
        _ => Err(ParseError::UnexpectedToken {
            span,
            description: format!("unexpected token `{}` in pattern position", cur.peek()),
        }),
    }
}

// ── parse_inline_record_pattern (internal) ────────────────────────────────────

/// Parse an inline record pattern `{ field, … }` or `{ field, .. }`.
///
/// Grammar:
/// ```ebnf
/// RecordPattern ::= '{' '}'
///                 | '{' RecordPatField (',' RecordPatField)* ','? '}'
///                 | '{' RecordPatField (',' RecordPatField)* ',' '..' ','? '}'
///                 | '{' '..' '}' ;
/// RecordPatField ::= LOWER_IDENT               (* shorthand: bind to same-named var *)
///                  | LOWER_IDENT '=' Pattern ; (* explicit: match field against pattern *)
/// ```
///
/// `has_rest = true` when `..` is present.  `..` must be last.
///
/// Precondition: `cur.peek() == Token::LBrace`.
fn parse_inline_record_pattern(cur: &mut Cursor<'_>) -> Result<Pattern, ParseError> {
    let start_span = cur.span();
    cur.bump(); // consume `{`

    // `{}` — empty record pattern.
    if cur.peek() == &Token::RBrace {
        let end_span = cur.span();
        cur.bump();
        return Ok(Pattern::Record {
            fields: vec![],
            has_rest: false,
            span: start_span.merge(end_span),
        });
    }

    // `{ .. }` — rest-only pattern.
    if cur.peek() == &Token::DotDot {
        cur.bump(); // consume `..`
                    // Optional trailing comma.
        if cur.peek() == &Token::Comma {
            cur.bump();
        }
        let end_span = cur.expect(&Token::RBrace)?;
        return Ok(Pattern::Record {
            fields: vec![],
            has_rest: true,
            span: start_span.merge(end_span),
        });
    }

    let mut fields: Vec<FieldPattern> = Vec::new();
    let mut has_rest = false;

    loop {
        // Check for `..` rest marker (must be last).
        if cur.peek() == &Token::DotDot {
            cur.bump(); // consume `..`
            has_rest = true;
            // Optional trailing comma.
            if cur.peek() == &Token::Comma {
                cur.bump();
            }
            break;
        }

        let field_span = cur.span();

        // Field name — must be lowercase.
        let name_text = match cur.peek().clone() {
            Token::LowerIdent(s) => {
                cur.bump();
                s
            }
            _ => {
                return Err(ParseError::UnexpectedToken {
                    span: field_span,
                    description: format!(
                        "expected a lowercase field name in record pattern, found `{}`",
                        cur.peek()
                    ),
                });
            }
        };
        let name = Ident::new(name_text, field_span);

        // Shorthand `{ x }` vs. explicit `{ x = pat }`.
        let (pat, fp_end) = if cur.peek() == &Token::Assign {
            cur.bump(); // consume `=`
            let inner = parse_pattern(cur)?;
            let end = inner.span();
            (Some(inner), end)
        } else {
            (None, field_span)
        };

        fields.push(FieldPattern {
            name,
            pattern: pat,
            span: field_span.merge(fp_end),
        });

        // Separator: `,` or end.
        if cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
                        // Trailing comma before `}` — done.
            if cur.peek() == &Token::RBrace {
                break;
            }
        } else {
            break;
        }
    }

    let end_span = cur.expect(&Token::RBrace)?;
    Ok(Pattern::Record {
        fields,
        has_rest,
        span: start_span.merge(end_span),
    })
}

// ── parse_constructor_pattern (internal) ─────────────────────────────────────

/// Parse a constructor pattern (grammar §7.4).
///
/// Syntax:
/// ```ebnf
/// ConstructorPattern ::= UPPER_IDENT [ "{" FieldPatternList "}" ]
///                                    [ PatternArg { PatternArg } ] ;
/// ```
///
/// Precondition: `cur.peek()` is `Token::UpperIdent`.
fn parse_constructor_pattern(cur: &mut Cursor<'_>) -> Result<Pattern, ParseError> {
    let start_span = cur.span();

    let name_text = match cur.bump() {
        Token::UpperIdent(s) => s.clone(),
        _ => unreachable!("precondition: current token is UpperIdent"),
    };
    let name = Ident::new(name_text, start_span);
    let mut end_span = start_span;

    // Check for the record-body form `{ … }`.
    if cur.peek() == &Token::LBrace {
        cur.bump(); // consume `{`
        let (fields, has_rest) = parse_field_pattern_list(cur)?;
        let rbrace_span = cur.expect(&Token::RBrace)?;
        end_span = rbrace_span;
        return Ok(Pattern::Constructor {
            name,
            fields: Some(fields),
            has_rest,
            args: vec![],
            span: start_span.merge(end_span),
        });
    }

    // Positional form: greedily consume zero or more `PatternAtom` arguments.
    let mut args: Vec<Pattern> = Vec::new();
    while can_start_pattern_atom(cur) {
        let arg = parse_pattern_atom(cur)?;
        end_span = arg.span();
        args.push(arg);
    }

    Ok(Pattern::Constructor {
        name,
        fields: None,
        has_rest: false,
        args,
        span: start_span.merge(end_span),
    })
}

// ── parse_field_pattern_list (internal) ──────────────────────────────────────

/// Parse `FieldPattern { "," FieldPattern } [ "," [ ".." ] ]` inside `{ … }`.
///
/// Precondition: the opening `{` has already been consumed.
/// The closing `}` is NOT consumed here; the caller handles it.
///
/// Returns `(fields, has_rest)` where `has_rest` is `true` when a trailing
/// `..` was present (D259 record rest pattern).
fn parse_field_pattern_list(cur: &mut Cursor<'_>) -> Result<(Vec<FieldPattern>, bool), ParseError> {
    let mut fields: Vec<FieldPattern> = Vec::new();
    let mut has_rest = false;

    // Empty record body `{}` is allowed (edge case).
    if cur.peek() == &Token::RBrace {
        return Ok((fields, has_rest));
    }

    loop {
        // A leading `..` with no preceding field name — record rest at the
        // start of the list (or after a comma).
        if cur.peek() == &Token::DotDot {
            cur.bump(); // consume `..`
            has_rest = true;
            // After `..` the only valid token is `}` (or an optional trailing
            // comma before `}`).
            if cur.peek() == &Token::Comma {
                cur.bump(); // consume optional trailing `,`
            }
            break;
        }

        let field = parse_field_pattern(cur)?;
        fields.push(field);

        if cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
                        // Trailing comma: if next is `}`, stop.
            if cur.peek() == &Token::RBrace {
                break;
            }
            // `..` after a comma — record rest (D259).
            if cur.peek() == &Token::DotDot {
                cur.bump(); // consume `..`
                has_rest = true;
                // Accept an optional trailing comma after `..`.
                if cur.peek() == &Token::Comma {
                    cur.bump();
                }
                break;
            }
            // Otherwise continue to the next field.
        } else {
            // No comma: must be followed by `}`.
            break;
        }
    }

    Ok((fields, has_rest))
}

// ── parse_field_pattern (internal) ───────────────────────────────────────────

/// Parse a single `FieldPattern`:
///
/// - Explicit: `LOWER_IDENT "=" Pattern`
/// - Shorthand: `LOWER_IDENT` — binds the field to a variable of the same name.
///
/// The `=` token in the lexer is `Token::Assign`.
fn parse_field_pattern(cur: &mut Cursor<'_>) -> Result<FieldPattern, ParseError> {
    let name_span = cur.span();

    // Expect a lower-case identifier as the field name.
    let name_text = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            s
        }
        _ => {
            return Err(ParseError::Expected {
                span: name_span,
                expected: "<identifier>",
                found: cur.peek().to_string(),
            });
        }
    };
    let name = Ident::new(name_text, name_span);

    // Check for explicit binding `= Pattern`.
    if cur.peek() == &Token::Assign {
        cur.bump(); // consume `=`
        let pat = parse_pattern(cur)?;
        let full_span = name_span.merge(pat.span());
        return Ok(FieldPattern {
            name,
            pattern: Some(pat),
            span: full_span,
        });
    }

    // Shorthand: no binding — field is bound to a variable of the same name.
    Ok(FieldPattern {
        name,
        pattern: None,
        span: name_span,
    })
}

// ── parse_list_pattern_elements (internal) ────────────────────────────────────

/// Parse the element list inside `[…]` (D258).
///
/// Returns `(elements, has_rest)`.  `has_rest` is `true` when at least one
/// `..` element was present.
///
/// Accepted forms (comma-separated, trailing comma allowed):
/// - `Pattern`          → `ListPatElem::Elem(pat)`
/// - `..`               → `ListPatElem::Rest { bind: None }`
/// - `IDENT @ ..`       → `ListPatElem::Rest { bind: Some(ident) }`
///
/// The `..` may appear in any position (prefix, middle, or suffix):
/// - `[a, ..]`          — prefix rest
/// - `[.., z]`          — suffix rest
/// - `[a, .., z]`       — middle rest
///
/// Errors:
/// - `P024 MultipleRestInListPattern` — more than one `..` in the list.
///
/// Precondition: the opening `[` has been consumed and the current token is
/// NOT `]`.
fn parse_list_pattern_elements(
    cur: &mut Cursor<'_>,
) -> Result<(Vec<ListPatElem>, bool), ParseError> {
    let mut elements: Vec<ListPatElem> = Vec::new();
    let mut has_rest = false;

    loop {
        let elem_span = cur.span();

        // ── `..` bare rest ────────────────────────────────────────────────────
        if cur.peek() == &Token::DotDot {
            if has_rest {
                return Err(ParseError::MultipleRestInListPattern { span: elem_span });
            }
            cur.bump(); // consume `..`
            has_rest = true;
            elements.push(ListPatElem::Rest {
                bind: None,
                span: elem_span,
            });

            // After a rest element, consume optional trailing comma and
            // continue parsing — suffix elements after `..` are now supported.
            if cur.peek() == &Token::Comma {
                cur.bump(); // consume `,`
                if cur.peek() == &Token::RBrack {
                    break; // trailing comma before `]`
                }
                // More elements follow — these are suffix elements after the rest.
                continue;
            }
            // No comma: must be `]`.
            break;
        }

        // ── `IDENT @ ..` bound rest ───────────────────────────────────────────
        //
        // Detect `LOWER_IDENT` followed by `@` followed by `..` — this is the
        // bound rest form.  We special-case it here so it does NOT go through
        // the general `@` as-pattern path in `parse_pattern`.
        if let Token::LowerIdent(ref name_text) = cur.peek().clone() {
            // Peek ahead: LowerIdent `@` DotDot
            // We need two-token lookahead.  Use a temporary clone check:
            // bump the ident, check for `@`, then check for `..`.
            let name_text = name_text.clone();
            let name_span = elem_span;
            cur.bump(); // consume the ident tentatively

            if cur.peek() == &Token::At {
                cur.bump(); // consume `@`
                if cur.peek() == &Token::DotDot {
                    // Confirmed: `IDENT @ ..`
                    let dot_span = cur.span();
                    cur.bump(); // consume `..`
                    if has_rest {
                        return Err(ParseError::MultipleRestInListPattern { span: dot_span });
                    }
                    has_rest = true;
                    let full_rest_span = name_span.merge(dot_span);
                    elements.push(ListPatElem::Rest {
                        bind: Some(Ident::new(name_text, name_span)),
                        span: full_rest_span,
                    });

                    // After a bound rest, consume optional trailing comma and
                    // continue — suffix elements are allowed after `name @ ..`.
                    if cur.peek() == &Token::Comma {
                        cur.bump();
                        if cur.peek() == &Token::RBrack {
                            break; // trailing comma before `]`
                        }
                        continue;
                    }
                    break;
                }
                // `IDENT @` but not followed by `..`: fall back to normal `@`
                // as-pattern via parse_pattern.  We already consumed the ident
                // and `@`; reconstruct as `As { name, inner: parse_pattern_atom }`.
                let inner = parse_pattern_atom(cur)?;
                let full_span = name_span.merge(inner.span());
                elements.push(ListPatElem::Elem(Pattern::As {
                    name: Ident::new(name_text, name_span),
                    inner: Box::new(inner),
                    span: full_span,
                }));
            } else {
                // Not an `@` — treat as a plain Var pattern.
                elements.push(ListPatElem::Elem(Pattern::Var {
                    name: Ident::new(name_text, name_span),
                    span: name_span,
                }));
            }
        } else {
            // Normal pattern element.
            let pat = parse_pattern(cur)?;
            elements.push(ListPatElem::Elem(pat));
        }

        // ── Separator / terminator ────────────────────────────────────────────
        if cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
                        // Trailing comma before `]`.
            if cur.peek() == &Token::RBrack {
                break;
            }
            // Continue to the next element.
        } else {
            break;
        }
    }

    Ok((elements, has_rest))
}

// ── parse_paren_or_tuple_pattern (internal) ───────────────────────────────────

/// Parse `(…)` in pattern position — three cases:
///
/// 1. `()` — no `Unit` pattern variant exists; emit P002.
/// 2. `(Pattern)` — `Pattern::Paren { inner }`.
/// 3. `(Pattern, Pattern, …)` with ≥2 elements — `Pattern::Tuple { elems }`.
///
/// Precondition: `cur.peek() == &Token::LParen`.
fn parse_paren_or_tuple_pattern(cur: &mut Cursor<'_>) -> Result<Pattern, ParseError> {
    let start_span = cur.span();
    cur.bump(); // consume `(`

    // Case 1: `()` — unit is not a valid pattern in 0.1.0.
    if cur.peek() == &Token::RParen {
        let span = cur.span();
        return Err(ParseError::UnexpectedToken {
            span,
            description: "unit `()` is not a valid pattern in 0.1.0".to_string(),
        });
    }

    // Parse the first pattern.
    let first = parse_pattern(cur)?;

    // Case 3: tuple — `, Pattern` repeats until `)`.
    if cur.peek() == &Token::Comma {
        let mut elems = vec![first];
        while cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
            elems.push(parse_pattern(cur)?);
        }
        let end_span = cur.expect(&Token::RParen)?;
        return Ok(Pattern::Tuple {
            elems,
            span: start_span.merge(end_span),
        });
    }

    // Case 2: paren — single pattern, no comma.
    let end_span = cur.expect(&Token::RParen)?;
    Ok(Pattern::Paren {
        inner: Box::new(first),
        span: start_span.merge(end_span),
    })
}

// ── can_start_pattern_atom (internal) ────────────────────────────────────────

/// Return `true` if the current token can begin a `PatternAtom`.
///
/// Used to drive the greedy argument loop in `parse_constructor_pattern`.
/// The set excludes all tokens that act as delimiters or terminators in the
/// contexts where constructor patterns appear.
fn can_start_pattern_atom(cur: &Cursor<'_>) -> bool {
    matches!(
        cur.peek(),
        Token::Underscore
            | Token::LowerIdent(_)
            | Token::UpperIdent(_)
            | Token::IntDec(_)
            | Token::IntBin(_)
            | Token::IntOct(_)
            | Token::IntHex(_)
            | Token::Float(_)
            | Token::DecimalLit(_)
            | Token::KwTrue
            | Token::KwFalse
            | Token::TextLit(_)
            | Token::LParen
            | Token::LBrack
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Literal, Span};
    use ridge_lexer::tokenize;

    fn lex(src: &str) -> Vec<(Token, Span)> {
        tokenize(src).tokens
    }

    fn parse_pat(src: &str) -> Result<Pattern, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        parse_pattern(&mut cur)
    }

    // ── wildcard ────────────────────────────────────────────────────────

    #[test]
    fn parse_pattern_wildcard() {
        let result = parse_pat("_");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(
            matches!(result, Ok(Pattern::Wildcard { .. })),
            "expected Wildcard, got {result:?}"
        );
    }

    // ── literal integer ─────────────────────────────────────────────────

    #[test]
    fn parse_pattern_literal_int() {
        let result = parse_pat("42");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(
            matches!(
                result,
                Ok(Pattern::Literal {
                    lit: Literal::IntDec { ref raw, .. },
                    ..
                }) if raw == "42"
            ),
            "expected Literal::IntDec(42), got {result:?}"
        );
    }

    // ── literal text ────────────────────────────────────────────────────

    #[test]
    fn parse_pattern_literal_text() {
        let result = parse_pat("\"hi\"");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(
            matches!(
                result,
                Ok(Pattern::Literal {
                    lit: Literal::Text { ref raw, .. },
                    ..
                }) if raw == "hi"
            ),
            "expected Literal::Text(hi), got {result:?}"
        );
    }

    // ── variable ────────────────────────────────────────────────────────

    #[test]
    fn parse_pattern_var() {
        let result = parse_pat("x");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Var { name, .. }) = result {
            assert_eq!(name.text, "x");
        } else {
            unreachable!("expected Var, got {result:?}");
        }
    }

    // ── constructor zero args ───────────────────────────────────────────

    #[test]
    fn parse_pattern_constructor_positional_zero_args() {
        let result = parse_pat("None");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Constructor {
            name, fields, args, ..
        }) = result
        {
            assert_eq!(name.text, "None");
            assert!(fields.is_none(), "expected fields=None");
            assert!(args.is_empty(), "expected no positional args");
        } else {
            unreachable!("expected Constructor, got {result:?}");
        }
    }

    // ── constructor one positional arg ──────────────────────────────────

    #[test]
    fn parse_pattern_constructor_positional_one_arg() {
        let result = parse_pat("Some x");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Constructor {
            name, fields, args, ..
        }) = result
        {
            assert_eq!(name.text, "Some");
            assert!(fields.is_none(), "expected fields=None");
            assert_eq!(args.len(), 1);
            assert!(
                matches!(&args[0], Pattern::Var { name, .. } if name.text == "x"),
                "expected Var(x) arg, got {:?}",
                args[0]
            );
        } else {
            unreachable!("expected Constructor, got {result:?}");
        }
    }

    // ── constructor record shorthand ────────────────────────────────────

    #[test]
    fn parse_pattern_constructor_record_shorthand() {
        // `User { name }` — shorthand: FieldPattern { pattern: None }
        let result = parse_pat("User { name }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Constructor {
            name, fields, args, ..
        }) = result
        {
            assert_eq!(name.text, "User");
            assert!(args.is_empty(), "expected no positional args");
            if let Some(fields) = fields {
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].name.text, "name");
                assert!(
                    fields[0].pattern.is_none(),
                    "shorthand field should have pattern=None"
                );
            } else {
                unreachable!("expected Some(fields)");
            }
        } else {
            unreachable!("expected Constructor, got {result:?}");
        }
    }

    // ── constructor record with explicit binding ────────────────────────

    #[test]
    fn parse_pattern_constructor_record_with_binding() {
        // `User { name = n, age }` — explicit + shorthand
        let result = parse_pat("User { name = n, age }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Constructor {
            name, fields, args, ..
        }) = result
        {
            assert_eq!(name.text, "User");
            assert!(args.is_empty());
            if let Some(fields) = fields {
                assert_eq!(fields.len(), 2);
                // First field: explicit binding `name = n`
                assert_eq!(fields[0].name.text, "name");
                assert!(
                    matches!(&fields[0].pattern, Some(Pattern::Var { name, .. }) if name.text == "n"),
                    "expected explicit binding to Var(n), got {:?}",
                    fields[0].pattern
                );
                // Second field: shorthand `age`
                assert_eq!(fields[1].name.text, "age");
                assert!(
                    fields[1].pattern.is_none(),
                    "shorthand field should have pattern=None"
                );
            } else {
                unreachable!("expected Some(fields)");
            }
        } else {
            unreachable!("expected Constructor, got {result:?}");
        }
    }

    // ── tuple ───────────────────────────────────────────────────────────

    #[test]
    fn parse_pattern_tuple() {
        let result = parse_pat("(x, y)");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Tuple { elems, .. }) = result {
            assert_eq!(elems.len(), 2);
            assert!(matches!(&elems[0], Pattern::Var { name, .. } if name.text == "x"));
            assert!(matches!(&elems[1], Pattern::Var { name, .. } if name.text == "y"));
        } else {
            unreachable!("expected Tuple, got {result:?}");
        }
    }

    // ── paren ──────────────────────────────────────────────────────────

    #[test]
    fn parse_pattern_paren() {
        // `(x)` → Paren { inner: Var("x") }  NOT a tuple
        let result = parse_pat("(x)");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Paren { inner, .. }) = result {
            assert!(
                matches!(inner.as_ref(), Pattern::Var { name, .. } if name.text == "x"),
                "expected Var(x) inside Paren, got {inner:?}"
            );
        } else {
            unreachable!("expected Paren, got {result:?}");
        }
    }

    // ── right-associative cons ─────────────────────────────────────────

    #[test]
    fn parse_pattern_cons_right_assoc() {
        // `a :: b :: rest` must produce:
        // Cons { head: Var(a), tail: Cons { head: Var(b), tail: Var(rest) } }
        let result = parse_pat("a :: b :: rest");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Cons { head, tail, .. }) = result {
            // head must be Var(a)
            assert!(
                matches!(head.as_ref(), Pattern::Var { name, .. } if name.text == "a"),
                "expected head=Var(a), got {head:?}"
            );
            // tail must itself be Cons { Var(b), Var(rest) }
            if let Pattern::Cons {
                head: b,
                tail: rest,
                ..
            } = tail.as_ref()
            {
                assert!(
                    matches!(b.as_ref(), Pattern::Var { name, .. } if name.text == "b"),
                    "expected inner head=Var(b), got {b:?}"
                );
                assert!(
                    matches!(rest.as_ref(), Pattern::Var { name, .. } if name.text == "rest"),
                    "expected inner tail=Var(rest), got {rest:?}"
                );
            } else {
                unreachable!("expected Cons as tail, got {tail:?}");
            }
        } else {
            unreachable!("expected Cons, got {result:?}");
        }
    }

    // ── `@` alias pattern ─────────────────────────────────────────────

    #[test]
    fn parse_pattern_at() {
        // `admin @ User { role = Admin }` → As { name: admin, inner: Constructor }
        let result = parse_pat("admin @ User { role = Admin }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::As { name, inner, .. }) = result {
            assert_eq!(name.text, "admin");
            if let Pattern::Constructor {
                name: cname,
                fields,
                args,
                ..
            } = inner.as_ref()
            {
                assert_eq!(cname.text, "User");
                assert!(args.is_empty());
                if let Some(fields) = fields {
                    assert_eq!(fields.len(), 1);
                    assert_eq!(fields[0].name.text, "role");
                    assert!(
                        matches!(
                            &fields[0].pattern,
                            Some(Pattern::Constructor { name, fields: None, args, .. })
                            if name.text == "Admin" && args.is_empty()
                        ),
                        "expected Constructor(Admin) in field pattern, got {:?}",
                        fields[0].pattern
                    );
                } else {
                    unreachable!("expected Some(fields) for User record pattern");
                }
            } else {
                unreachable!("expected Constructor inside As, got {inner:?}");
            }
        } else {
            unreachable!("expected As, got {result:?}");
        }
    }

    // ── inline record pattern — shorthand field ───────────────────────────

    #[test]
    fn parse_pattern_inline_record_shorthand() {
        // `{ name }` → Pattern::Record { fields: [FieldPattern { name: "name", pattern: None }], has_rest: false }
        let result = parse_pat("{ name }");
        assert!(
            result.is_ok(),
            "expected Ok for inline record pattern, got {result:?}"
        );
        if let Ok(Pattern::Record {
            fields, has_rest, ..
        }) = result
        {
            assert!(!has_rest);
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name.text, "name");
            assert!(fields[0].pattern.is_none());
        } else {
            panic!("expected Pattern::Record, got {result:?}");
        }
    }

    // ── inline record pattern — has_rest ────────────────────────────────────

    #[test]
    fn parse_pattern_inline_record_has_rest() {
        // `{ name, .. }` → Pattern::Record { has_rest: true }
        let result = parse_pat("{ name, .. }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Record {
            fields, has_rest, ..
        }) = result
        {
            assert!(has_rest, "expected has_rest = true");
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name.text, "name");
        } else {
            panic!("expected Pattern::Record, got {result:?}");
        }
    }

    // ── inline record pattern — empty ────────────────────────────────────────

    #[test]
    fn parse_pattern_inline_record_empty() {
        let result = parse_pat("{}");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Record {
            fields, has_rest, ..
        }) = result
        {
            assert!(fields.is_empty());
            assert!(!has_rest);
        } else {
            panic!("expected Pattern::Record, got {result:?}");
        }
    }

    // ── inline record pattern — rest-only ────────────────────────────────────

    #[test]
    fn parse_pattern_inline_record_rest_only() {
        let result = parse_pat("{ .. }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Record {
            fields, has_rest, ..
        }) = result
        {
            assert!(fields.is_empty());
            assert!(has_rest, "expected has_rest = true");
        } else {
            panic!("expected Pattern::Record, got {result:?}");
        }
    }

    // ── inline record pattern — explicit binding ──────────────────────────────

    #[test]
    fn parse_pattern_inline_record_explicit() {
        // `{ name = "Ada", age }` — explicit + shorthand
        let result = parse_pat("{ name = \"Ada\", age }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Record {
            fields, has_rest, ..
        }) = result
        {
            assert!(!has_rest);
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].name.text, "name");
            assert!(fields[0].pattern.is_some()); // explicit
            assert_eq!(fields[1].name.text, "age");
            assert!(fields[1].pattern.is_none()); // shorthand
        } else {
            panic!("expected Pattern::Record, got {result:?}");
        }
    }

    // ── `@` without leading var → P001 ────────────────────────────────

    #[test]
    fn parse_pattern_at_without_var() {
        // `42 @ x` — `42` is a literal, not a Var; `@` should fail.
        let result = parse_pat("42 @ x");
        assert!(result.is_err(), "expected Err for `42 @ x`");
        if let Err(e) = result {
            assert!(
                e.code() == "P001" || e.code() == "P002",
                "expected P001 or P002, got code={} err={e:?}",
                e.code()
            );
        }
    }

    // ── missing `}` in record pattern → P001 ───────────────────────────

    #[test]
    fn parse_pattern_missing_rbrace() {
        // `User { name` — no closing `}` → P001
        let result = parse_pat("User { name");
        assert!(
            result.is_err(),
            "expected Err for unterminated record pattern"
        );
        if let Err(e) = result {
            assert_eq!(
                e.code(),
                "P001",
                "expected P001 (missing `}}`), got code={} err={e:?}",
                e.code()
            );
        }
    }

    // ── Span coverage ─────────────────────────────────────────────────────────

    #[test]
    fn parse_pattern_cons_span_covers_full_input() {
        // `x :: xs` — span should cover all 7 bytes.
        let result = parse_pat("x :: xs");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(pat) = result {
            assert!(
                !pat.span().is_empty(),
                "expected non-empty span for `x :: xs`"
            );
        }
    }

    #[test]
    fn parse_pattern_tuple_three_elements() {
        // `(a, b, c)` → Tuple with 3 elements
        let result = parse_pat("(a, b, c)");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Tuple { elems, .. }) = result {
            assert_eq!(elems.len(), 3);
        } else {
            unreachable!("expected Tuple, got {result:?}");
        }
    }

    #[test]
    fn parse_pattern_constructor_two_args() {
        // `Ok result` → Constructor { name: Ok, args: [Var(result)] }
        let result = parse_pat("Ok result");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Constructor {
            name, args, fields, ..
        }) = result
        {
            assert_eq!(name.text, "Ok");
            assert!(fields.is_none());
            assert_eq!(args.len(), 1);
            assert!(matches!(&args[0], Pattern::Var { name, .. } if name.text == "result"));
        } else {
            unreachable!("expected Constructor, got {result:?}");
        }
    }

    // ── list patterns ─────────────────────────────────────────────────────────

    #[test]
    fn parse_pattern_list_empty() {
        // `[]` → ListNil
        let result = parse_pat("[]");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(
            matches!(result, Ok(Pattern::ListNil { .. })),
            "expected ListNil, got {result:?}"
        );
    }

    #[test]
    fn parse_pattern_list_three_elements() {
        // `[a, b, c]` → List with 3 Elem entries
        let result = parse_pat("[a, b, c]");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::List { elements, .. }) = result {
            assert_eq!(elements.len(), 3);
            for (i, elem) in elements.iter().enumerate() {
                let expected = ["a", "b", "c"][i];
                assert!(
                    matches!(elem, ridge_ast::pattern::ListPatElem::Elem(Pattern::Var { name, .. }) if name.text == expected),
                    "element {i} expected Elem(Var({expected})), got {elem:?}"
                );
            }
        } else {
            unreachable!("expected List, got {result:?}");
        }
    }

    #[test]
    fn parse_pattern_list_bare_rest() {
        // `[a, ..]` → List { elements: [Elem(a), Rest { bind: None }] }
        let result = parse_pat("[a, ..]");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::List { elements, .. }) = result {
            assert_eq!(elements.len(), 2);
            assert!(
                matches!(&elements[0], ridge_ast::pattern::ListPatElem::Elem(Pattern::Var { name, .. }) if name.text == "a"),
                "element 0 expected Elem(Var(a)), got {:?}",
                elements[0]
            );
            assert!(
                matches!(
                    &elements[1],
                    ridge_ast::pattern::ListPatElem::Rest { bind: None, .. }
                ),
                "element 1 expected Rest {{ bind: None }}, got {:?}",
                elements[1]
            );
        } else {
            unreachable!("expected List, got {result:?}");
        }
    }

    #[test]
    fn parse_pattern_list_bound_rest() {
        // `[a, rest @ ..]` → List { elements: [Elem(a), Rest { bind: Some(rest) }] }
        let result = parse_pat("[a, rest @ ..]");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::List { elements, .. }) = result {
            assert_eq!(elements.len(), 2);
            assert!(
                matches!(&elements[0], ridge_ast::pattern::ListPatElem::Elem(Pattern::Var { name, .. }) if name.text == "a"),
                "element 0 expected Elem(Var(a)), got {:?}",
                elements[0]
            );
            if let ridge_ast::pattern::ListPatElem::Rest {
                bind: Some(name), ..
            } = &elements[1]
            {
                assert_eq!(name.text, "rest");
            } else {
                unreachable!(
                    "element 1 expected Rest {{ bind: Some(rest) }}, got {:?}",
                    elements[1]
                );
            }
        } else {
            unreachable!("expected List, got {result:?}");
        }
    }

    #[test]
    fn parse_pattern_list_multi_bare_rest() {
        // `[a, b, ..]` → List with two Elem entries and a trailing Rest
        let result = parse_pat("[a, b, ..]");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::List { elements, .. }) = result {
            assert_eq!(elements.len(), 3);
            assert!(
                matches!(
                    &elements[2],
                    ridge_ast::pattern::ListPatElem::Rest { bind: None, .. }
                ),
                "element 2 expected Rest {{ bind: None }}, got {:?}",
                elements[2]
            );
        } else {
            unreachable!("expected List, got {result:?}");
        }
    }

    #[test]
    fn parse_pattern_list_suffix_rest_parses_ok() {
        // `[.., last]` is now supported — suffix rest parses successfully.
        let result = parse_pat("[.., last]");
        assert!(
            result.is_ok(),
            "expected Ok for suffix rest `[.., last]`, got {result:?}"
        );
        if let Ok(Pattern::List { elements, .. }) = result {
            assert_eq!(
                elements.len(),
                2,
                "expected 2 elements: Rest and Elem(last)"
            );
            assert!(
                matches!(
                    &elements[0],
                    ridge_ast::pattern::ListPatElem::Rest { bind: None, .. }
                ),
                "element 0 expected Rest {{ bind: None }}, got {:?}",
                elements[0]
            );
            assert!(
                matches!(&elements[1], ridge_ast::pattern::ListPatElem::Elem(Pattern::Var { name, .. }) if name.text == "last"),
                "element 1 expected Elem(Var(last)), got {:?}",
                elements[1]
            );
        } else {
            unreachable!("expected List, got {result:?}");
        }
    }

    #[test]
    fn parse_pattern_list_middle_rest_parses_ok() {
        // `[a, .., z]` is now supported — middle rest parses successfully.
        let result = parse_pat("[a, .., z]");
        assert!(
            result.is_ok(),
            "expected Ok for middle rest `[a, .., z]`, got {result:?}"
        );
        if let Ok(Pattern::List { elements, .. }) = result {
            assert_eq!(elements.len(), 3, "expected 3 elements");
            assert!(
                matches!(&elements[0], ridge_ast::pattern::ListPatElem::Elem(Pattern::Var { name, .. }) if name.text == "a"),
                "element 0 expected Elem(Var(a)), got {:?}",
                elements[0]
            );
            assert!(
                matches!(
                    &elements[1],
                    ridge_ast::pattern::ListPatElem::Rest { bind: None, .. }
                ),
                "element 1 expected Rest, got {:?}",
                elements[1]
            );
            assert!(
                matches!(&elements[2], ridge_ast::pattern::ListPatElem::Elem(Pattern::Var { name, .. }) if name.text == "z"),
                "element 2 expected Elem(Var(z)), got {:?}",
                elements[2]
            );
        } else {
            unreachable!("expected List, got {result:?}");
        }
    }

    #[test]
    fn parse_pattern_list_middle_bound_rest_parses_ok() {
        // `[a, mid @ .., z]` — middle rest with binding.
        let result = parse_pat("[a, mid @ .., z]");
        assert!(
            result.is_ok(),
            "expected Ok for `[a, mid @ .., z]`, got {result:?}"
        );
        if let Ok(Pattern::List { elements, .. }) = result {
            assert_eq!(elements.len(), 3, "expected 3 elements");
            assert!(
                matches!(&elements[1], ridge_ast::pattern::ListPatElem::Rest { bind: Some(name), .. } if name.text == "mid"),
                "element 1 expected Rest {{ bind: Some(mid) }}, got {:?}",
                elements[1]
            );
        } else {
            unreachable!("expected List, got {result:?}");
        }
    }

    #[test]
    fn parse_pattern_list_multiple_rests_yields_p024() {
        // `[.., ..]` — two rests → P024
        let result = parse_pat("[.., ..]");
        assert!(result.is_err(), "expected Err for two rests");
        if let Err(e) = result {
            assert_eq!(
                e.code(),
                "P024",
                "expected P024 MultipleRestInListPattern, got code={} err={e:?}",
                e.code()
            );
        }
    }

    // ── record rest pattern ───────────────────────────────────────────────────

    #[test]
    fn parse_pattern_record_rest_has_rest_true() {
        // `User { name, .. }` → Constructor { fields: Some([name]), has_rest: true }
        let result = parse_pat("User { name, .. }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Constructor {
            name,
            fields,
            has_rest,
            ..
        }) = result
        {
            assert_eq!(name.text, "User");
            assert!(has_rest, "expected has_rest = true");
            assert!(fields.is_some(), "expected fields = Some(...)");
            if let Some(fields) = fields {
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].name.text, "name");
            }
        } else {
            unreachable!("expected Constructor, got {result:?}");
        }
    }

    #[test]
    fn parse_pattern_record_no_rest_has_rest_false() {
        // `User { name }` → Constructor { fields: Some([name]), has_rest: false }
        let result = parse_pat("User { name }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Pattern::Constructor { has_rest, .. }) = result {
            assert!(!has_rest, "expected has_rest = false");
        } else {
            unreachable!("expected Constructor, got {result:?}");
        }
    }

    // ── desugar_list ──────────────────────────────────────────────────────────

    #[test]
    fn desugar_list_empty_gives_list_nil() {
        let pat = Pattern::List {
            elements: vec![],
            span: Span::point(0),
        };
        let desugared = pat.desugar_list();
        assert!(
            matches!(desugared, Pattern::ListNil { .. }),
            "expected ListNil, got {desugared:?}"
        );
    }

    #[test]
    fn desugar_list_single_element_gives_cons_nil() {
        let pat = Pattern::List {
            elements: vec![ridge_ast::pattern::ListPatElem::Elem(Pattern::Var {
                name: ridge_ast::Ident::new("x", Span::point(0)),
                span: Span::point(0),
            })],
            span: Span::point(0),
        };
        let desugared = pat.desugar_list();
        if let Pattern::Cons { head, tail, .. } = desugared {
            assert!(matches!(*head, Pattern::Var { name, .. } if name.text == "x"));
            assert!(matches!(*tail, Pattern::ListNil { .. }));
        } else {
            unreachable!("expected Cons, got {desugared:?}");
        }
    }

    #[test]
    fn desugar_list_prefix_rest_no_bind_gives_cons_wildcard() {
        // `[a, ..]` desugars to `Cons(a, Wildcard)`
        let pat = Pattern::List {
            elements: vec![
                ridge_ast::pattern::ListPatElem::Elem(Pattern::Var {
                    name: ridge_ast::Ident::new("a", Span::point(0)),
                    span: Span::point(0),
                }),
                ridge_ast::pattern::ListPatElem::Rest {
                    bind: None,
                    span: Span::point(2),
                },
            ],
            span: Span::point(0),
        };
        let desugared = pat.desugar_list();
        if let Pattern::Cons { head, tail, .. } = desugared {
            assert!(matches!(*head, Pattern::Var { name, .. } if name.text == "a"));
            assert!(matches!(*tail, Pattern::Wildcard { .. }));
        } else {
            unreachable!("expected Cons, got {desugared:?}");
        }
    }

    #[test]
    fn desugar_list_prefix_rest_bound_gives_cons_var() {
        // `[a, rest @ ..]` desugars to `Cons(a, Var(rest))`
        let pat = Pattern::List {
            elements: vec![
                ridge_ast::pattern::ListPatElem::Elem(Pattern::Var {
                    name: ridge_ast::Ident::new("a", Span::point(0)),
                    span: Span::point(0),
                }),
                ridge_ast::pattern::ListPatElem::Rest {
                    bind: Some(ridge_ast::Ident::new("rest", Span::point(2))),
                    span: Span::point(2),
                },
            ],
            span: Span::point(0),
        };
        let desugared = pat.desugar_list();
        if let Pattern::Cons { head, tail, .. } = desugared {
            assert!(matches!(*head, Pattern::Var { name, .. } if name.text == "a"));
            assert!(matches!(*tail, Pattern::Var { name, .. } if name.text == "rest"));
        } else {
            unreachable!("expected Cons, got {desugared:?}");
        }
    }
}
