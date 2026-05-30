//! Expression parsing: atom, Pratt binary/unary, call, field access.
//!
//! This module implements:
//!
//! - [`parse_literal`]     — grammar §6.10 literals.
//! - [`parse_expr_atom`]   — grammar §6.9 atoms (extended in T6).
//! - [`parse_expr_pratt`]  — Pratt core (level 1); internal, no keyword dispatch.
//! - [`parse_expr`]  — public entry point (T7+): keyword dispatch,
//!   Pratt fallback, assign tail.
//!
//! ## Pratt table (§4.5)
//!
//! | Level | Tokens           | Assoc       | lbp | rbp |
//! |-------|------------------|-------------|-----|-----|
//! | 1     | `\|>`            | left        |  1  |  2  |
//! | 2     | `\|\|`           | right       |  4  |  3  |
//! | 3     | `&&`             | right       |  6  |  5  |
//! | 4     | `==`, `!=`       | non-assoc   |  7  |  8  |
//! | 5     | `<`,`>`,`<=`,`>=`| non-assoc   |  9  | 10  |
//! | 6     | `++`, `::`       | right       | 12  | 11  |
//! | 7     | `+`, `-`         | left        | 13  | 14  |
//! | 8     | `*`, `/`, `%`    | left        | 15  | 16  |
//! | 9     | `^`              | right       | 18  | 17  |
//! | 10    | prefix `-`       | n/a         | n/a | 19  |
//! | 11    | juxtaposition    | left        | 20  | 21  |
//! | 12    | `.field`         | postfix     | 22  | n/a |

// These functions are called from tests and will be called from production code
// in T7+.  Suppress dead_code until all callers are wired in.
#![allow(dead_code)]
#![allow(clippy::redundant_pub_crate)]

use ridge_ast::{
    expr::RecordCtor, AskTimeout, BinOp, Expr, Ident, Literal, QualifiedName, UnaryOp,
};
use ridge_lexer::Token;

use crate::{actor_ops, ctrl, cursor::Cursor, error::ParseError};

// ── parse_literal ─────────────────────────────────────────────────────────────

/// Parse a single numeric, boolean, or text literal (grammar §6.10).
///
/// Advances the cursor past the matched token.
/// Returns `Err(P002)` if the current token is not a literal.
pub(crate) fn parse_literal(cur: &mut Cursor<'_>) -> Result<Literal, ParseError> {
    let span = cur.span();
    match cur.peek().clone() {
        Token::IntDec(raw) => {
            cur.bump();
            Ok(Literal::IntDec { raw, span })
        }
        Token::IntBin(raw) => {
            cur.bump();
            Ok(Literal::IntBin { raw, span })
        }
        Token::IntOct(raw) => {
            cur.bump();
            Ok(Literal::IntOct { raw, span })
        }
        Token::IntHex(raw) => {
            cur.bump();
            Ok(Literal::IntHex { raw, span })
        }
        Token::Float(raw) => {
            cur.bump();
            Ok(Literal::Float { raw, span })
        }
        Token::KwTrue => {
            cur.bump();
            Ok(Literal::Bool { value: true, span })
        }
        Token::KwFalse => {
            cur.bump();
            Ok(Literal::Bool { value: false, span })
        }
        Token::TextLit(raw) => {
            cur.bump();
            Ok(Literal::Text { raw, span })
        }
        Token::RawTextLit(raw) => {
            cur.bump();
            Ok(Literal::RawText { raw, span })
        }
        _ => Err(ParseError::UnexpectedToken {
            span,
            description: format!("expected a literal, found `{}`", cur.peek()),
        }),
    }
}

// ── parse_expr (public, keyword-dispatch + Pratt + assign tail) ──────────────

/// Parse a full expression (grammar §6 `Expr`).
///
/// **Entry point for all statement-level expression parsing.**
///
/// Dispatch order:
/// 1. Keyword forms: `if`, `match`, `let`, `var`, `try`, `guard`, `return`
///    are dispatched to their dedicated helpers in `ctrl.rs`.
/// 2. Pratt fallback: everything else goes to [`parse_expr_pratt`].
/// 3. Assign tail: after the Pratt expression, if `<-` follows, wrap in
///    `Expr::Assign`.
///
/// `parse_expr_pratt` is used inside Pratt recursion to avoid re-dispatching
/// on keywords at interior positions.
pub(crate) fn parse_expr(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    // ── 1. Keyword dispatch ───────────────────────────────────────────────────
    let lhs = match cur.peek() {
        Token::KwIf => return ctrl::parse_if(cur),
        Token::KwMatch => return ctrl::parse_match(cur),
        Token::KwLet => return ctrl::parse_let(cur),
        Token::KwVar => return ctrl::parse_var_decl(cur),
        Token::KwTry => return ctrl::parse_try(cur),
        Token::KwGuard => return ctrl::parse_guard(cur),
        Token::KwReturn => return ctrl::parse_return(cur),
        // `fn name params = body` is InnerFn; `fn params -> body` is Lambda.
        // Disambiguation: scan forward from current position to find whether
        // `->` (Arrow) or `=` (Assign) appears first at bracket depth 0.
        // Bracket depth is tracked to skip `(name: Type)` annotated params.
        //
        // Additionally, InnerFn requires the first token after `fn` (possibly
        // after capabilities) to be a LOWER_IDENT that is NOT followed by `->`.
        // If peek_n(1) is NOT a LowerIdent (e.g. LParen, Underscore), it's a Lambda.
        Token::KwFn => {
            let is_inner_fn = fn_is_inner_fn(cur);
            if is_inner_fn {
                let fn_decl =
                    crate::decl::parse_fn_decl(cur, ridge_ast::Visibility::Private, None)?;
                let fn_span = fn_decl.span;
                return Ok(Expr::InnerFn {
                    decl: Box::new(fn_decl),
                    span: fn_span,
                });
            }
            return actor_ops::parse_lambda(cur);
        }
        Token::KwSpawn => return actor_ops::parse_spawn(cur),
        _ => parse_expr_pratt(cur)?,
    };

    // ── 2. Assign tail: `lhs <- rhs` ─────────────────────────────────────────
    if cur.peek() == &Token::LeftArrow {
        let arrow_span = cur.span();
        cur.bump(); // consume `<-`
        let rhs = parse_expr(cur)?; // right-recursive (right-assoc-ish)
        let span = lhs.span().merge(rhs.span()).merge(arrow_span);
        return Ok(Expr::Assign {
            target: Box::new(lhs),
            value: Box::new(rhs),
            span,
        });
    }

    Ok(lhs)
}

// ── parse_expr_pratt (Pratt core, no keyword dispatch) ───────────────────────

/// Pratt core — parse an expression using only the Pratt precedence table.
///
/// **Do not call for statement-level parsing** — use [`parse_expr`] instead.
/// This function is used:
/// - By [`parse_expr`] as a fallback when no keyword is matched.
/// - By helpers in `ctrl.rs` for sub-expressions where keyword re-dispatch
///   would be incorrect (e.g. the condition of an `if`, the guard of a
///   `match` arm).
pub(crate) fn parse_expr_pratt(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    parse_expr_bp(cur, 0)
}

// ── Recursion-depth guard ───────────────────────────────────────────────────

/// Maximum expression nesting depth the recursive-descent parser will follow.
///
/// The expression grammar is parsed by mutual recursion (`parse_expr_bp`
/// recurses for operands and re-enters through parenthesised/list atoms), so
/// deeply nested input — thousands of `(((…)))`, nested lists, or chained
/// operators — would otherwise grow the native stack without bound and abort
/// the process. 256 is far deeper than any hand-written or formatter-produced
/// program nests, yet shallow enough to stop well short of a stack overflow.
const MAX_EXPR_DEPTH: u32 = 256;

/// RAII guard that increments [`Cursor::expr_depth`] on creation and decrements
/// it on drop, so the counter is restored no matter which `?`/early-return path
/// unwinds out of the Pratt core.
struct DepthGuard<'a, 't> {
    cur: &'a mut Cursor<'t>,
}

impl<'a, 't> DepthGuard<'a, 't> {
    /// Enter one expression-recursion level.
    ///
    /// Returns `Err(P028 ExpressionTooDeep)` (without entering) when the limit
    /// is already reached, so the caller stops descending gracefully instead of
    /// recursing further.
    fn enter(cur: &'a mut Cursor<'t>) -> Result<Self, ParseError> {
        if cur.expr_depth >= MAX_EXPR_DEPTH {
            return Err(ParseError::ExpressionTooDeep {
                span: cur.span(),
                limit: MAX_EXPR_DEPTH,
            });
        }
        cur.expr_depth += 1;
        Ok(Self { cur })
    }
}

impl Drop for DepthGuard<'_, '_> {
    fn drop(&mut self) {
        self.cur.expr_depth -= 1;
    }
}

// ── Pratt core ────────────────────────────────────────────────────────────────

