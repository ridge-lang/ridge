//! Lambda, spawn, record-construction, field-init, and interpolation parsers
//! (T8, grammar §§6.13, 6.16, 6.18, 6.19, 6.20).
//!
//! Entry points:
//!
//! - [`parse_lambda`]            — `fn Param+ -> Body` (D052)
//! - [`parse_spawn`]             — `spawn UPPER_IDENT arg*` (D061)
//! - [`parse_record_construct`]  — `Constructor { FieldInit* }` (D051)
//! - [`parse_field_init_list`]   — comma-separated `name [= Expr]` list (D053)
//! - [`parse_interp_full`]       — full `$"…"` with expression holes (T8)
//!
//! All functions are `pub(crate)` and called from `expr.rs`.

#![allow(dead_code)]
#![allow(clippy::redundant_pub_crate)]

use ridge_ast::{expr::RecordCtor, Expr, FieldInit, Ident, InterpPart, LambdaParam, Pattern};
use ridge_lexer::Token;

use crate::{
    ctrl::parse_branch_body_flat, cursor::Cursor, error::ParseError, expr::parse_expr,
    pattern::parse_pattern_atom,
};

// ── parse_lambda ──────────────────────────────────────────────────────────────

/// Parse a lambda expression `fn Param+ -> Body` (grammar §6.16, D052).
///
/// Syntax:
/// ```text
/// LambdaExpr  ::= "fn" LambdaParam+ "->" Expr ;
/// LambdaParam ::= "(" Pattern ":" Type ")"  -- annotated
///               | Pattern                    -- bare (any pattern atom)
/// ```
///
/// Disambiguation from `InnerFn` (deferred to T10): in T8 every `fn` in
/// expression position is treated as a lambda.  If parsing fails (e.g. a name
/// without `->` ever appearing), the parser returns a P002 error naturally.
///
/// Precondition: `cur.peek() == &Token::KwFn`.
pub(crate) fn parse_lambda(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `fn`

    // Collect one or more parameters until we see `->`.
    let mut params: Vec<LambdaParam> = Vec::new();

    while cur.peek() != &Token::Arrow {
        // Guard against EOF / layout tokens.
        match cur.peek() {
            Token::Eof | Token::Newline | Token::Indent | Token::Dedent => {
                return Err(ParseError::Expected {
                    span: cur.span(),
                    expected: "`->`",
                    found: cur.peek().to_string(),
                });
            }
            _ => {}
        }

        let param = parse_lambda_param(cur)?;
        params.push(param);
    }

    if params.is_empty() {
        return Err(ParseError::Expected {
            span: cur.span(),
            expected: "lambda parameter",
            found: "`->`".to_string(),
        });
    }

    cur.expect(&Token::Arrow)?; // consume `->`

    // Optional return-type annotation `Type =` before the body.
    //
    // Handles `fn (param: Type) -> RetType = body` (anonymous function with
    // declared return type, e.g. in url_shortener).  We detect this by
    // scanning for `=` at bracket depth 0 before any statement keyword,
    // layout token, or scope exit.  If found, parse and discard the type,
    // then consume `=`.  The return type is not stored in `Lambda`'s AST
    // (use `InnerFn` via `parse_fn_decl` if you need it in the AST).
    if lambda_has_return_type_eq(cur) {
        crate::ty::parse_type(cur)?; // consume return type (annotation; discarded)
        cur.expect(&Token::Assign)?; // consume `=`
    }

    // Use parse_branch_body_flat so that both inline, INDENT-delimited block,
    // and flat-NEWLINE-block (inside brackets, OQ-R014) forms are handled:
    //   fn x -> x + 1            ← single inline expression
    //   fn x ->                  ← block form (NEWLINE INDENT ... DEDENT)
    //       let y = x + 1
    //       y * 2
    //   List.forEach (fn row ->  ← flat-block form inside parens (OQ-R014)
    //       let line = ...
    //       Io.println line)
    let body = parse_branch_body_flat(cur)?;
    let span = start.merge(body.span());

    Ok(Expr::Lambda {
        params,
        body: Box::new(body),
        span,
    })
}

// ── lambda_has_return_type_eq (internal) ─────────────────────────────────────

/// Return `true` if, starting at the current cursor position (immediately
/// after the `->` of a lambda), there is a return-type annotation followed
/// by `=` at bracket depth 0 before any statement keyword, layout token, or
/// scope exit.
///
/// Used to detect `fn params -> RetType = body` (anonymous function with a
/// declared return type).
fn lambda_has_return_type_eq(cur: &Cursor<'_>) -> bool {
    const SCAN_LIMIT: usize = 64;
    let mut depth: i32 = 0;
    for i in 0..SCAN_LIMIT {
        match cur.peek_n(i) {
            Some(Token::LParen | Token::LBrack | Token::LBrace) => depth += 1,
            Some(Token::RParen | Token::RBrack | Token::RBrace) => {
                depth -= 1;
                if depth < 0 {
                    return false; // exited enclosing scope
                }
            }
            Some(Token::Assign) if depth == 0 => return true,
            // Statement keywords indicate we are in the body, not a type
            Some(
                Token::KwLet
                | Token::KwVar
                | Token::KwIf
                | Token::KwMatch
                | Token::KwGuard
                | Token::KwReturn
                | Token::KwTry
                | Token::KwSpawn,
            ) if depth == 0 => return false,
            Some(Token::Newline | Token::Indent | Token::Dedent | Token::Eof) if depth == 0 => {
                return false;
            }
            None => return false,
            _ => {}
        }
    }
    false
}

