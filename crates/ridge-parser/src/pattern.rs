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
//! # P018 — Bare record pattern
//!
//! A bare `{` in pattern position (without a leading `UPPER_IDENT`) is
//! rejected with [`ParseError::BareRecordPattern`] (P018).

// These functions are called by tests in this file.  They will be called from
// production code (match/let/lambda).  Suppress dead_code until then.
#![allow(dead_code)]
#![allow(clippy::redundant_pub_crate)]

use ridge_ast::{FieldPattern, Ident, Pattern};
use ridge_lexer::Token;

use crate::{cursor::Cursor, error::ParseError, expr::parse_literal};

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
/// | `{` | `P018 BareRecordPattern` |
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

        // ── Empty-list pattern `[]` ───────────────────────────────────────────
        Token::LBrack => {
            cur.bump(); // consume `[`
            let end_span = cur.expect(&Token::RBrack)?;
            Ok(Pattern::ListNil {
                span: span.merge(end_span),
            })
        }

        // ── Bare record pattern `{ … }` — P018 error ─────────────────────────
        Token::LBrace => Err(ParseError::BareRecordPattern { span }),

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
        let fields = parse_field_pattern_list(cur)?;
        let rbrace_span = cur.expect(&Token::RBrace)?;
        end_span = rbrace_span;
        return Ok(Pattern::Constructor {
            name,
            fields: Some(fields),
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
        args,
        span: start_span.merge(end_span),
    })
}

// ── parse_field_pattern_list (internal) ──────────────────────────────────────

/// Parse `FieldPattern { "," FieldPattern } [ "," ]` inside `{ … }`.
///
/// Precondition: the opening `{` has already been consumed.
/// The closing `}` is NOT consumed here; the caller handles it.
fn parse_field_pattern_list(cur: &mut Cursor<'_>) -> Result<Vec<FieldPattern>, ParseError> {
    let mut fields: Vec<FieldPattern> = Vec::new();

    // Empty record body `{}` is allowed (edge case).
    if cur.peek() == &Token::RBrace {
        return Ok(fields);
    }

    loop {
        let field = parse_field_pattern(cur)?;
        fields.push(field);

        if cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
                        // Trailing comma: if next is `}`, stop.
            if cur.peek() == &Token::RBrace {
                break;
            }
            // Otherwise continue to the next field.
        } else {
            // No comma: must be followed by `}`.
            break;
        }
    }

    Ok(fields)
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

    // ── bare record pattern → P018 ─────────────────────────────────────

    #[test]
    fn parse_pattern_bare_record_rejects_with_p018() {
        let result = parse_pat("{ name }");
        assert!(result.is_err(), "expected Err for bare record pattern");
        if let Err(e) = result {
            assert_eq!(
                e.code(),
                "P018",
                "expected P018 BareRecordPattern, got code={} err={e:?}",
                e.code()
            );
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
}