/// Pratt parser — parses expressions whose left binding power exceeds
/// `min_bp`.
///
/// Internal to this module; all callers go through [`parse_expr_pratt`].
#[allow(clippy::too_many_lines)] // Pratt table + postfix block is inherently long
#[allow(clippy::cognitive_complexity)] // same reason — Pratt loop dispatch is irreducibly branchy
fn parse_expr_bp(cur: &mut Cursor<'_>, min_bp: u8) -> Result<Expr, ParseError> {
    // ── Recursion-depth guard ─────────────────────────────────────────────────
    // Bound the descent before doing any work. The guard decrements on drop, so
    // the counter is correct on every return path below.
    let guard = DepthGuard::enter(cur)?;
    let cur = &mut *guard.cur;

    // ── Prefix / nud ─────────────────────────────────────────────────────────
    let mut lhs = if cur.peek() == &Token::Minus {
        // Unary minus (only prefix operator). rbp = 19.
        let op_span = cur.span();
        cur.bump(); // consume `-`
        let operand = parse_expr_bp(cur, 19)?;
        let span = op_span.merge(operand.span());
        Expr::Unary {
            op: UnaryOp::Neg,
            expr: Box::new(operand),
            span,
        }
    } else {
        // All other cases: atom + level-12 field access.
        parse_expr_atom12(cur)?
    };

    // ── Infix / led loop ─────────────────────────────────────────────────────
    loop {
        // ── `with` at Pratt level 5.5 (lbp=10, rbp=11, left-assoc) ──────────
        // `with` produces `Expr::With` — not a BinOp — so it is handled before
        // the generic `infix_bp` lookup.  lbp=10 means it binds tighter than
        // relational ops (lbp=9) but looser than concat/cons (lbp=12).
        if cur.peek() == &Token::KwWith && 10 > min_bp {
            cur.bump(); // consume `with`
            cur.expect(&Token::LBrace)?;
            let fields = actor_ops::parse_field_init_list(cur)?;
            let end_span = cur.expect(&Token::RBrace)?;
            let span = lhs.span().merge(end_span);
            lhs = Expr::With {
                base: Box::new(lhs),
                fields,
                span,
            };
            continue;
        }

        // ── Juxtaposition-as-call (level 11, lbp=20) ─────────────────────────
        // Must be checked before binary ops so that `f x + y` parses as
        // `(f x) + y` (call binds tighter than addition).
        //
        // When `no_layout_arm` is set (bracket-suppressed match arm bodies),
        // stop collecting args when the current token sequence looks like the
        // start of a new match arm: `<pattern> ->` or `<pattern> when`.
        // This prevents the last call-expression in an arm body from eating the
        // next arm's pattern tuple as a spurious argument.
        if 20 > min_bp && can_start_arg_atom(cur) {
            // Guard: don't start juxta if we're right on an arm boundary.
            if cur.no_layout_arm && ctrl::is_match_arm_start(cur) {
                break;
            }
            let mut args: Vec<Expr> = Vec::new();
            let call_start = lhs.span();
            while can_start_arg_atom(cur) {
                // Guard: stop before consuming the next arm's pattern.
                if cur.no_layout_arm && ctrl::is_match_arm_start(cur) {
                    break;
                }
                args.push(parse_expr_atom12(cur)?);
            }
            if args.is_empty() {
                break; // no args collected — stop to avoid infinite loop
            }
            let call_end = args.last().map_or(call_start, Expr::span);
            lhs = Expr::Call {
                callee: Box::new(lhs),
                args,
                span: call_start.merge(call_end),
            };
            continue;
        }

        // ── Postfix operators `?>`, `!`, `?` (level 12) ─────────────────────
        // These are placed AFTER juxta so that `fetchUser id ?` first builds
        // the juxta-call `Call(fetchUser, [id])`, then the `?` wraps the whole
        // Call in `Propagate`.
        //
        // Single-site rule: after ONE postfix fires we `break` — the result is
        // returned to the caller.  Chaining (`a ?> m ?> n`) is not allowed
        // without parentheses; the caller that sees leftover `?> n` will either
        // consume it as an argument to a larger expression or produce a parse
        // error in context.
        //
        // The postfix gate is set to 1, so `?`, `?>`, `!` bind LOOSER than `|>`
        // (which recurses with min_bp=2).  The pipe completes first, then the
        // postfix applies to the whole pipe result, which matches the obvious
        // reading (`xs |> fetchUser ?` = `(fetchUser xs) ?`).  Tighter binary
        // ops are unaffected — `a + b ?` still parses as `(a + b) ?`.
        if 1 > min_bp {
            match cur.peek() {
                Token::QuestionGt => {
                    // `handle ?> message arg* [timeout <ms|never>]` → Expr::Ask
                    //
                    // After collecting args, peek for the contextual identifier
                    // `timeout`.  If present, consume it and parse either the
                    // contextual identifier `never` (→ AskTimeout::Never) or an
                    // expression (→ AskTimeout::Millis).  Both `timeout` and `never`
                    // are kept as contextual identifiers (not reserved keywords) so
                    // that they remain valid variable names elsewhere.
                    cur.bump(); // consume `?>`
                    let msg_span = cur.span();
                    let msg_text = match cur.peek().clone() {
                        Token::LowerIdent(s) => {
                            cur.bump();
                            s
                        }
                        _ => {
                            return Err(ParseError::Expected {
                                span: cur.span(),
                                expected: "<message name (LOWER_IDENT)>",
                                found: cur.peek().to_string(),
                            });
                        }
                    };
                    let message = Ident::new(msg_text, msg_span);
                    // Greedily collect argument atoms (same predicate as juxta).
                    // In no-layout arm mode, cap at one arg: without Newline tokens
                    // we cannot distinguish trailing arg atoms from the next
                    // statement (e.g. `store ?> shorten url` followed immediately
                    // by `okText …` with no Newline separator).
                    let max_args: usize = if cur.no_layout_arm { 1 } else { usize::MAX };
                    let mut args: Vec<Expr> = Vec::new();
                    while actor_ops::can_start_arg_atom(cur) && args.len() < max_args {
                        if cur.no_layout_arm && ctrl::is_match_arm_start(cur) {
                            break;
                        }
                        // Two-token lookahead: stop before `timeout <never|literal>`
                        // to let the contextual keyword handling below consume it.
                        // This prevents `timeout` from being mis-parsed as an arg.
                        //
                        // Trigger: current token is `timeout` AND the following
                        // token is either `never` (another lower ident) or a numeric
                        // literal (IntDec/IntBin/IntOct/IntHex/Float).  Both are
                        // unambiguous — `never` can only be the `timeout never`
                        // form; numeric literals cannot be the first token of the
                        // next statement when preceded by the contextual `timeout`.
                        if matches!(cur.peek(), Token::LowerIdent(s) if s == "timeout") {
                            let next = cur.peek_n(1);
                            let is_timeout_postfix = matches!(
                                next,
                                Some(
                                    Token::LowerIdent(_)
                                        | Token::IntDec(_)
                                        | Token::IntBin(_)
                                        | Token::IntOct(_)
                                        | Token::IntHex(_)
                                        | Token::Float(_)
                                )
                            );
                            if is_timeout_postfix {
                                break;
                            }
                        }
                        args.push(parse_expr_atom12(cur)?);
                    }

                    // ── Optional `timeout <ms|never>` postfix ────────────────
                    // Single-token lookahead: only trigger when the very next token
                    // is the contextual identifier "timeout" (LowerIdent("timeout")).
                    // This ensures the postfix only fires immediately after the `?>`
                    // expression and cannot bind to a `timeout` identifier in any
                    // other syntactic position.
                    let timeout = if matches!(cur.peek(), Token::LowerIdent(s) if s == "timeout") {
                        cur.bump(); // consume contextual `timeout`
                                    // Next: either `never` (contextual ident) or an expression.
                        if matches!(cur.peek(), Token::LowerIdent(s) if s == "never") {
                            cur.bump(); // consume contextual `never`
                            Some(AskTimeout::Never)
                        } else {
                            // Parse a full expression for the millisecond count.
                            let ms_expr = parse_expr(cur)?;
                            Some(AskTimeout::Millis(Box::new(ms_expr)))
                        }
                    } else {
                        None
                    };

                    let end_span = timeout
                        .as_ref()
                        .and_then(|t| {
                            if let AskTimeout::Millis(e) = t {
                                Some(e.span())
                            } else {
                                None
                            }
                        })
                        .or_else(|| args.last().map(Expr::span))
                        .unwrap_or(msg_span);
                    let span = lhs.span().merge(end_span);
                    lhs = Expr::Ask {
                        handle: Box::new(lhs),
                        message,
                        args,
                        timeout,
                        span,
                    };
                    break; // single-site — stop here
                }
                Token::Bang => {
                    // `handle ! message arg*` → Expr::Send
                    // The message may be a call-expression (juxtaposition), e.g.
                    // `collector ! report allowed denied` where `report allowed denied`
                    // parses as `Call { callee: report, args: [allowed, denied] }`.
                    // Use parse_expr_pratt so juxtaposition is collected.
                    cur.bump(); // consume `!`
                    let message = parse_expr_pratt(cur)?;
                    let span = lhs.span().merge(message.span());
                    lhs = Expr::Send {
                        handle: Box::new(lhs),
                        message: Box::new(message),
                        span,
                    };
                    break; // single-site — stop here
                }
                Token::Question => {
                    // `expr ?` → Expr::Propagate
                    let end_span = cur.span();
                    cur.bump(); // consume `?`
                    let span = lhs.span().merge(end_span);
                    lhs = Expr::Propagate {
                        inner: Box::new(lhs),
                        span,
                    };
                    break; // single-site — stop here
                }
                _ => {}
            }
        }

        // ── Binary operators ──────────────────────────────────────────────────
        let Some((lbp, rbp, op)) = infix_bp(cur.peek()) else {
            break;
        };
        if lbp <= min_bp {
            break;
        }

        // ── Non-associative chain check (P009) ────────────────────────────────
        // Non-associative operators (levels 4–5) must not chain.  After `a op b`,
        // we are back at the call site with min_bp=0.  If the next token is a
        // same-level non-assoc op, detect it here BEFORE consuming it.
        // We detect chaining by noting that lbp == rbp - 1 for non-assoc (the
        // pair (7,8) for level-4, (9,10) for level-5).
        // A chain is triggered when `lbp > min_bp` but we are already "inside"
        // a non-assoc expression at the same level.  We detect this by returning
        // a P009 error when the lhs was produced at the same non-assoc level.
        // Simpler: track via a flag passed as a parameter.
        //
        // Actually the right detection point: after building a non-assoc Binary,
        // we return to the parent call.  The parent then re-enters the loop and
        // sees the next non-assoc token. The parent's min_bp is 0 (top-level).
        // We can detect it by: "we are about to consume a non-assoc op, but the
        // lhs is itself a Binary with a non-assoc op at the same level."
        //
        // Cleanest approach: check if lhs is a non-assoc Binary AND incoming op
        // is also non-assoc at the same level.  The `is_non_assoc(*prev_op)` guard
        // is load-bearing: without it, any `(arith op) non_assoc x` chain such as
        // `a + b == c` would be wrongly rejected, because `non_assoc_level` returns
        // 0 for every op and the level-equality check would always match.
        if is_non_assoc(op) {
            if let Expr::Binary { op: prev_op, .. } = &lhs {
                if is_non_assoc(*prev_op) && non_assoc_level(*prev_op) == non_assoc_level(op) {
                    let err_span = cur.span();
                    return Err(ParseError::NonAssociativeChain {
                        span: err_span,
                        op: op_static_str(op),
                    });
                }
            }
        }

        let op_span = cur.span();
        cur.bump(); // consume the operator token

        // ── Pipe forward → Expr::Pipe ─────────────────────────────────────────
        if op == BinOp::Pipe {
            let rhs = parse_expr_bp(cur, rbp)?;
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Pipe {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
            continue;
        }

        let rhs = parse_expr_bp(cur, rbp)?;
        let span = lhs.span().merge(rhs.span()).merge(op_span);
        lhs = Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span,
        };
    }

    Ok(lhs)
}