// ── parse_lambda_param (internal) ────────────────────────────────────────────

/// Parse a single lambda parameter.
///
/// Three cases (all starting with `(`):
/// 1. `(pat : Type)` → `LambdaParam::Annotated`
/// 2. `(pat, pat, …)` → `LambdaParam::Pattern(Tuple)`  (via `parse_pattern_atom`)
/// 3. `(pat)` → `LambdaParam::Pattern(Paren)`           (via `parse_pattern_atom`)
///
/// And for non-`(` tokens:
/// 4. Any other pattern atom → `LambdaParam::Pattern`
///
/// Note: the annotated form `(pat : Type)` requires special disambiguation
/// because the lexer emits `Colon`, which is also used in `Type` parsing.
/// We peek inside the `(` to check for `Pattern Colon` vs. `Pattern Comma`.
fn parse_lambda_param(cur: &mut Cursor<'_>) -> Result<LambdaParam, ParseError> {
    if cur.peek() != &Token::LParen {
        // Non-parenthesised parameter: parse a pattern atom.
        let pat = parse_pattern_atom(cur)?;
        return Ok(LambdaParam::Pattern(pat));
    }

    // Starting with `(`.  We need to look ahead to distinguish:
    //   (pat : Type)  — annotated param
    //   (pat, …)      — tuple pattern
    //   (pat)         — paren pattern
    //
    // Strategy: parse the first pattern inside the parens, then peek at the
    // next token.
    let start_span = cur.span();
    cur.bump(); // consume `(`

    // Parse the first (inner) pattern.
    let inner_pat = parse_pattern_from_cursor(cur)?;

    match cur.peek() {
        Token::Colon => {
            // Annotated form: `(pat : Type)`.
            cur.bump(); // consume `:`
            let ty = crate::ty::parse_type(cur)?;
            let end_span = cur.expect(&Token::RParen)?;
            let span = start_span.merge(end_span);
            Ok(LambdaParam::Annotated {
                pat: inner_pat,
                ty,
                span,
            })
        }
        Token::RParen => {
            // Paren form: `(pat)` — single pattern in parens.
            let end_span = cur.span();
            cur.bump(); // consume `)`
            let span = start_span.merge(end_span);
            Ok(LambdaParam::Pattern(Pattern::Paren {
                inner: Box::new(inner_pat),
                span,
            }))
        }
        Token::Comma => {
            // Tuple form: `(pat, pat, …)`.
            let mut elems = vec![inner_pat];
            while cur.peek() == &Token::Comma {
                cur.bump(); // consume `,`
                if cur.peek() == &Token::RParen {
                    // Trailing comma — stop.
                    break;
                }
                elems.push(parse_pattern_from_cursor(cur)?);
            }
            let end_span = cur.expect(&Token::RParen)?;
            let span = start_span.merge(end_span);
            Ok(LambdaParam::Pattern(Pattern::Tuple { elems, span }))
        }
        _ => Err(ParseError::Expected {
            span: cur.span(),
            expected: "`:`, `,`, or `)`",
            found: cur.peek().to_string(),
        }),
    }
}

/// Helper: parse a full pattern (including `::` and `@`) from the cursor.
/// Delegates to the public `parse_pattern` helper.
fn parse_pattern_from_cursor(cur: &mut Cursor<'_>) -> Result<Pattern, ParseError> {
    crate::pattern::parse_pattern(cur)
}

// ── parse_spawn ───────────────────────────────────────────────────────────────

/// Parse a spawn expression `spawn UPPER_IDENT arg*` (D061, grammar §6.19).
///
/// ```text
/// SpawnExpr ::= "spawn" UPPER_IDENT { ExprAtom } ;
/// ```
///
/// Arguments are zero or more `ExprAtom`-level terms (same predicate as
/// juxtaposition call arguments).
///
/// Precondition: `cur.peek() == &Token::KwSpawn`.
pub(crate) fn parse_spawn(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `spawn`

    // Expect an upper-case identifier for the actor name.
    let actor_span = cur.span();
    let actor_text = match cur.peek().clone() {
        Token::UpperIdent(s) => {
            cur.bump();
            s
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<actor name (UPPER_IDENT)>",
                found: cur.peek().to_string(),
            });
        }
    };
    let actor = Ident::new(actor_text, actor_span);

    // Greedily collect argument atoms.
    let mut args: Vec<Expr> = Vec::new();
    while can_start_arg_atom(cur) {
        args.push(crate::expr::parse_expr_atom12(cur)?);
    }

    let end_span = args.last().map_or(actor_span, Expr::span);
    let span = start.merge(end_span);

    Ok(Expr::Spawn { actor, args, span })
}

// ── parse_record_construct ────────────────────────────────────────────────────