// ── Level-12 atom: atom + field access ───────────────────────────────────────

/// Parse an atom (grammar §6.9) and then greedily consume `.field` suffixes
/// (grammar §6 level 12, lbp = 22).
///
/// `a.b.c` → `FieldAccess { base: FieldAccess { base: a, field: b }, field: c }`.
///
/// Note: postfix operators `?>`, `!`, `?` are NOT handled here — they are
/// handled in `parse_expr_bp` AFTER the juxta-call block so that
/// `fetchUser id ?` parses as `Propagate(Call(fetchUser, [id]))` rather than
/// `Call(fetchUser, [Propagate(id)])`.
pub(crate) fn parse_expr_atom12(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let mut base = parse_expr_atom(cur)?;

    // Level-12 field access: greedily consume `.LOWER_IDENT` chains.
    while cur.peek() == &Token::Dot {
        // Peek ahead: if next after Dot is LowerIdent → field access.
        // Otherwise stop (e.g. qualified names are handled in parse_expr_atom).
        if !matches!(cur.peek_n(1), Some(Token::LowerIdent(_))) {
            break;
        }
        cur.bump(); // consume `.`
        let field_span = cur.span();
        let field_text = match cur.bump() {
            Token::LowerIdent(s) => s.clone(),
            _ => unreachable!("peek_n checked LowerIdent above"),
        };
        let field = Ident::new(field_text, field_span);
        let span = base.span().merge(field_span);
        base = Expr::FieldAccess {
            base: Box::new(base),
            field,
            span,
        };
    }

    Ok(base)
}

// ── parse_expr_atom ───────────────────────────────────────────────────────────

/// Parse a single atomic expression (grammar §6.9 `ExprAtom`).
///
/// Extended in T6 to handle:
/// - `()` → `Expr::Unit`
/// - `(.name)` → `Expr::FieldAccessorFn`
/// - `(e)` → `Expr::Paren`
/// - `(e, e, …)` → `Expr::Tuple`
/// - `[…]` → `Expr::List`
///
/// Forms not yet handled (T7–T8):
/// - `fn` → Lambda / `InnerFn`
/// - `spawn` → `SpawnExpr`
/// - Bare `UPPER_IDENT` without dot → Record construction (T8)
pub(crate) fn parse_expr_atom(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let span = cur.span();

    match cur.peek() {
        // ── Literals ──────────────────────────────────────────────────────────
        Token::IntDec(_)
        | Token::IntBin(_)
        | Token::IntOct(_)
        | Token::IntHex(_)
        | Token::Float(_)
        | Token::KwTrue
        | Token::KwFalse
        | Token::TextLit(_)
        | Token::RawTextLit(_) => parse_literal(cur).map(Expr::Literal),

        // ── Parenthesised forms ───────────────────────────────────────────────
        // Four cases dispatched by lookahead:
        //   `()` (Unit), `(.name)` (FieldAccessorFn), `(e)` (Paren),
        //   `(e, …)` (Tuple).
        Token::LParen => parse_paren_expr(cur),

        // ── List literal `[…]` ────────────────────────────────────────────────
        Token::LBrack => parse_list_literal(cur),

        // ── Lower-case / private identifier ──────────────────────────────────
        Token::LowerIdent(_) => {
            let text = match cur.bump() {
                Token::LowerIdent(s) => s.clone(),
                _ => unreachable!(),
            };
            Ok(Expr::Ident(Ident::new(text, span)))
        }

        // ── Bare `_` wildcard — not an expression ────────────────────────────
        Token::Underscore => Err(ParseError::UnexpectedToken {
            span,
            description: "bare `_` is a wildcard pattern, not an expression".to_string(),
        }),

        // ── Lambda or InnerFn disambiguation ─────────────────────────────────
        // Rule: after `fn`, if peek_n(1) is a LowerIdent AND peek_n(2) is not
        // `->`, it is an InnerFn (the LowerIdent is the function name).
        // Otherwise it is a Lambda.
        //
        // Examples:
        //   fn x -> x + 1          → Lambda (peek_n(2) == Arrow)
        //   fn (x, y) -> body      → Lambda (peek_n(1) == LParen)
        //   fn foo x = x + 1       → InnerFn (peek_n(2) != Arrow)
        //   fn io log msg = body   → InnerFn (peek_n(2) == LowerIdent)
        Token::KwFn => {
            // Use the same scan-forward rule as in parse_expr.
            if fn_is_inner_fn(cur) {
                let fn_decl =
                    crate::decl::parse_fn_decl(cur, ridge_ast::Visibility::Private, None)?;
                let fn_span = fn_decl.span;
                Ok(Expr::InnerFn {
                    decl: Box::new(fn_decl),
                    span: fn_span,
                })
            } else {
                actor_ops::parse_lambda(cur)
            }
        }

        // ── Spawn expression `spawn UPPER_IDENT arg*` ────────────────────────
        Token::KwSpawn => actor_ops::parse_spawn(cur),

        // ── `UPPER_IDENT`: qualified name, record construct, or bare ctor ─────
        //
        // T8 (Phase 4 §3.8): A qualified path followed by `{` is a qualified
        // record constructor: `Http.Response { ... }`.
        // All other qualified paths (e.g. `List.map`) remain `Expr::Qualified`.
        Token::UpperIdent(_) => {
            if cur.peek_n(1) == Some(&Token::Dot) {
                // Could be: qualified name OR qualified record constructor.
                // Parse the qualified name first, then check for `{`.
                let qn = parse_qualified_name(cur)?;
                if cur.peek() == &Token::LBrace {
                    // Qualified record construction: Http.Response { ... }
                    let ctor = RecordCtor::Qualified(qn);
                    actor_ops::parse_record_construct(cur, ctor)
                } else {
                    // Regular qualified name in expression position.
                    Ok(Expr::Qualified(qn))
                }
            } else if cur.peek_n(1) == Some(&Token::LBrace) {
                // Record construction: User { ... }
                let ctor_span = cur.span();
                let ctor_text = match cur.bump() {
                    Token::UpperIdent(s) => s.clone(),
                    _ => unreachable!("peeked UpperIdent above"),
                };
                let constructor = RecordCtor::Bare(Ident::new(ctor_text, ctor_span));
                actor_ops::parse_record_construct(cur, constructor)
            } else {
                // Bare constructor in expression position — treat as zero-arg
                // record construct with empty fields (e.g. `None`, `True`).
                // Grammar §6.18: Record { constructor, fields: [] }.
                let ctor_span = cur.span();
                let ctor_text = match cur.bump() {
                    Token::UpperIdent(s) => s.clone(),
                    _ => unreachable!("peeked UpperIdent above"),
                };
                let constructor = RecordCtor::Bare(Ident::new(ctor_text, ctor_span));
                Ok(Expr::Record {
                    fields: vec![],
                    span: ctor_span,
                    constructor,
                })
            }
        }

        // ── Interpolated string (full with holes, T8) ─────────────────────────
        Token::InterpStart => actor_ops::parse_interp_full(cur),

        // ── Layout tokens in expression position (P006) ──────────────────────
        // `Indent` and `Dedent` should never appear at the start of an atom
        // expression.  Their presence indicates a layout invariant violation
        // (e.g. an unexpected extra indent, or a dedent that the block parser
        // missed).  Report P006 rather than the generic P002.
        Token::Indent | Token::Dedent => Err(ParseError::LayoutMismatch {
            span,
            hint: "unexpected layout token in expression position",
        }),

        // ── Everything else ───────────────────────────────────────────────────
        _ => Err(ParseError::UnexpectedToken {
            span,
            description: format!("unexpected token `{}` in expression position", cur.peek()),
        }),
    }
}

// ── Parenthesised expression dispatch ────────────────────────────────────────

/// Parse one of: `()` (Unit), `(.name)` (`FieldAccessorFn`), `(e)` (Paren),
/// `(e, e, …)` (Tuple).
///
/// Precondition: `cur.peek() == LParen`.
fn parse_paren_expr(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start_span = cur.span();
    cur.bump(); // consume `(`

    // ── `()` — unit literal ───────────────────────────────────────────────────
    if cur.peek() == &Token::RParen {
        let end_span = cur.span();
        cur.bump(); // consume `)`
        return Ok(Expr::Unit(start_span.merge(end_span)));
    }

    // ── `(.name)` — field accessor function ──────────────────────────────────
    if cur.peek() == &Token::Dot {
        if let Some(Token::LowerIdent(_)) = cur.peek_n(1) {
            cur.bump(); // consume `.`
            let field_span = cur.span();
            let field_text = match cur.bump() {
                Token::LowerIdent(s) => s.clone(),
                _ => unreachable!(),
            };
            let end_span = cur.expect(&Token::RParen)?;
            let span = start_span.merge(end_span);
            return Ok(Expr::FieldAccessorFn {
                field: Ident::new(field_text, field_span),
                span,
            });
        }
    }

    // ── `(e)` or `(e, e, …)` — parse first expression, then decide ───────────
    // Increment bracket depth so that parse_branch_body knows it may apply the
    // flat-block NEWLINE extension.
    cur.bracket_depth += 1;
    let result = parse_paren_inner(cur, start_span);
    cur.bracket_depth -= 1;
    result
}

/// Inner helper for `parse_paren_expr`, called after the opening `(` is
/// consumed and `bracket_depth` is incremented.
fn parse_paren_inner(
    cur: &mut Cursor<'_>,
    start_span: ridge_ast::Span,
) -> Result<Expr, ParseError> {
    let first = parse_expr(cur)?;

    // Skip trailing NEWLINE before `)` — can occur when the closing paren is
    // on its own physical line.  Only skip if immediately followed by `)` to
    // avoid hiding real errors.
    if cur.peek() == &Token::Newline && cur.peek_n(1) == Some(&Token::RParen) {
        cur.bump(); // consume NEWLINE
    }

    if cur.peek() == &Token::RParen {
        // Single element in parens → Paren expression.
        let end_span = cur.span();
        cur.bump(); // consume `)`
        return Ok(Expr::Paren {
            inner: Box::new(first),
            span: start_span.merge(end_span),
        });
    }

    if cur.peek() == &Token::Comma {
        // Tuple: collect remaining elements.
        let mut elems = vec![first];
        while cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
            if cur.peek() == &Token::RParen {
                // Trailing comma before `)` — stop collecting.
                break;
            }
            elems.push(parse_expr(cur)?);
        }
        let end_span = cur.expect(&Token::RParen)?;
        let span = start_span.merge(end_span);
        return Ok(Expr::Tuple { elems, span });
    }

    // Unexpected token after first element.
    Err(ParseError::Expected {
        span: cur.span(),
        expected: "`)` or `,`",
        found: cur.peek().to_string(),
    })
}

// ── List literal ──────────────────────────────────────────────────────────────

/// Parse a list literal `[e₁, e₂, …]` (grammar §6.11).
///
/// Empty list `[]` is allowed.  Trailing comma is allowed.
/// Returns `Err(P001)` if `]` is missing.
fn parse_list_literal(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start_span = cur.span();
    cur.bump(); // consume `[`
    cur.bracket_depth += 1;
    let result = parse_list_inner(cur, start_span);
    cur.bracket_depth -= 1;
    result
}

fn parse_list_inner(cur: &mut Cursor<'_>, start_span: ridge_ast::Span) -> Result<Expr, ParseError> {
    let mut elems: Vec<Expr> = Vec::new();

    // The layout pass may emit a NEWLINE inside `[...]` before each new
    // sibling logical line.  Skip NEWLINEs at every separator position so the
    // list parser sees a clean `elem , elem , ... ]` stream.
    cur.skip_newlines();

    // Empty list.
    if cur.peek() == &Token::RBrack {
        let end_span = cur.span();
        cur.bump();
        return Ok(Expr::List {
            elems,
            span: start_span.merge(end_span),
        });
    }

    // First element.
    elems.push(parse_expr(cur)?);
    cur.skip_newlines();

    // Remaining elements separated by `,`.
    while cur.peek() == &Token::Comma {
        cur.bump(); // consume `,`
        cur.skip_newlines();
        if cur.peek() == &Token::RBrack {
            // Trailing comma.
            break;
        }
        elems.push(parse_expr(cur)?);
        cur.skip_newlines();
    }

    let end_span = cur.expect(&Token::RBrack)?;
    Ok(Expr::List {
        elems,
        span: start_span.merge(end_span),
    })
}

// ── Qualified name ────────────────────────────────────────────────────────────

/// Parse a qualified dotted name (grammar §6.15).
///
/// Syntax: `UPPER_IDENT ( "." (LOWER_IDENT | UPPER_IDENT) )+`
///
/// Precondition: `cur.peek()` is `Token::UpperIdent` and `cur.peek_n(1)` is
/// `Token::Dot`.
fn parse_qualified_name(cur: &mut Cursor<'_>) -> Result<QualifiedName, ParseError> {
    let start_span = cur.span();

    let first_text = match cur.bump() {
        Token::UpperIdent(s) => s.clone(),
        _ => unreachable!("precondition: current token is UpperIdent"),
    };
    let mut segments = vec![Ident::new(first_text, start_span)];
    let mut end_span = start_span;

    // Consume one or more `.segment` pairs.  Always consume the Dot once we
    // enter the loop — stopping without consuming would leave a stray `.`
    // that breaks callers expecting a clean parse position.
    while cur.peek() == &Token::Dot {
        cur.bump(); // consume `.`
        let seg_span = cur.span();
        match cur.peek().clone() {
            Token::LowerIdent(text) | Token::UpperIdent(text) => {
                cur.bump();
                segments.push(Ident::new(text, seg_span));
                end_span = seg_span;
            }
            _ => {
                // Dot not followed by an identifier — grammar §6.15 violation.
                return Err(ParseError::Expected {
                    span: seg_span,
                    expected: "<identifier>",
                    found: cur.peek().to_string(),
                });
            }
        }
    }

    Ok(QualifiedName {
        segments,
        span: start_span.merge(end_span),
    })
}

// ── Juxtaposition / argument-start predicate ─────────────────────────────────

/// Return `true` if the current token can start an `Expr12` atom used as a
/// call argument in juxtaposition (level 11).
///
/// Excludes:
/// - Layout tokens: `Newline`, `Indent`, `Dedent`
/// - Delimiters that close a context: `)`, `]`, `}`, `,`
/// - Binary/infix operators
/// - Keywords that begin statements, not expressions: `fn`, `spawn` (T8)
/// - `Dot` — a bare `.` is not an expression start (field access on LHS)
/// - `Eof`
fn can_start_arg_atom(cur: &Cursor<'_>) -> bool {
    matches!(
        cur.peek(),
        Token::IntDec(_)
            | Token::IntBin(_)
            | Token::IntOct(_)
            | Token::IntHex(_)
            | Token::Float(_)
            | Token::TextLit(_)
            | Token::RawTextLit(_)
            | Token::KwTrue
            | Token::KwFalse
            | Token::InterpStart
            | Token::LowerIdent(_)
            | Token::UpperIdent(_)
            | Token::LParen
            | Token::LBrack
            | Token::KwFn
            | Token::KwSpawn
    )
}

// ── Infix binding powers ──────────────────────────────────────────────────────