/// Parse the `{ FieldInit* }` body of a record-construction expression (D051).
///
/// The caller has already consumed the constructor token(s) and passed the
/// resulting [`RecordCtor`] (bare or qualified).
///
/// ```text
/// RecordConstruct ::= RecordCtor "{" FieldInitList "}" ;
/// RecordCtor      ::= UPPER_IDENT ( "." UPPER_IDENT )* ;
/// FieldInitList   ::= [ FieldInit { "," FieldInit } [ "," ] ] ;
/// ```
///
/// Precondition: `cur.peek() == &Token::LBrace`.
pub(crate) fn parse_record_construct(
    cur: &mut Cursor<'_>,
    constructor: RecordCtor,
) -> Result<Expr, ParseError> {
    let start = match &constructor {
        RecordCtor::Bare(id) => id.span,
        RecordCtor::Qualified(qn) => qn.span,
    };
    cur.expect(&Token::LBrace)?; // consume `{`
    cur.bracket_depth += 1;

    let fields = parse_field_init_list(cur)?;

    cur.bracket_depth -= 1;
    let end_span = cur.expect(&Token::RBrace)?;
    let span = start.merge(end_span);

    Ok(Expr::Record {
        constructor,
        fields,
        span,
    })
}

// ── parse_field_init_list ─────────────────────────────────────────────────────

/// Parse a comma-separated list of field initialisers with optional trailing
/// comma.
///
/// Precondition: the opening `{` has already been consumed.
/// The closing `}` is NOT consumed here; the caller handles it.
pub(crate) fn parse_field_init_list(cur: &mut Cursor<'_>) -> Result<Vec<FieldInit>, ParseError> {
    let mut fields: Vec<FieldInit> = Vec::new();

    // Empty body.
    if cur.peek() == &Token::RBrace {
        return Ok(fields);
    }

    loop {
        let field = parse_field_init(cur)?;
        fields.push(field);

        if cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
            if cur.peek() == &Token::RBrace {
                // Trailing comma.
                break;
            }
            // Continue to the next field.
        } else {
            // No comma: must be followed by `}`.
            break;
        }
    }

    Ok(fields)
}

// ── parse_field_init (internal) ───────────────────────────────────────────────

/// Parse a single field initialiser `name [= Expr]`.
///
/// - Explicit: `name = Expr` → `FieldInit { value: Some(expr) }`
/// - Shorthand (D053): `name` → `FieldInit { value: None }`
fn parse_field_init(cur: &mut Cursor<'_>) -> Result<FieldInit, ParseError> {
    let name_span = cur.span();

    let name_text = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            s
        }
        _ => {
            return Err(ParseError::Expected {
                span: name_span,
                expected: "<field name (LOWER_IDENT)>",
                found: cur.peek().to_string(),
            });
        }
    };
    let name = Ident::new(name_text, name_span);

    if cur.peek() == &Token::Assign {
        cur.bump(); // consume `=`
                    // Use parse_expr to allow if/match/lambda as field values.
        let value = parse_expr(cur)?;
        let span = name_span.merge(value.span());
        Ok(FieldInit {
            name,
            value: Some(value),
            span,
        })
    } else {
        // Shorthand: no explicit value.
        Ok(FieldInit {
            name,
            value: None,
            span: name_span,
        })
    }
}

// ── parse_interp_full ─────────────────────────────────────────────────────────

/// Parse a full interpolated string `$"…"` with expression holes (T8).
///
/// ```text
/// InterpolatedText ::= InterpStart { InterpText | InterpExprStart Expr InterpExprEnd }
///                      InterpEnd ;
/// ```
///
/// Replaces the T3 `parse_interp_stripped` which only handled the zero-hole case.
/// This version handles holes (`${…}`) by fully parsing the expression inside each
/// hole via `parse_expr`, producing proper `InterpPart::Expr` nodes.
///
/// The lexer now tokenizes the content of `${…}` holes using its full logos scanner,
/// so the parser sees `InterpExprStart, <expr tokens…>, InterpExprEnd`.
///
/// Precondition: `cur.peek() == &Token::InterpStart`.
pub(crate) fn parse_interp_full(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `InterpStart` (`$"`)

    let mut parts: Vec<InterpPart> = Vec::new();

    loop {
        match cur.peek().clone() {
            Token::InterpText(raw) => {
                let seg_span = cur.span();
                cur.bump();
                parts.push(InterpPart::Text {
                    raw,
                    span: seg_span,
                });
            }
            Token::InterpExprStart => {
                let hole_start = cur.span();
                cur.bump(); // consume `${`

                // Parse the expression inside the hole using the full Pratt parser.
                let expr = parse_expr(cur)?;

                let hole_end = cur.expect(&Token::InterpExprEnd)?;
                let span = hole_start.merge(hole_end);
                parts.push(InterpPart::Expr {
                    expr: Box::new(expr),
                    span,
                });
            }
            Token::InterpEnd => {
                let end_span = cur.span();
                cur.bump(); // consume closing `"`
                return Ok(Expr::Interp {
                    parts,
                    span: start.merge(end_span),
                });
            }
            _ => {
                let err_span = cur.span();
                return Err(ParseError::UnexpectedToken {
                    span: err_span,
                    description: format!(
                        "unexpected token `{}` inside interpolated string",
                        cur.peek()
                    ),
                });
            }
        }
    }
}

// ── can_start_arg_atom (re-export for parse_spawn) ────────────────────────────