/// Return `Some((lbp, rbp, op))` if the current token is a binary infix
/// operator, otherwise `None`.
///
/// Binding powers follow §4.5 exactly (see module-level table).
const fn infix_bp(tok: &Token) -> Option<(u8, u8, BinOp)> {
    match tok {
        Token::PipeFwd => Some((1, 2, BinOp::Pipe)),
        Token::PipePipe => Some((4, 3, BinOp::Or)),
        Token::AmpAmp => Some((6, 5, BinOp::And)),
        Token::EqEq => Some((7, 8, BinOp::Eq)),
        Token::BangEq => Some((7, 8, BinOp::Ne)),
        Token::Lt => Some((9, 10, BinOp::Lt)),
        Token::Gt => Some((9, 10, BinOp::Gt)),
        Token::Le => Some((9, 10, BinOp::Le)),
        Token::Ge => Some((9, 10, BinOp::Ge)),
        Token::PlusPlus => Some((12, 11, BinOp::Concat)),
        Token::ColonColon => Some((12, 11, BinOp::Cons)),
        Token::Plus => Some((13, 14, BinOp::Add)),
        Token::Minus => Some((13, 14, BinOp::Sub)),
        Token::Star => Some((15, 16, BinOp::Mul)),
        Token::Slash => Some((15, 16, BinOp::Div)),
        Token::Percent => Some((15, 16, BinOp::Mod)),
        Token::Caret => Some((18, 17, BinOp::Pow)),
        _ => None,
    }
}

// ── Non-associativity helpers ─────────────────────────────────────────────────

/// Return `true` if `op` is a non-associative comparison operator (levels 4–5).
const fn is_non_assoc(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge
    )
}

/// Return the non-assoc level (4 or 5) for a comparison operator.
///
/// Used to decide whether two successive non-assoc ops are at the *same* level
/// (both would trigger P009) vs different levels (`a == b < c` also triggers).
/// Per the plan: same-level OR cross-level — all level-4 and level-5 chains
/// are errors.  We model this by returning the same level ID for both groups.
const fn non_assoc_level(_op: BinOp) -> u8 {
    // All non-assoc ops (levels 4 and 5) share the same "non-assoc group" —
    // cross-level chains (`a == b < c`) are also errors per the plan.
    0
}

/// Return the static display string for a `BinOp` (used in P009 messages).
const fn op_static_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Pipe => "|>",
        BinOp::Or => "||",
        BinOp::And => "&&",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        BinOp::Concat => "++",
        BinOp::Cons => "::",
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Pow => "^",
    }
}

// ── InnerFn / Lambda disambiguation ──────────────────────────────────────────