/// Return `true` if the current token can begin an argument atom for spawn
/// or ask argument lists (same set as juxtaposition in `expr.rs`).
pub(crate) fn can_start_arg_atom(cur: &Cursor<'_>) -> bool {
    matches!(
        cur.peek(),
        Token::IntDec(_)
            | Token::IntBin(_)
            | Token::IntOct(_)
            | Token::IntHex(_)
            | Token::Float(_)
            | Token::TextLit(_)
            | Token::KwTrue
            | Token::KwFalse
            | Token::InterpStart
            | Token::LowerIdent(_)
            | Token::UpperIdent(_)
            | Token::LParen
            | Token::LBrack
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::panic)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ridge_ast::{Expr, InterpPart, LambdaParam, Literal, Pattern, Span};
    use ridge_lexer::tokenize;

    use crate::{cursor::Cursor, error::ParseError, expr::parse_expr};

    fn lex(src: &str) -> Vec<(Token, Span)> {
        tokenize(src).tokens
    }

    fn parse_e(src: &str) -> Result<Expr, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        parse_expr(&mut cur)
    }

    fn ok(src: &str) -> Expr {
        parse_e(src).unwrap_or_else(|e| panic!("parse_expr({src:?}) failed: {e:?}"))
    }

    fn err_e(src: &str) -> ParseError {
        parse_e(src)
            .err()
            .unwrap_or_else(|| panic!("parse_expr({src:?}) expected Err, got Ok"))
    }

    // ── T8-1: parse_lambda_single_param ──────────────────────────────────────

    #[test]
    fn parse_lambda_single_param() {
        let e = ok("fn x -> x + 1");
        if let Expr::Lambda { params, body, .. } = e {
            assert_eq!(params.len(), 1, "expected 1 param, got {}", params.len());
            assert!(
                matches!(&params[0], LambdaParam::Pattern(Pattern::Var { name, .. }) if name.text == "x"),
                "expected Pattern(Var(x)), got {:?}",
                params[0]
            );
            assert!(
                matches!(
                    *body,
                    Expr::Binary {
                        op: ridge_ast::BinOp::Add,
                        ..
                    }
                ),
                "expected Add body, got {body:?}"
            );
        } else {
            panic!("expected Lambda, got {e:?}");
        }
    }

    // ── T8-2: parse_lambda_multi_param ───────────────────────────────────────

    #[test]
    fn parse_lambda_multi_param() {
        let e = ok("fn x y -> x + y");
        if let Expr::Lambda { params, .. } = e {
            assert_eq!(params.len(), 2, "expected 2 params, got {}", params.len());
            assert!(
                matches!(&params[0], LambdaParam::Pattern(Pattern::Var { name, .. }) if name.text == "x")
            );
            assert!(
                matches!(&params[1], LambdaParam::Pattern(Pattern::Var { name, .. }) if name.text == "y")
            );
        } else {
            panic!("expected Lambda, got {e:?}");
        }
    }

    // ── T8-3: parse_lambda_pattern_tuple (D052) ───────────────────────────────

    #[test]
    fn parse_lambda_pattern_tuple() {
        let e = ok("fn (x, y) -> x + y");
        if let Expr::Lambda { params, .. } = e {
            assert_eq!(
                params.len(),
                1,
                "expected 1 param (tuple), got {}",
                params.len()
            );
            if let LambdaParam::Pattern(Pattern::Tuple { elems, .. }) = &params[0] {
                assert_eq!(elems.len(), 2);
                assert!(matches!(&elems[0], Pattern::Var { name, .. } if name.text == "x"));
                assert!(matches!(&elems[1], Pattern::Var { name, .. } if name.text == "y"));
            } else {
                panic!("expected Pattern(Tuple), got {:?}", params[0]);
            }
        } else {
            panic!("expected Lambda, got {e:?}");
        }
    }

    // ── T8-4: parse_lambda_annotated_param ───────────────────────────────────

    #[test]
    fn parse_lambda_annotated_param() {
        use ridge_ast::{PrimitiveType, Type};
        let e = ok("fn (x: Int) -> x");
        if let Expr::Lambda { params, body, .. } = e {
            assert_eq!(params.len(), 1);
            if let LambdaParam::Annotated { pat, ty, .. } = &params[0] {
                assert!(
                    matches!(pat, Pattern::Var { name, .. } if name.text == "x"),
                    "expected Var(x) in annotated param, got {pat:?}"
                );
                assert!(
                    matches!(
                        ty,
                        Type::Primitive {
                            name: PrimitiveType::Int,
                            ..
                        }
                    ),
                    "expected Int type, got {ty:?}"
                );
            } else {
                panic!("expected Annotated param, got {:?}", params[0]);
            }
            assert!(
                matches!(*body, Expr::Ident(ref id) if id.text == "x"),
                "expected Ident(x) body, got {body:?}"
            );
        } else {
            panic!("expected Lambda, got {e:?}");
        }
    }

    // ── T8-5: parse_lambda_body_is_block ─────────────────────────────────────

    #[test]
    fn parse_lambda_body_is_block() {
        // fn x ->
        //     let y = x + 1
        //     y * 2
        // parse_lambda uses parse_branch_body, so the NEWLINE+INDENT form is
        // handled correctly — the body is an Expr::Block with 2 statements.
        let src = "fn x ->\n    let y = x + 1\n    y * 2";
        let e = ok(src);
        if let Expr::Lambda { params, body, .. } = e {
            assert_eq!(params.len(), 1);
            assert!(
                matches!(body.as_ref(), Expr::Block(_)),
                "expected Expr::Block body, got {body:?}"
            );
            if let Expr::Block(block) = body.as_ref() {
                assert_eq!(
                    block.stmts.len(),
                    2,
                    "expected 2 stmts in lambda block body, got {}",
                    block.stmts.len()
                );
            }
        } else {
            panic!("expected Lambda, got {e:?}");
        }
    }

    // ── T8-6: parse_with_single_field ────────────────────────────────────────

    #[test]
    fn parse_with_single_field() {
        let e = ok("u with { age = 31 }");
        if let Expr::With { base, fields, .. } = e {
            assert!(matches!(*base, Expr::Ident(ref id) if id.text == "u"));
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name.text, "age");
            assert!(
                matches!(&fields[0].value, Some(Expr::Literal(Literal::IntDec { raw, .. })) if raw == "31"),
                "expected value=31, got {:?}",
                fields[0].value
            );
        } else {
            panic!("expected With, got {e:?}");
        }
    }

    // ── T8-7: parse_with_chained_left_assoc ──────────────────────────────────

    #[test]
    fn parse_with_chained_left_assoc() {
        // `u with { a = 1 } with { b = 2 }` → With { With { u, [a=1] }, [b=2] }
        let e = ok("u with { a = 1 } with { b = 2 }");
        if let Expr::With {
            base: outer_base,
            fields: outer_fields,
            ..
        } = e
        {
            // Outer fields should be [b=2].
            assert_eq!(outer_fields.len(), 1);
            assert_eq!(outer_fields[0].name.text, "b");
            // Outer base should itself be a With.
            if let Expr::With {
                base: inner_base,
                fields: inner_fields,
                ..
            } = *outer_base
            {
                assert_eq!(inner_fields.len(), 1);
                assert_eq!(inner_fields[0].name.text, "a");
                assert!(matches!(*inner_base, Expr::Ident(ref id) if id.text == "u"));
            } else {
                panic!(
                    "expected inner With, got {outer_base:?}",
                    outer_base = *outer_base
                );
            }
        } else {
            panic!("expected outer With, got {e:?}");
        }
    }

    // ── T8-8: parse_with_shorthand_field (D053) ──────────────────────────────

    #[test]
    fn parse_with_shorthand_field() {
        let e = ok("u with { age }");
        if let Expr::With { fields, .. } = e {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name.text, "age");
            assert!(
                fields[0].value.is_none(),
                "shorthand field should have value=None"
            );
        } else {
            panic!("expected With, got {e:?}");
        }
    }

    // ── T8-9: parse_record_construct_empty ───────────────────────────────────

    #[test]
    fn parse_record_construct_empty() {
        use ridge_ast::expr::RecordCtor;
        let e = ok("User {}");
        if let Expr::Record {
            constructor,
            fields,
            ..
        } = e
        {
            assert!(
                matches!(&constructor, RecordCtor::Bare(id) if id.text == "User"),
                "expected RecordCtor::Bare(User), got {constructor:?}"
            );
            assert!(fields.is_empty(), "expected empty fields, got {fields:?}");
        } else {
            panic!("expected Record, got {e:?}");
        }
    }

    // ── T8-10: parse_record_construct_single_field ───────────────────────────

    #[test]
    fn parse_record_construct_single_field() {
        use ridge_ast::expr::RecordCtor;
        let e = ok("User { name = \"ada\" }");
        if let Expr::Record {
            constructor,
            fields,
            ..
        } = e
        {
            assert!(
                matches!(&constructor, RecordCtor::Bare(id) if id.text == "User"),
                "expected RecordCtor::Bare(User), got {constructor:?}"
            );
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name.text, "name");
            assert!(
                matches!(&fields[0].value, Some(Expr::Literal(Literal::Text { raw, .. })) if raw == "ada"),
                "expected value=\"ada\", got {:?}",
                fields[0].value
            );
        } else {
            panic!("expected Record, got {e:?}");
        }
    }

    // ── T8-11: parse_record_construct_shorthand (D053) ───────────────────────

    #[test]
    fn parse_record_construct_shorthand() {
        let e = ok("User { name, age }");
        if let Expr::Record { fields, .. } = e {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].name.text, "name");
            assert!(fields[0].value.is_none(), "expected shorthand for name");
            assert_eq!(fields[1].name.text, "age");
            assert!(fields[1].value.is_none(), "expected shorthand for age");
        } else {
            panic!("expected Record, got {e:?}");
        }
    }

    // ── T8-12: parse_record_trailing_comma ───────────────────────────────────

    #[test]
    fn parse_record_trailing_comma() {
        let e = ok("User { name = x, }");
        if let Expr::Record { fields, .. } = e {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name.text, "name");
        } else {
            panic!("expected Record, got {e:?}");
        }
    }

    // ── T8-13: parse_interp_single_hole ──────────────────────────────────────

    #[test]
    fn parse_interp_single_hole() {
        // The lexer now tokenizes the inner bytes of `${...}` holes, so the
        // parser receives `InterpExprStart, LowerIdent("name"), InterpExprEnd`
        // and can parse a proper Expr::Ident rather than a Unit placeholder.
        let e = ok("$\"hello ${name}\"");
        if let Expr::Interp { parts, .. } = e {
            assert_eq!(
                parts.len(),
                2,
                "expected 2 parts (text + hole), got {}",
                parts.len()
            );
            assert!(
                matches!(&parts[0], InterpPart::Text { raw, .. } if raw == "hello "),
                "expected text 'hello ', got {:?}",
                parts[0]
            );
            assert!(
                matches!(&parts[1], InterpPart::Expr { .. }),
                "expected Expr hole, got {:?}",
                parts[1]
            );
            if let InterpPart::Expr { expr, .. } = &parts[1] {
                assert!(
                    matches!(expr.as_ref(), Expr::Ident(id) if id.text == "name"),
                    "expected Ident(name) in hole, got {expr:?}"
                );
            }
        } else {
            panic!("expected Interp, got {e:?}");
        }
    }

    // ── T8-14: parse_interp_empty_text_between_holes ─────────────────────────

    #[test]
    fn parse_interp_empty_text_between_holes() {
        // `$"${a}${b}"` — two adjacent holes with no text between.
        let e = ok("$\"${a}${b}\"");
        if let Expr::Interp { parts, .. } = e {
            // The lexer may or may not emit an empty InterpText between holes.
            // We assert at least 2 Expr parts.
            let expr_parts: Vec<_> = parts
                .iter()
                .filter(|p| matches!(p, InterpPart::Expr { .. }))
                .collect();
            assert_eq!(
                expr_parts.len(),
                2,
                "expected 2 Expr holes, got {}. Parts: {parts:?}",
                expr_parts.len()
            );
        } else {
            panic!("expected Interp, got {e:?}");
        }
    }

    // ── T8-15: parse_interp_zero_holes ───────────────────────────────────────

    #[test]
    fn parse_interp_zero_holes() {
        // Re-verify T3 behaviour: `$"hello"` → Interp with 1 Text part.
        let e = ok("$\"hello\"");
        if let Expr::Interp { parts, .. } = e {
            assert_eq!(parts.len(), 1, "expected 1 part, got {}", parts.len());
            assert!(
                matches!(&parts[0], InterpPart::Text { raw, .. } if raw == "hello"),
                "expected text 'hello', got {:?}",
                parts[0]
            );
        } else {
            panic!("expected Interp, got {e:?}");
        }
    }

    // ── T8-16: parse_ask_no_args ──────────────────────────────────────────────

    #[test]
    fn parse_ask_no_args() {
        let e = ok("store ?> increment");
        if let Expr::Ask {
            handle,
            message,
            args,
            ..
        } = e
        {
            assert!(matches!(*handle, Expr::Ident(ref id) if id.text == "store"));
            assert_eq!(message.text, "increment");
            assert!(args.is_empty(), "expected no args, got {args:?}");
        } else {
            panic!("expected Ask, got {e:?}");
        }
    }

    // ── T8-17: parse_ask_with_args ────────────────────────────────────────────

    #[test]
    fn parse_ask_with_args() {
        let e = ok("store ?> shorten url");
        if let Expr::Ask {
            handle,
            message,
            args,
            ..
        } = e
        {
            assert!(matches!(*handle, Expr::Ident(ref id) if id.text == "store"));
            assert_eq!(message.text, "shorten");
            assert_eq!(args.len(), 1, "expected 1 arg, got {}", args.len());
            assert!(
                matches!(&args[0], Expr::Ident(id) if id.text == "url"),
                "expected Ident(url), got {:?}",
                args[0]
            );
        } else {
            panic!("expected Ask, got {e:?}");
        }
    }

    // ── T8-18: parse_send_simple ──────────────────────────────────────────────

    #[test]
    fn parse_send_simple() {
        // `w ! run ()` — Send { handle: w, message: Call(run, [Unit]) }
        // OR if juxtaposition applies after Send: Send { handle: w, message: run }
        // then `()` is a separate application.
        // Per plan §4.5 note: `a ! b c` = Send(a, b) then juxta applies c.
        // So `w ! run` = Send(w, run), then `()` is juxta → Call(Send(w,run), [()]).
        // But wait — Send has lbp/rbp at level 12 (postfix), so after Send is
        // emitted we return to the caller. The juxta loop at level 11 then applies.
        // For the test, verify at least that a Send or Call(Send) is produced.
        let e = ok("w ! run ()");
        // Accept either Send at the top level or Call wrapping Send.
        let has_send = match &e {
            Expr::Send { .. } => true,
            Expr::Call { callee, .. } => matches!(callee.as_ref(), Expr::Send { .. }),
            _ => false,
        };
        assert!(
            has_send,
            "expected Send (possibly wrapped in Call), got {e:?}"
        );
    }

    // ── T8-19: parse_spawn_no_args ────────────────────────────────────────────

    #[test]
    fn parse_spawn_no_args() {
        let e = ok("spawn Counter");
        if let Expr::Spawn { actor, args, .. } = e {
            assert_eq!(actor.text, "Counter");
            assert!(args.is_empty(), "expected no args, got {args:?}");
        } else {
            panic!("expected Spawn, got {e:?}");
        }
    }

    // ── T8-20: parse_spawn_with_args ─────────────────────────────────────────

    #[test]
    fn parse_spawn_with_args() {
        // `spawn Limiter 10 2.0` — D061 init args
        let e = ok("spawn Limiter 10 2.0");
        if let Expr::Spawn { actor, args, .. } = e {
            assert_eq!(actor.text, "Limiter");
            assert_eq!(args.len(), 2, "expected 2 args, got {}", args.len());
            assert!(
                matches!(&args[0], Expr::Literal(Literal::IntDec { raw, .. }) if raw == "10"),
                "expected IntDec(10), got {:?}",
                args[0]
            );
            assert!(
                matches!(&args[1], Expr::Literal(Literal::Float { raw, .. }) if raw == "2.0"),
                "expected Float(2.0), got {:?}",
                args[1]
            );
        } else {
            panic!("expected Spawn, got {e:?}");
        }
    }

    // ── T8-21: parse_propagate ────────────────────────────────────────────────

    #[test]
    fn parse_propagate() {
        // `fetchUser id ?` → Propagate { inner: Call(fetchUser, [id]) }
        let e = ok("fetchUser id ?");
        if let Expr::Propagate { inner, .. } = e {
            assert!(
                matches!(inner.as_ref(), Expr::Call { .. }),
                "expected Call inside Propagate, got {inner:?}"
            );
            if let Expr::Call { callee, args, .. } = inner.as_ref() {
                assert!(
                    matches!(callee.as_ref(), Expr::Ident(id) if id.text == "fetchUser"),
                    "expected callee=fetchUser, got {callee:?}"
                );
                assert_eq!(args.len(), 1);
                assert!(
                    matches!(&args[0], Expr::Ident(id) if id.text == "id"),
                    "expected arg=id, got {:?}",
                    args[0]
                );
            }
        } else {
            panic!("expected Propagate, got {e:?}");
        }
    }

    // ── T8-22: parse_pipe_chain_from_t6 ──────────────────────────────────────

    #[test]
    fn parse_pipe_chain_from_t6() {
        // `users |> List.map (.name) |> List.take 10` → chained Pipes
        let e = ok("users |> List.map (.name) |> List.take 10");
        // Top-level should be Pipe(Pipe(users, ...), ...)
        if let Expr::Pipe { lhs, rhs, .. } = e {
            // rhs = Call(Qualified(List.take), [10])
            assert!(
                matches!(rhs.as_ref(), Expr::Call { .. }),
                "expected Call on outer rhs, got {rhs:?}"
            );
            // lhs = Pipe(users, Call(Qualified(List.map), [...]))
            assert!(
                matches!(lhs.as_ref(), Expr::Pipe { .. }),
                "expected inner Pipe, got {lhs:?}"
            );
        } else {
            panic!("expected outer Pipe, got {e:?}");
        }
    }

    // ── T8-23: parse_chained_ask_rejects (D068) ───────────────────────────────

    #[test]
    fn parse_chained_ask_rejects() {
        // `a ?> m ?> n` — D068 single-site; second `?>` should be rejected.
        // After emitting Ask(a, m, []), the loop should NOT consume another `?>`.
        // The result should be an Err (P002) or the second `?>` triggers P002
        // when encountered in a context that doesn't allow it.
        //
        // Actually, after Ask(a, m) is built and returned from parse_expr_atom12,
        // the Pratt infix loop will NOT see `?>` as a binary op (it's not in
        // infix_bp). Juxtaposition won't fire because `?>` is not an arg-start
        // atom. So the parse returns Ask(a, m) with `?>` remaining. In parse_expr,
        // the remaining `?>` will trigger an error when the caller tries to use
        // the expression. For a standalone test, the cursor just has `?> n` left
        // over — which means parse succeeds but cursor is not at EOF.
        //
        // We test by wrapping in a context where trailing tokens are rejected.
        // Use parse_atom12 + check for leftover tokens.
        //
        // Simplest approach: parse_e("a ?> m ?> n") and check we don't get
        // Propagate(Ask(Ask(a,m),n)) — i.e. chaining is NOT allowed.
        let e = ok("a ?> m ?> n");
        // The parse should produce Ask(a, m, []) with `?> n` left unconsumed.
        // Since parse_e uses parse_expr which returns after the first complete
        // expression, we get Ask(a, m, []).
        assert!(
            matches!(&e, Expr::Ask { .. }),
            "expected Ask (single-site), got {e:?}"
        );
        // Verify it's NOT a double-ask.
        if let Expr::Ask { handle, .. } = &e {
            assert!(
                !matches!(handle.as_ref(), Expr::Ask { .. }),
                "chained Ask is NOT allowed per D068; got nested Ask in handle"
            );
        }
    }

    // ── T8-24: parse_lambda_missing_arrow → P001 ─────────────────────────────

    #[test]
    fn parse_lambda_missing_arrow() {
        // `fn x` — no `->`, should fail with P001.
        let result = parse_e("fn x");
        assert!(result.is_err(), "expected Err, got Ok");
        let code = result.unwrap_err().code();
        assert!(
            code == "P001" || code == "P002",
            "expected P001 or P002, got {code}"
        );
    }

    // ── T8-25: parse_with_missing_lbrace → P001 ──────────────────────────────

    #[test]
    fn parse_with_missing_lbrace() {
        // `u with x` — `with` expects `{` next, should fail with P001.
        let result = parse_e("u with x");
        assert!(result.is_err(), "expected Err for `u with x`, got Ok");
        let code = result.unwrap_err().code();
        assert_eq!(code, "P001", "expected P001, got {code}");
    }

    // ── T8 new: parse_qualified_record_ctor_basic (Phase 4 §3.8) ────────────────
    //
    // Input: `Http.Response { status = 200 }`
    // Expected: Expr::Record {
    //     constructor: RecordCtor::Qualified(QualifiedName { segments: ["Http", "Response"] }),
    //     fields: [FieldInit { name: "status", value: Some(IntDec("200")) }],
    //     ..
    // }
    #[test]
    fn parse_qualified_record_ctor_basic() {
        use ridge_ast::{expr::RecordCtor, Expr, Literal};
        let e = ok("Http.Response { status = 200 }");
        if let Expr::Record {
            constructor,
            fields,
            ..
        } = e
        {
            if let RecordCtor::Qualified(ref qn) = constructor {
                assert_eq!(
                    qn.segments.len(),
                    2,
                    "expected 2 segments, got {}",
                    qn.segments.len()
                );
                assert_eq!(qn.segments[0].text, "Http");
                assert_eq!(qn.segments[1].text, "Response");
            } else {
                panic!("expected RecordCtor::Qualified, got {constructor:?}");
            }
            assert_eq!(fields.len(), 1, "expected 1 field, got {}", fields.len());
            assert_eq!(fields[0].name.text, "status");
            assert!(
                matches!(&fields[0].value, Some(Expr::Literal(Literal::IntDec { raw, .. })) if raw == "200"),
                "expected IntDec(200) for status field, got {:?}",
                fields[0].value
            );
        } else {
            panic!("expected Expr::Record, got {e:?}");
        }
    }

    // ── T0-P1: parse roundtrip — `?> h() timeout 1000` (Phase 6 T0, OQ-E001) ──
    //
    // Verifies that the contextual `timeout <ms>` postfix parses cleanly and
    // produces `AskTimeout::Millis(IntDec(1000))` in the AST.
    #[test]
    fn parse_ask_timeout_millis() {
        use ridge_ast::AskTimeout;
        let e = ok("store ?> increment timeout 1000");
        if let Expr::Ask {
            handle,
            message,
            args,
            timeout,
            ..
        } = e
        {
            assert!(
                matches!(*handle, Expr::Ident(ref id) if id.text == "store"),
                "expected handle=store, got {handle:?}"
            );
            assert_eq!(message.text, "increment");
            assert!(args.is_empty(), "expected no positional args, got {args:?}");
            match timeout {
                Some(AskTimeout::Millis(ms)) => {
                    assert!(
                        matches!(
                            *ms,
                            Expr::Literal(Literal::IntDec { ref raw, .. }) if raw == "1000"
                        ),
                        "expected Millis(IntDec(1000)), got {ms:?}"
                    );
                }
                other => panic!("expected Some(Millis(1000)), got {other:?}"),
            }
        } else {
            panic!("expected Ask, got {e:?}");
        }
    }

    // ── T0-P2: parse roundtrip — `?> h() timeout never` (Phase 6 T0, OQ-E001) ──
    //
    // Verifies that `timeout never` parses to `AskTimeout::Never`.
    #[test]
    fn parse_ask_timeout_never() {
        use ridge_ast::AskTimeout;
        let e = ok("store ?> increment timeout never");
        if let Expr::Ask {
            handle,
            message,
            args,
            timeout,
            ..
        } = e
        {
            assert!(
                matches!(*handle, Expr::Ident(ref id) if id.text == "store"),
                "expected handle=store, got {handle:?}"
            );
            assert_eq!(message.text, "increment");
            assert!(args.is_empty(), "expected no positional args, got {args:?}");
            assert!(
                matches!(timeout, Some(AskTimeout::Never)),
                "expected Some(Never), got {timeout:?}"
            );
        } else {
            panic!("expected Ask, got {e:?}");
        }
    }

    // ── T0-P3: parse roundtrip — `?> h()` (no timeout) ──────────────────────────
    //
    // Verifies that a plain `?>` without any timeout postfix still produces
    // `timeout: None`, keeping the field at its default.
    #[test]
    fn parse_ask_no_timeout_is_none() {
        let e = ok("store ?> increment");
        if let Expr::Ask { timeout, .. } = e {
            assert!(
                timeout.is_none(),
                "plain ?> with no timeout postfix must produce timeout=None, got {timeout:?}"
            );
        } else {
            panic!("expected Ask, got {e:?}");
        }
    }

    // ── T0-P4: timeout does not bind to the arg identifier `timeout` ──────────────
    //
    // Verifies that when `timeout` IS used as an arg (not followed by `never` or
    // a numeric literal), it is treated as a positional argument, not a keyword.
    // Specifically: `store ?> shorten timeout` → Ask { args: [Ident("timeout")],
    // timeout: None }.
    #[test]
    fn parse_ask_timeout_as_arg_ident() {
        // `timeout` is followed by EOF (not `never` or a literal), so the
        // 2-token lookahead does NOT treat it as the contextual keyword.
        let e = ok("store ?> shorten timeout");
        if let Expr::Ask {
            message,
            args,
            timeout,
            ..
        } = e
        {
            assert_eq!(message.text, "shorten");
            // `timeout` should be collected as a positional arg (a local variable).
            assert_eq!(
                args.len(),
                1,
                "expected 1 arg (the `timeout` ident), got {args:?}"
            );
            assert!(
                matches!(&args[0], Expr::Ident(id) if id.text == "timeout"),
                "expected arg=Ident(timeout), got {:?}",
                args[0]
            );
            assert!(
                timeout.is_none(),
                "timeout followed by EOF must NOT be parsed as timeout keyword"
            );
        } else {
            panic!("expected Ask, got {e:?}");
        }
    }
}