/// Determine whether the current `fn` token begins an `InnerFnExpr` or
/// a `LambdaExpr`, without consuming any tokens.
///
/// Strategy:
///
/// **Step 1 — name check.**  Skip `fn` and any leading capability keywords
/// (`io`, `fs`, `net`, `time`, `random`, `env`, `proc`, `spawn`, `ffi`).
/// If the first non-capability token is NOT a `LowerIdent` or `PrivIdent`,
/// there is no function name → this is a Lambda (e.g. `fn (x, y) -> …`).
///
/// **Step 2 — scan for body `=`.**  From that name token, scan forward
/// tracking bracket depth.  At depth 0, the first decisive token determines
/// the result:
///
/// | Token | Effect |
/// |-------|--------|
/// | `=` (`Assign`) | **`InnerFn`** — body separator found |
/// | `->` (`Arrow`) | set `past_arrow = true`, keep scanning |
/// | statement keyword (`let`/`var`/`if`/`match`/`guard`/`return`/`try`/`spawn`) AFTER arrow | **Lambda** — body has started |
/// | layout (`Newline`/`Indent`/`Dedent`) or `Eof` | **Lambda** — end of line |
/// | bracket closing past depth 0 | **Lambda** — exited enclosing scope |
///
/// This correctly handles all `InnerFn` forms:
/// - `fn foo x = body`                         (no return type)
/// - `fn foo x -> Ret = body`                  (with return type)
/// - `fn io time loop (gen: Int) -> Unit = …`  (caps + annotated params)
///
/// And Lambda forms:
/// - `fn x -> x + 1`                (`->` then no `=` before scope exit)
/// - `fn x -> let y = x`            (`let` after `->` → body started)
/// - `fn (x, y) -> body`            (no name → step 1 returns false)
/// - `fn row -> let line = row …`   (block lambda; `let` after `->`)
///
/// Precondition: `cur.peek() == &Token::KwFn`.
fn fn_is_inner_fn(cur: &Cursor<'_>) -> bool {
    // Scan up to 200 tokens after `fn` (offset 0).
    const SCAN_LIMIT: usize = 200;

    // ── Step 1: skip capabilities; check for a function name ─────────────────
    const CAPS: &[&str] = &[
        "io", "fs", "net", "time", "random", "env", "proc", "spawn", "ffi",
    ];
    let mut first_non_cap = 1usize;
    loop {
        match cur.peek_n(first_non_cap) {
            Some(Token::LowerIdent(s)) if CAPS.contains(&s.as_str()) => {
                first_non_cap += 1;
            }
            _ => break,
        }
    }
    // If first non-capability token is not a lower-ident name, it is a Lambda.
    match cur.peek_n(first_non_cap) {
        Some(Token::LowerIdent(_)) => {}
        _ => return false,
    }

    // ── Step 2: scan for `=` at depth 0, watching for Arrow and body keywords ─
    let mut depth: i32 = 0;
    // Set to true once we have seen `->` at depth 0.  After that, statement
    // keywords at depth 0 indicate we are inside the lambda body, not a type.
    let mut past_arrow = false;

    for i in 1..SCAN_LIMIT {
        match cur.peek_n(i) {
            // ── Bracket depth tracking ────────────────────────────────────────
            Some(Token::LParen | Token::LBrack | Token::LBrace) => depth += 1,
            Some(Token::RParen | Token::RBrack | Token::RBrace) => {
                depth -= 1;
                if depth < 0 {
                    // Exited an enclosing bracket scope without finding `=` → Lambda.
                    return false;
                }
            }
            // ── Arrow at depth 0: might be return-type separator → keep scanning ─
            Some(Token::Arrow) if depth == 0 => {
                past_arrow = true;
            }
            // ── Assign at depth 0: InnerFn body separator ─────────────────────
            Some(Token::Assign) if depth == 0 => {
                return true; // InnerFn
            }
            // ── Statement keywords after `->`: we are inside the body → Lambda ─
            Some(
                Token::KwLet
                | Token::KwVar
                | Token::KwIf
                | Token::KwMatch
                | Token::KwGuard
                | Token::KwReturn
                | Token::KwTry
                | Token::KwSpawn,
            ) if depth == 0 && past_arrow => {
                return false; // Lambda: body has started
            }
            // ── Layout / Eof at depth 0 → end of logical line → Lambda ─────────
            Some(Token::Newline | Token::Indent | Token::Dedent | Token::Eof) if depth == 0 => {
                return false;
            }
            None => return false,
            _ => {}
        }
    }
    false // scan limit reached without signal → default Lambda
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::panic)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ridge_ast::{Expr, Literal, Span};
    use ridge_lexer::tokenize;

    fn lex(src: &str) -> Vec<(Token, Span)> {
        tokenize(src).tokens
    }

    fn parse_atom(src: &str) -> Result<Expr, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        parse_expr_atom(&mut cur)
    }

    fn parse_lit(src: &str) -> Result<Literal, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        parse_literal(&mut cur)
    }

    fn parse_e(src: &str) -> Result<Expr, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        parse_expr(&mut cur)
    }

    // Helper: get Ok(expr) or panic with debug info.
    fn ok(src: &str) -> Expr {
        parse_e(src).unwrap_or_else(|e| panic!("parse_expr({src:?}) failed: {e:?}"))
    }

    // Helper: get Err(e) or panic.
    fn err(src: &str) -> ParseError {
        parse_e(src)
            .err()
            .unwrap_or_else(|| panic!("parse_expr({src:?}) expected Err, got Ok"))
    }

    // ── Literal tests ─────────────────────────────────────────────────────────

    #[test]
    fn parse_literal_int_dec() {
        let r = parse_lit("42");
        assert!(matches!(r, Ok(Literal::IntDec { ref raw, .. }) if raw == "42"));
    }

    #[test]
    fn parse_literal_int_bin() {
        let r = parse_lit("0b101");
        assert!(matches!(r, Ok(Literal::IntBin { ref raw, .. }) if raw == "0b101"));
    }

    #[test]
    fn parse_literal_int_oct() {
        let r = parse_lit("0o17");
        assert!(matches!(r, Ok(Literal::IntOct { ref raw, .. }) if raw == "0o17"));
    }

    #[test]
    fn parse_literal_int_hex() {
        let r = parse_lit("0xDEADBEEF");
        assert!(matches!(r, Ok(Literal::IntHex { ref raw, .. }) if raw == "0xDEADBEEF"));
    }

    #[test]
    fn parse_literal_float() {
        let r = parse_lit("3.14");
        assert!(matches!(r, Ok(Literal::Float { ref raw, .. }) if raw == "3.14"));
    }

    #[test]
    fn parse_literal_bool_true() {
        assert!(matches!(
            parse_lit("true"),
            Ok(Literal::Bool { value: true, .. })
        ));
    }

    #[test]
    fn parse_literal_bool_false() {
        assert!(matches!(
            parse_lit("false"),
            Ok(Literal::Bool { value: false, .. })
        ));
    }

    #[test]
    fn parse_literal_text() {
        let r = parse_lit("\"hello\"");
        if let Ok(Literal::Text { raw, .. }) = r {
            assert_eq!(raw, "hello");
        } else {
            panic!("expected Literal::Text, got {r:?}");
        }
    }

    #[test]
    fn parse_expr_unit() {
        assert!(matches!(parse_atom("()"), Ok(Expr::Unit(_))));
    }

    #[test]
    fn parse_expr_ident_lower() {
        if let Ok(Expr::Ident(id)) = parse_atom("foo") {
            assert_eq!(id.text, "foo");
        } else {
            panic!("expected Expr::Ident");
        }
    }

    #[test]
    fn parse_expr_ident_priv() {
        if let Ok(Expr::Ident(id)) = parse_atom("_foo") {
            assert_eq!(id.text, "_foo");
            assert!(id.is_priv());
        } else {
            panic!("expected Expr::Ident");
        }
    }

    #[test]
    fn parse_expr_qualified_two_segments() {
        if let Ok(Expr::Qualified(q)) = parse_atom("Io.println") {
            assert_eq!(q.segments.len(), 2);
            assert_eq!(q.segments[0].text, "Io");
            assert_eq!(q.segments[1].text, "println");
        } else {
            panic!("expected Expr::Qualified");
        }
    }

    #[test]
    fn parse_expr_qualified_three_segments() {
        if let Ok(Expr::Qualified(q)) = parse_atom("List.Map.get") {
            assert_eq!(q.segments.len(), 3);
        } else {
            panic!("expected Expr::Qualified");
        }
    }

    #[test]
    fn parse_expr_interp_zero_holes() {
        use ridge_ast::InterpPart;
        if let Ok(Expr::Interp { ref parts, .. }) = parse_atom("$\"hello\"") {
            assert_eq!(parts.len(), 1);
            assert!(matches!(&parts[0], InterpPart::Text { raw, .. } if raw == "hello"));
        } else {
            panic!("expected Expr::Interp");
        }
    }

    #[test]
    fn parse_expr_bare_upper_is_empty_record() {
        // T8: bare UPPER_IDENT in expression position is treated as a zero-arg
        // Record construct (e.g. `None`, `True`).  It produces Expr::Record
        // with an empty fields list rather than an error.
        use ridge_ast::{expr::RecordCtor, Expr};
        let r = parse_atom("Foo");
        assert!(
            matches!(r, Ok(Expr::Record { constructor: RecordCtor::Bare(ref id), ref fields, .. })
                if id.text == "Foo" && fields.is_empty()),
            "expected Record {{ constructor: RecordCtor::Bare(Foo), fields: [] }}, got {r:?}"
        );
    }

    #[test]
    fn parse_expr_qualified_trailing_dot() {
        let r = parse_atom("Io.");
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code(), "P001");
    }

    #[test]
    fn parse_expr_interp_with_hole_parses() {
        // T8: expression holes `${...}` are now fully supported by
        // `parse_interp_full`.  Verify that a hole is parsed as InterpPart::Expr.
        use ridge_ast::InterpPart;
        let r = parse_atom("$\"hi ${name}\"");
        assert!(r.is_ok(), "expected Ok for interp with hole, got {r:?}");
        if let Ok(ridge_ast::Expr::Interp { parts, .. }) = r {
            let has_expr_part = parts.iter().any(|p| matches!(p, InterpPart::Expr { .. }));
            assert!(
                has_expr_part,
                "expected at least one Expr part in interp, got {parts:?}"
            );
        } else {
            panic!("expected Expr::Interp");
        }
    }

    #[test]
    fn parse_expr_bare_underscore_rejects() {
        let r = parse_atom("_");
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code(), "P002");
    }

    #[test]
    fn parse_literal_span_is_nonzero() {
        let r = parse_lit("42");
        assert!(r.is_ok());
        assert!(!r.unwrap().span().is_empty());
    }

    #[test]
    fn parse_expr_unit_span_covers_both_parens() {
        let r = parse_atom("()");
        assert!(r.is_ok());
        assert_eq!(r.unwrap().span().len(), 2);
    }

    #[test]
    fn parse_expr_qualified_span_covers_full_path() {
        let r = parse_atom("Io.println");
        assert!(r.is_ok());
        assert_eq!(r.unwrap().span().len(), 10);
    }

    // ── T6: Pratt precedence positive tests ──────────────────────────────────

    /// Test 1: `a |> b |> c` → `Pipe(Pipe(a,b), c)` (left-assoc).
    #[test]
    fn pratt_pipe_left_assoc() {
        let e = ok("a |> b |> c");
        // Should be Pipe { lhs: Pipe { lhs: a, rhs: b }, rhs: c }
        if let Expr::Pipe { lhs, rhs, .. } = e {
            // outer rhs = c
            assert!(matches!(*rhs, Expr::Ident(ref id) if id.text == "c"));
            // outer lhs = Pipe(a, b)
            if let Expr::Pipe {
                lhs: inner_lhs,
                rhs: inner_rhs,
                ..
            } = *lhs
            {
                assert!(matches!(*inner_lhs, Expr::Ident(ref id) if id.text == "a"));
                assert!(matches!(*inner_rhs, Expr::Ident(ref id) if id.text == "b"));
            } else {
                panic!("expected inner Pipe");
            }
        } else {
            panic!("expected outer Pipe, got {e:?}");
        }
    }

    /// Test 2: `a || b || c` → `Or(a, Or(b, c))` (right-assoc).
    #[test]
    fn pratt_or_right_assoc() {
        let e = ok("a || b || c");
        if let Expr::Binary {
            op: BinOp::Or,
            lhs,
            rhs,
            ..
        } = e
        {
            // lhs = a
            assert!(matches!(*lhs, Expr::Ident(ref id) if id.text == "a"));
            // rhs = Or(b, c)
            assert!(matches!(*rhs, Expr::Binary { op: BinOp::Or, .. }));
        } else {
            panic!("expected Or, got {e:?}");
        }
    }

    /// Test 3: `a && b && c` → `And(a, And(b, c))` (right-assoc).
    #[test]
    fn pratt_and_right_assoc() {
        let e = ok("a && b && c");
        if let Expr::Binary {
            op: BinOp::And,
            lhs,
            rhs,
            ..
        } = e
        {
            assert!(matches!(*lhs, Expr::Ident(ref id) if id.text == "a"));
            assert!(matches!(*rhs, Expr::Binary { op: BinOp::And, .. }));
        } else {
            panic!("expected And, got {e:?}");
        }
    }

    /// Test 4: `a == b` → single Binary(Eq) — positive non-assoc.
    #[test]
    fn pratt_eq_non_assoc_single() {
        let e = ok("a == b");
        assert!(
            matches!(e, Expr::Binary { op: BinOp::Eq, .. }),
            "expected Eq, got {e:?}"
        );
    }

    /// Test 5: `a == b == c` → P009 (non-associative chain).
    #[test]
    fn pratt_eq_non_assoc_chain_rejects() {
        let e = err("a == b == c");
        assert_eq!(e.code(), "P009", "expected P009, got {e:?}");
    }

    /// Test 6: `a < b < c` → P009.
    #[test]
    fn pratt_lt_non_assoc_chain_rejects() {
        let e = err("a < b < c");
        assert_eq!(e.code(), "P009", "expected P009, got {e:?}");
    }

    /// Arithmetic on the left of a non-assoc comparison must NOT trigger P009.
    /// Regression test for the false-positive where any `(Binary _) op_non_assoc x`
    /// was rejected because `non_assoc_level` ignored its op argument and always
    /// returned 0 — `a + b == c`, `a + b != c`, `a * b < c`, … all reported P009.
    #[test]
    fn pratt_arith_then_eq_no_chain() {
        let e = ok("a + b == c");
        assert!(
            matches!(e, Expr::Binary { op: BinOp::Eq, .. }),
            "expected outer Eq, got {e:?}"
        );
    }

    #[test]
    fn pratt_arith_then_ne_no_chain() {
        let e = ok("a + b != c");
        assert!(
            matches!(e, Expr::Binary { op: BinOp::Ne, .. }),
            "expected outer Ne, got {e:?}"
        );
    }

    #[test]
    fn pratt_mul_then_lt_no_chain() {
        let e = ok("a * b < c");
        assert!(
            matches!(e, Expr::Binary { op: BinOp::Lt, .. }),
            "expected outer Lt, got {e:?}"
        );
    }

    /// Cross-level non-assoc chains stay rejected — both operands must be
    /// comparison ops for P009 to fire.
    #[test]
    fn pratt_lt_then_eq_still_rejects() {
        let e = err("a < b == c");
        assert_eq!(e.code(), "P009", "expected P009, got {e:?}");
    }

    /// Test 7: `"a" ++ "b" ++ "c"` → `Concat("a", Concat("b","c"))` (right-assoc).
    #[test]
    fn pratt_concat_right_assoc() {
        let e = ok("\"a\" ++ \"b\" ++ \"c\"");
        if let Expr::Binary {
            op: BinOp::Concat,
            rhs,
            ..
        } = e
        {
            assert!(matches!(
                *rhs,
                Expr::Binary {
                    op: BinOp::Concat,
                    ..
                }
            ));
        } else {
            panic!("expected Concat, got {e:?}");
        }
    }

    /// Test 8: `x :: y :: z` → `Cons(x, Cons(y, z))` (right-assoc).
    #[test]
    fn pratt_cons_right_assoc() {
        let e = ok("x :: y :: z");
        if let Expr::Binary {
            op: BinOp::Cons,
            rhs,
            ..
        } = e
        {
            assert!(matches!(
                *rhs,
                Expr::Binary {
                    op: BinOp::Cons,
                    ..
                }
            ));
        } else {
            panic!("expected Cons, got {e:?}");
        }
    }

    /// Test 9: `a + b + c` → `Add(Add(a,b), c)` (left-assoc).
    #[test]
    fn pratt_add_left_assoc() {
        let e = ok("a + b + c");
        if let Expr::Binary {
            op: BinOp::Add,
            lhs,
            rhs,
            ..
        } = e
        {
            assert!(matches!(*rhs, Expr::Ident(ref id) if id.text == "c"));
            assert!(matches!(*lhs, Expr::Binary { op: BinOp::Add, .. }));
        } else {
            panic!("expected Add, got {e:?}");
        }
    }

    /// Test 10: `1 + 2 * 3` → `Add(1, Mul(2,3))` — mul binds tighter.
    #[test]
    fn pratt_add_mul_precedence() {
        let e = ok("1 + 2 * 3");
        if let Expr::Binary {
            op: BinOp::Add,
            lhs,
            rhs,
            ..
        } = e
        {
            // lhs = Literal(1)
            assert!(matches!(*lhs, Expr::Literal(Literal::IntDec { ref raw, .. }) if raw == "1"));
            // rhs = Mul(2, 3)
            assert!(matches!(*rhs, Expr::Binary { op: BinOp::Mul, .. }));
        } else {
            panic!("expected Add at top, got {e:?}");
        }
    }

    /// Test 11: `a * b * c` → `Mul(Mul(a,b), c)` (left-assoc).
    #[test]
    fn pratt_mul_left_assoc() {
        let e = ok("a * b * c");
        if let Expr::Binary {
            op: BinOp::Mul,
            lhs,
            rhs,
            ..
        } = e
        {
            assert!(matches!(*rhs, Expr::Ident(ref id) if id.text == "c"));
            assert!(matches!(*lhs, Expr::Binary { op: BinOp::Mul, .. }));
        } else {
            panic!("expected Mul, got {e:?}");
        }
    }

    /// Test 12: `x ^ y ^ z` → `Pow(x, Pow(y,z))` (right-assoc).
    #[test]
    fn pratt_pow_right_assoc() {
        let e = ok("x ^ y ^ z");
        if let Expr::Binary {
            op: BinOp::Pow,
            lhs,
            rhs,
            ..
        } = e
        {
            assert!(matches!(*lhs, Expr::Ident(ref id) if id.text == "x"));
            assert!(matches!(*rhs, Expr::Binary { op: BinOp::Pow, .. }));
        } else {
            panic!("expected Pow, got {e:?}");
        }
    }

    /// Test 13: `-x + 1` → `Add(Neg(x), 1)`.
    #[test]
    fn pratt_unary_minus() {
        let e = ok("-x + 1");
        if let Expr::Binary {
            op: BinOp::Add,
            lhs,
            rhs,
            ..
        } = e
        {
            assert!(matches!(
                *lhs,
                Expr::Unary {
                    op: UnaryOp::Neg,
                    ..
                }
            ));
            assert!(matches!(*rhs, Expr::Literal(Literal::IntDec { ref raw, .. }) if raw == "1"));
        } else {
            panic!("expected Add(Neg(x), 1), got {e:?}");
        }
    }

    /// Test 14: `- -x` → `Neg(Neg(x))`.
    ///
    /// Note: `--` is a line comment in Ridge, so double negation requires a
    /// space: `- -x`.
    #[test]
    fn pratt_unary_minus_nested() {
        let e = ok("- -x");
        if let Expr::Unary {
            op: UnaryOp::Neg,
            expr,
            ..
        } = e
        {
            assert!(matches!(
                *expr,
                Expr::Unary {
                    op: UnaryOp::Neg,
                    ..
                }
            ));
        } else {
            panic!("expected Neg(Neg(x)), got {e:?}");
        }
    }

    /// Test 15: `f x y z` → `Call { callee: f, args: [x, y, z] }` (flat).
    #[test]
    fn pratt_call_juxta_flat() {
        let e = ok("f x y z");
        if let Expr::Call { callee, args, .. } = e {
            assert!(matches!(*callee, Expr::Ident(ref id) if id.text == "f"));
            assert_eq!(args.len(), 3, "expected 3 args, got {}", args.len());
            assert!(matches!(&args[0], Expr::Ident(id) if id.text == "x"));
            assert!(matches!(&args[1], Expr::Ident(id) if id.text == "y"));
            assert!(matches!(&args[2], Expr::Ident(id) if id.text == "z"));
        } else {
            panic!("expected Call, got {e:?}");
        }
    }

    /// Test 16: `f` alone → `Ident(f)` (no Call wrapper).
    #[test]
    fn pratt_call_zero_args_is_ident() {
        let e = ok("f");
        assert!(
            matches!(e, Expr::Ident(ref id) if id.text == "f"),
            "expected Ident(f), got {e:?}"
        );
    }

    /// Test 17: `a.b.c` → nested `FieldAccess`.
    #[test]
    fn pratt_field_access_chain() {
        let e = ok("a.b.c");
        if let Expr::FieldAccess { base, field, .. } = e {
            assert_eq!(field.text, "c");
            if let Expr::FieldAccess {
                base: inner_base,
                field: inner_field,
                ..
            } = *base
            {
                assert_eq!(inner_field.text, "b");
                assert!(matches!(*inner_base, Expr::Ident(ref id) if id.text == "a"));
            } else {
                panic!("expected inner FieldAccess");
            }
        } else {
            panic!("expected FieldAccess, got {e:?}");
        }
    }

    /// Test 18: `(.name)` → `FieldAccessorFn { field: "name" }`.
    #[test]
    fn pratt_field_accessor_fn() {
        let e = ok("(.name)");
        if let Expr::FieldAccessorFn { field, .. } = e {
            assert_eq!(field.text, "name");
        } else {
            panic!("expected FieldAccessorFn, got {e:?}");
        }
    }

    /// Test 19: `[]` → `Expr::List { elems: [] }`.
    #[test]
    fn pratt_list_literal_empty() {
        let e = ok("[]");
        assert!(
            matches!(e, Expr::List { ref elems, .. } if elems.is_empty()),
            "expected empty List, got {e:?}"
        );
    }

    /// Test 20: `[1, 2, 3]` → 3-element List.
    #[test]
    fn pratt_list_literal_elems() {
        let e = ok("[1, 2, 3]");
        if let Expr::List { elems, .. } = e {
            assert_eq!(elems.len(), 3);
        } else {
            panic!("expected List, got {e:?}");
        }
    }

    /// Test 21: `(1, 2)` → `Tuple { elems: [1, 2] }`.
    #[test]
    fn pratt_tuple_literal() {
        let e = ok("(1, 2)");
        if let Expr::Tuple { elems, .. } = e {
            assert_eq!(elems.len(), 2);
        } else {
            panic!("expected Tuple, got {e:?}");
        }
    }

    /// Test 22: `(1 + 2)` → `Paren { inner: Add(1, 2) }`.
    #[test]
    fn pratt_paren_single() {
        let e = ok("(1 + 2)");
        if let Expr::Paren { inner, .. } = e {
            assert!(matches!(*inner, Expr::Binary { op: BinOp::Add, .. }));
        } else {
            panic!("expected Paren, got {e:?}");
        }
    }

    /// Test 23: `1 + 2 * 3 ^ 2 - 4 / 2` — complex precedence tree.
    ///
    /// Expected (applying precedence): `(1 + (2 * (3 ^ 2))) - (4 / 2)`
    /// Top: `Sub(Add(1, Mul(2, Pow(3, 2))), Div(4, 2))`
    #[test]
    fn pratt_mixed_precedence_big() {
        let e = ok("1 + 2 * 3 ^ 2 - 4 / 2");
        // Top-level should be Sub (left-assoc: (1 + 2 * 3 ^ 2) - (4 / 2))
        if let Expr::Binary {
            op: BinOp::Sub,
            lhs,
            rhs,
            ..
        } = e
        {
            // rhs = Div(4, 2)
            assert!(matches!(*rhs, Expr::Binary { op: BinOp::Div, .. }));
            // lhs = Add(1, Mul(2, Pow(3, 2)))
            if let Expr::Binary {
                op: BinOp::Add,
                rhs: add_rhs,
                ..
            } = *lhs
            {
                assert!(matches!(*add_rhs, Expr::Binary { op: BinOp::Mul, .. }));
            } else {
                panic!("expected Add inside Sub.lhs");
            }
        } else {
            panic!("expected Sub at top, got {e:?}");
        }
    }

    /// Test 24: `users |> List.map (.name)` → `Pipe(users, Call(Qualified([List,map]), [FAFn(name)]))`.
    #[test]
    fn pratt_pipe_with_call_and_accessor() {
        let e = ok("users |> List.map (.name)");
        if let Expr::Pipe { lhs, rhs, .. } = e {
            // lhs = Ident(users)
            assert!(matches!(*lhs, Expr::Ident(ref id) if id.text == "users"));
            // rhs = Call { callee: Qualified([List, map]), args: [FieldAccessorFn(name)] }
            if let Expr::Call { callee, args, .. } = *rhs {
                assert!(matches!(
                    *callee,
                    Expr::Qualified(ref q) if q.segments.len() == 2
                        && q.segments[0].text == "List"
                        && q.segments[1].text == "map"
                ));
                assert_eq!(args.len(), 1);
                assert!(
                    matches!(&args[0], Expr::FieldAccessorFn { field, .. } if field.text == "name")
                );
            } else {
                panic!("expected Call on rhs of Pipe, got {rhs:?}", rhs = *rhs);
            }
        } else {
            panic!("expected Pipe, got {e:?}");
        }
    }

    // ── T6: Negative tests ────────────────────────────────────────────────────

    /// Test 25: `1 +` → P001 or P002 (missing RHS).
    #[test]
    fn pratt_add_missing_rhs() {
        let e = err("1 +");
        assert!(
            matches!(e.code(), "P001" | "P002"),
            "expected P001 or P002, got {e:?}"
        );
    }

    /// Test 26: `[1, 2` → P001 (missing `]`).
    #[test]
    fn pratt_list_missing_rbrack() {
        let e = err("[1, 2");
        assert_eq!(e.code(), "P001", "expected P001, got {e:?}");
    }

    /// Test 27: `(1, 2` → P001 (missing `)`).
    #[test]
    fn pratt_tuple_missing_rparen() {
        // `(1, 2` — the expect(RParen) will return Err(P001).
        let r = parse_e("(1, 2");
        assert!(r.is_err(), "expected Err, got Ok");
        assert_eq!(r.unwrap_err().code(), "P001");
    }

    // ── InnerFn expression ────────────────────────────────────────────────────

    /// `fn foo x = x + 1` at expression position → `Expr::InnerFn`.
    ///
    /// Scan-forward finds `=` before `->`, so `InnerFn` path is taken.
    #[test]
    fn parse_inner_fn_expr() {
        let e = ok("fn foo x = x");
        assert!(
            matches!(e, Expr::InnerFn { ref decl, .. } if decl.name.text == "foo"),
            "expected InnerFn with name 'foo', got {e:?}"
        );
        if let Expr::InnerFn { decl, .. } = e {
            assert_eq!(decl.params.len(), 1);
        }
    }

    /// `fn x -> x + 1` at expression position → `Expr::Lambda` (not `InnerFn`).
    ///
    /// Scan-forward finds `->` before `=`, so `LambdaExpr` path is taken.
    #[test]
    fn parse_lambda_not_inner_fn() {
        let e = ok("fn x -> x");
        assert!(
            matches!(e, Expr::Lambda { .. }),
            "expected Lambda, got {e:?}"
        );
    }

    /// `fn (x, y) -> body` at expression position → `Lambda` (not `InnerFn`).
    #[test]
    fn parse_lambda_pattern_param_not_inner_fn() {
        let e = ok("fn (x, y) -> x");
        assert!(
            matches!(e, Expr::Lambda { .. }),
            "expected Lambda, got {e:?}"
        );
    }

    // ── InnerFn disambiguation with return-type annotation ───────────────────

    /// `fn foo (x: Int) -> Int = x + 1` → `Expr::InnerFn` with return type.
    ///
    /// `fn_is_inner_fn` scans past `->` + Type and finds `=` → `InnerFn`.
    #[test]
    fn inner_fn_with_return_type() {
        let e = ok("fn foo (x: Int) -> Int = x + 1");
        assert!(
            matches!(e, Expr::InnerFn { ref decl, .. } if decl.name.text == "foo"),
            "expected InnerFn with name 'foo', got {e:?}"
        );
        if let Expr::InnerFn { decl, .. } = e {
            assert!(
                decl.ret.is_some(),
                "expected return type annotation, got None"
            );
        }
    }

    /// `fn foo x = x + 1` → `Expr::InnerFn` (no return type).
    ///
    /// Regression guard: the simpler `InnerFn` form (no `->`) still works.
    #[test]
    fn inner_fn_without_return_type() {
        let e = ok("fn foo x = x + 1");
        assert!(
            matches!(e, Expr::InnerFn { ref decl, .. } if decl.name.text == "foo"),
            "expected InnerFn with name 'foo', got {e:?}"
        );
        if let Expr::InnerFn { decl, .. } = e {
            assert!(
                decl.ret.is_none(),
                "expected no return type annotation, got Some"
            );
        }
    }

    /// `fn x -> x` → `Expr::Lambda` (no `=` before end of input).
    ///
    /// Regression guard: simple lambda without return annotation still Lambda.
    #[test]
    fn lambda_with_return_annotation_not_supported() {
        // Lambda: `->` followed by body expression, no `=`.
        let e = ok("fn x -> x");
        assert!(
            matches!(e, Expr::Lambda { .. }),
            "expected Lambda, got {e:?}"
        );
    }

    /// `fn x y -> x + y` → `Expr::Lambda`.
    ///
    /// Regression guard: multi-param lambda still Lambda.
    #[test]
    fn lambda_multi_param() {
        let e = ok("fn x y -> x + y");
        assert!(
            matches!(e, Expr::Lambda { .. }),
            "expected Lambda, got {e:?}"
        );
    }

    // ── Recursion-depth guard (P028) ─────────────────────────────────────────

    /// Parse `src` on a worker thread with a generous (8 MiB) stack — the same
    /// order of magnitude the compiler's main thread gets — and return the
    /// first parse error.
    ///
    /// The deep-nesting cases below feed thousands of nested constructs.  The
    /// `P028` guard stops the descent at 256 levels, but the default per-test
    /// stack the Rust harness hands out is far smaller than a real main thread,
    /// so we run these on an explicit large-stack thread to exercise the guard
    /// under realistic conditions rather than the harness's tiny default.
    fn err_big_stack(src: &str) -> ParseError {
        let owned = src.to_string();
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || err(&owned))
            .expect("spawn parser thread")
            .join()
            .expect("parser thread panicked (likely stack overflow — guard failed)")
    }

    /// Pathological nesting must be rejected with P028 instead of overflowing
    /// the native stack.  `10_000` nested parens is far past the 256 limit and
    /// would crash an unbounded recursive-descent parser; with the guard it
    /// returns a clean diagnostic and the process stays alive.
    #[test]
    fn deeply_nested_parens_reject_without_overflow() {
        let depth = 10_000;
        let src = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
        let e = err_big_stack(&src);
        assert_eq!(e.code(), "P028", "expected P028, got {e:?}");
    }

    /// Deeply nested lists are bounded by the same guard (the list parser
    /// re-enters the expression parser per element).
    #[test]
    fn deeply_nested_lists_reject_without_overflow() {
        let depth = 10_000;
        let src = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        let e = err_big_stack(&src);
        assert_eq!(e.code(), "P028", "expected P028, got {e:?}");
    }

    /// A long chain of binary operators also descends, so it is bounded too.
    #[test]
    fn deeply_chained_operators_reject_without_overflow() {
        // Right-associative `::` recurses once per element → deep descent.
        let src = format!("{}x", "x :: ".repeat(10_000));
        let e = err_big_stack(&src);
        assert_eq!(e.code(), "P028", "expected P028, got {e:?}");
    }

    /// Nesting comfortably under the limit must still parse cleanly — the guard
    /// must not introduce a false positive on ordinary programs.  Real code
    /// rarely nests beyond a handful of levels; 64 is already extreme.
    #[test]
    fn moderately_nested_parens_still_parse() {
        let depth = 64;
        let src = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
        let e = ok(&src);
        // Outermost layer is a Paren wrapping the next.
        assert!(
            matches!(e, Expr::Paren { .. }),
            "expected Paren at top, got {e:?}"
        );
    }

    /// A normally-nested expression (a few levels of mixed parens, lists, and
    /// operators) is unaffected by the guard.
    #[test]
    fn normally_nested_expression_parses_clean() {
        let e = ok("(1 + [2, (3 * 4)]) :: [5]");
        assert!(
            matches!(
                e,
                Expr::Binary {
                    op: BinOp::Cons,
                    ..
                }
            ),
            "expected Cons at top, got {e:?}"
        );
    }
}
