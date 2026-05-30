//! Type expression parsing (grammar §3.4).
//!
//! This module implements:
//!
//! - [`parse_type`] — entry point; handles right-associative `->`.
//! - [`parse_type_atom`] — parses a single `TypeAtom` (no `->` at this level).
//!
//! Internal helpers (not exported):
//! - `parse_type_app` — wraps `parse_type_atom` adding greedy argument
//!   collection for `UPPER_IDENT`-headed applications.
//! - `parse_fn_type` — handles `fn CAP* Atom+ -> Type` (`CapFunctionType`).
//! - `peek_capability` — matches a `LOWER_IDENT` against the capability set.
//!
//! # Primitive types
//!
//! The lexer does **not** emit dedicated keyword tokens for `Int`, `Float`,
//! `Bool`, `Text`, `Unit`, or `Timestamp`.  These are emitted as
//! `Token::UpperIdent("Int")` etc.  The parser recognises them by matching on
//! the identifier text inside `parse_type_atom`.
//!
//! # Capability tokens
//!
//! The lexer does **not** emit dedicated keyword tokens for `io`, `fs`, `net`,
//! etc.  These are emitted as `Token::LowerIdent("io")` etc.  The parser
//! recognises capability words in `peek_capability` by matching the text.
//!
//! # Right-associativity of `->`
//!
//! `parse_type` calls `parse_type_app` for the left side, then if `Arrow`
//! follows it recurses into `parse_type` for the right side.  This gives:
//!
//! ```text
//! Int -> Text -> Bool  ≡  Int -> (Text -> Bool)
//!   =>  Fn { params:[Int], ret: Fn { params:[Text], ret: Bool } }
//! ```
//!
//! # OQ-P003 (flat `TypeApp`)
//!
//! `Map k v` is `App { head: Map, args: [k, v] }` — not nested
//! `App(App(Map, k), v)`.  Greedy argument collection happens in
//! `parse_type_app`.

// These functions are called by the tests in this file and will be called from
// production code in T10 (type declarations / fn/const annotations).
// Suppress dead_code until all callers exist.
#![allow(dead_code)]
#![allow(clippy::redundant_pub_crate)]

use ridge_ast::{Capability, FnType, Ident, PrimitiveType, RecordTypeField, Type};
use ridge_lexer::Token;

use crate::{cursor::Cursor, error::ParseError};

// ── Public entry: parse_type ──────────────────────────────────────────────────

/// Parse a full type expression including right-associative `->` (grammar §3.4
/// `Type`).
///
/// Grammar:
/// ```ebnf
/// Type         ::= FunctionType | TypeAtom ;
/// FunctionType ::= CapFunctionType | PlainFunctionType ;
/// PlainFunctionType ::= TypeAtom "->" Type ;
/// CapFunctionType   ::= "fn" { Capability } TypeAtom { TypeAtom } "->" Type ;
/// ```
///
/// Entry point for all type positions in the grammar (field types, return
/// types, parameter annotations, etc.).
pub(crate) fn parse_type(cur: &mut Cursor<'_>) -> Result<Type, ParseError> {
    // `fn` keyword opens a CapFunctionType; delegate to `parse_fn_type`.
    if cur.peek() == &Token::KwFn {
        return parse_fn_type(cur);
    }

    // Otherwise parse an application-level atom on the left.
    let left = parse_type_app(cur)?;
    let left_span = left.span();

    // If `->` follows, right-recursively parse the return type.
    if cur.peek() == &Token::Arrow {
        cur.bump(); // consume `->`
        let ret = parse_type(cur)?;
        let full_span = left_span.merge(ret.span());
        return Ok(Type::Fn {
            fn_ty: FnType {
                caps: vec![],
                params: vec![left],
                ret: Box::new(ret),
                span: full_span,
            },
            span: full_span,
        });
    }

    Ok(left)
}

// ── Public entry: parse_type_atom ─────────────────────────────────────────────

/// Parse a single `TypeAtom` — a type that cannot appear as the left side of
/// `->` without parentheses (grammar §3.4 `TypeAtom`).
///
/// Grammar:
/// ```ebnf
/// TypeAtom ::= PrimitiveType
///            | TupleType
///            | ListTypeApply
///            | TypeApp
///            | "(" Type ")" ;
/// ```
///
/// This function does **not** consume trailing type arguments; use
/// [`parse_type_app`] (or [`parse_type`]) when you need greedy argument
/// collection for `UPPER_IDENT`-headed forms.
pub(crate) fn parse_type_atom(cur: &mut Cursor<'_>) -> Result<Type, ParseError> {
    let span = cur.span();

    match cur.peek() {
        // ── UPPER_IDENT: primitive or named type ──────────────────────────────
        Token::UpperIdent(_) => {
            let text = match cur.bump() {
                Token::UpperIdent(s) => s.clone(),
                _ => unreachable!(),
            };

            // Recognise built-in primitive types by their UPPER_IDENT spelling.
            // The lexer does NOT emit dedicated keyword tokens for these.
            let prim = match text.as_str() {
                "Int" => Some(PrimitiveType::Int),
                "Float" => Some(PrimitiveType::Float),
                "Bool" => Some(PrimitiveType::Bool),
                "Text" => Some(PrimitiveType::Text),
                "Unit" => Some(PrimitiveType::Unit),
                "Timestamp" => Some(PrimitiveType::Timestamp),
                _ => None,
            };

            prim.map_or_else(
                || {
                    Ok(Type::Named {
                        name: Ident::new(text.clone(), span),
                        span,
                    })
                },
                |name| Ok(Type::Primitive { name, span }),
            )
        }

        // ── LOWER_IDENT: type variable ────────────────────────────────────────
        Token::LowerIdent(_) => {
            let text = match cur.bump() {
                Token::LowerIdent(s) => s.clone(),
                _ => unreachable!(),
            };
            Ok(Type::Var {
                name: Ident::new(text, span),
                span,
            })
        }

        // ── `[Type]` list sugar ───────────────────────────────────────────────
        Token::LBrack => {
            cur.bump(); // consume `[`
            let elem = parse_type(cur)?;
            let rbrack_span = cur.expect(&Token::RBrack)?;
            Ok(Type::List {
                elem: Box::new(elem),
                span: span.merge(rbrack_span),
            })
        }

        // ── `(…)`: unit literal, paren, or tuple ─────────────────────────────
        Token::LParen => parse_paren_or_tuple(cur),

        // ── `{ … }`: inline record type ──────────────────────────────────────
        //
        // Grammar:
        //   inline-record-type ::= '{' '}'
        //                        | '{' record-type-field (',' record-type-field)* ','? '}'
        //   record-type-field  ::= LOWER_IDENT ':' type
        Token::LBrace => parse_inline_record_type(cur),

        // ── Everything else is not a valid atom start ─────────────────────────
        _ => Err(ParseError::UnexpectedToken {
            span,
            description: format!("expected a type, found `{}`", cur.peek()),
        }),
    }
}

// ── parse_type_app (internal) ─────────────────────────────────────────────────

/// Parse a type application: an `UPPER_IDENT` followed by zero or more
/// `TypeAtom` arguments.
///
/// Grammar §3.4 line 424: `TypeApp ::= UPPER_IDENT { TypeAtom }`.
///
/// Per OQ-P003 the application is **flat**: `Map k v` →
/// `App { head: Map, args: [k, v] }`.
///
/// If the leading atom is NOT a `Type::Named` (upper-case), no greedy
/// argument collection is attempted — the atom is returned as-is.
fn parse_type_app(cur: &mut Cursor<'_>) -> Result<Type, ParseError> {
    let atom = parse_type_atom(cur)?;

    // Greedy argument collection only when the head is a Named (UPPER_IDENT)
    // type constructor that is not a primitive.  Primitives like `Int` are
    // saturated — they cannot accept type arguments in valid Ridge code.
    match atom {
        Type::Named {
            ref name,
            span: head_span,
        } => {
            let head = name.clone();
            let mut args: Vec<Type> = Vec::new();

            // Collect following atoms while the peek is an atom-start token.
            while is_type_atom_start(cur) {
                args.push(parse_type_atom(cur)?);
            }

            if args.is_empty() {
                // No arguments: keep as `Named`.
                Ok(atom)
            } else {
                let last_span = args.last().map_or(head_span, Type::span);
                let full_span = head_span.merge(last_span);
                Ok(Type::App {
                    head,
                    args,
                    span: full_span,
                })
            }
        }
        // Non-Named atoms: return without greedy collection.
        other => Ok(other),
    }
}

// ── parse_fn_type (internal) ──────────────────────────────────────────────────

/// Parse a capability-annotated function type (grammar §3.4 `CapFunctionType`).
///
/// Syntax: `fn CAP* TypeAtom { TypeAtom } -> Type`
///
/// Examples:
/// - `fn io Text -> Unit`
/// - `fn io fs (Text -> Unit) -> Bool`
///
/// Precondition: `cur.peek() == &Token::KwFn`.
fn parse_fn_type(cur: &mut Cursor<'_>) -> Result<Type, ParseError> {
    let start_span = cur.span();
    cur.bump(); // consume `fn`

    // Collect capabilities: LowerIdent tokens in the capability set.
    let mut caps: Vec<Capability> = Vec::new();
    while let Some(cap) = peek_capability(cur) {
        caps.push(cap);
        cur.bump();
    }

    // Collect one or more TypeAtom params (required before `->` per grammar).
    if !is_type_atom_start(cur) {
        return Err(ParseError::Expected {
            span: cur.span(),
            expected: "<type>",
            found: cur.peek().to_string(),
        });
    }

    let mut params: Vec<Type> = Vec::new();
    params.push(parse_type_atom(cur)?);
    // Collect additional atoms until we hit `->` or a non-atom-start token.
    while is_type_atom_start(cur) && cur.peek() != &Token::Arrow {
        params.push(parse_type_atom(cur)?);
    }

    // Expect `->` before the return type.
    cur.expect(&Token::Arrow)?;

    // Parse the return type (can itself be a full type including `->` chains).
    let ret = parse_type(cur)?;
    let full_span = start_span.merge(ret.span());

    Ok(Type::Fn {
        fn_ty: FnType {
            caps,
            params,
            ret: Box::new(ret),
            span: full_span,
        },
        span: full_span,
    })
}

// ── parse_paren_or_tuple (internal) ──────────────────────────────────────────

/// Parse `(…)` — three cases:
///
/// 1. `()` — `Type::Primitive { name: Unit, span }` (grammar treats `()` as
///    the `Unit` primitive; see grammar §3.4 and T4 spec).
/// 2. `(Type)` — `Type::Paren { inner, span }` (preserved for round-trip).
/// 3. `(Type, Type, …)` with ≥ 2 elements — `Type::Tuple { elems, span }`.
///
/// Precondition: `cur.peek() == &Token::LParen`.
fn parse_paren_or_tuple(cur: &mut Cursor<'_>) -> Result<Type, ParseError> {
    let start_span = cur.span();
    cur.bump(); // consume `(`

    // Case 1: `()` → Unit primitive.
    if cur.peek() == &Token::RParen {
        let end_span = cur.span();
        cur.bump(); // consume `)`
        return Ok(Type::Primitive {
            name: PrimitiveType::Unit,
            span: start_span.merge(end_span),
        });
    }

    // Parse the first type.
    let first = parse_type(cur)?;

    // Case 3: tuple — `, Type` repeats until `)`.
    if cur.peek() == &Token::Comma {
        let mut elems = vec![first];
        while cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
            elems.push(parse_type(cur)?);
        }
        let end_span = cur.expect(&Token::RParen)?;
        return Ok(Type::Tuple {
            elems,
            span: start_span.merge(end_span),
        });
    }

    // Case 2: paren — single type, no comma.
    let end_span = cur.expect(&Token::RParen)?;
    Ok(Type::Paren {
        inner: Box::new(first),
        span: start_span.merge(end_span),
    })
}

// ── peek_capability (internal) ────────────────────────────────────────────────

/// If the current token is a `LowerIdent` whose text matches a capability
/// keyword, return the [`Capability`] variant.  Otherwise return `None`.
///
/// Does **not** advance the cursor.  Callers must call `cur.bump()` on match.
///
/// Capabilities are `LowerIdent` tokens — the lexer does NOT emit dedicated
/// `KwIo` / `KwFs` / … token variants.
fn peek_capability(cur: &Cursor<'_>) -> Option<Capability> {
    match cur.peek() {
        Token::LowerIdent(s) => match s.as_str() {
            "io" => Some(Capability::Io),
            "fs" => Some(Capability::Fs),
            "net" => Some(Capability::Net),
            "time" => Some(Capability::Time),
            "random" => Some(Capability::Random),
            "env" => Some(Capability::Env),
            "proc" => Some(Capability::Proc),
            "spawn" => Some(Capability::Spawn),
            "ffi" => Some(Capability::Ffi),
            _ => None,
        },
        _ => None,
    }
}

// ── is_type_atom_start (internal) ─────────────────────────────────────────────

/// Return `true` if the current token can begin a `TypeAtom`.
///
/// The set is: `UPPER_IDENT`, `LOWER_IDENT`, `[`, `(`.
///
/// Used to drive the greedy argument loop in `parse_type_app` and the param
/// loop in `parse_fn_type`.
fn is_type_atom_start(cur: &Cursor<'_>) -> bool {
    matches!(
        cur.peek(),
        Token::UpperIdent(_)
            | Token::LowerIdent(_)
            | Token::LBrack
            | Token::LParen
            // `{` is recognised as a "start" so a stranded inline-record body
            // in TyCon-argument position (`Result { … } Text`) routes to
            // `parse_type_atom` and the dedicated `P021` diagnostic, rather
            // than ending greedy argument collection and surfacing the
            // misleading `P001 expected =` further upstream.
            | Token::LBrace
    )
}

// ── parse_inline_record_type (internal) ──────────────────────────────────────

/// Parse an inline record type `{ field: Type, … }`.
///
/// Grammar:
/// ```ebnf
/// inline-record-type ::= '{' '}'
///                       | '{' record-type-field (',' record-type-field)* ','? '}' ;
/// record-type-field  ::= LOWER_IDENT ':' type ;
/// ```
///
/// P021 (`MalformedInlineRecordType`) is emitted when:
/// - A field name is not a lowercase identifier.
/// - The `:` separator is missing (`{ x Int }` instead of `{ x: Int }`).
/// - The body is unterminated (EOF or unexpected token before `}`).
///
/// Precondition: `cur.peek() == Token::LBrace`.
fn parse_inline_record_type(cur: &mut Cursor<'_>) -> Result<Type, ParseError> {
    let start_span = cur.span();
    cur.bump(); // consume `{`

    // Empty record type: `{}`.
    if cur.peek() == &Token::RBrace {
        let end_span = cur.span();
        cur.bump(); // consume `}`
        return Ok(Type::Record {
            fields: vec![],
            span: start_span.merge(end_span),
        });
    }

    let mut fields: Vec<RecordTypeField> = Vec::new();

    loop {
        let field_span = cur.span();

        // Parse field name — must be a lowercase identifier.
        let name_text = match cur.peek().clone() {
            Token::LowerIdent(s) => {
                cur.bump();
                s
            }
            tok => {
                return Err(ParseError::MalformedInlineRecordType {
                    span: field_span,
                    description: format!(
                        "expected a lowercase field name, found `{tok}`; write the field as `fieldName: Type`, for example `x: Int`"
                    ),
                });
            }
        };
        let name = Ident::new(name_text, field_span);

        // Must have `:` separator.
        if cur.peek() != &Token::Colon {
            return Err(ParseError::MalformedInlineRecordType {
                span: cur.span(),
                description: format!(
                    "expected `:` after field name `{}` in record type; write `{}: Type`",
                    name.text, name.text
                ),
            });
        }
        cur.bump(); // consume `:`

        // Parse the field type.
        let field_ty = parse_type(cur)?;
        let field_end = field_ty.span();

        fields.push(RecordTypeField {
            name,
            ty: field_ty,
            span: field_span.merge(field_end),
        });

        // Separator: `,` or end.
        if cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
            // Trailing comma before `}` — done.
            if cur.peek() == &Token::RBrace {
                break;
            }
        } else {
            // No comma: must be `}` next.
            break;
        }
    }

    // Expect closing `}`.
    if cur.peek() != &Token::RBrace {
        return Err(ParseError::MalformedInlineRecordType {
            span: cur.span(),
            description: format!(
                "unterminated inline record type; expected `}}` but found `{}`",
                cur.peek()
            ),
        });
    }
    let end_span = cur.span();
    cur.bump(); // consume `}`

    Ok(Type::Record {
        fields,
        span: start_span.merge(end_span),
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::Span;
    use ridge_lexer::tokenize;

    fn lex(src: &str) -> Vec<(Token, Span)> {
        tokenize(src).tokens
    }

    fn parse_ty(src: &str) -> Result<Type, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        parse_type(&mut cur)
    }

    // ── T1: primitive Int ────────────────────────────────────────────────────

    #[test]
    fn parse_type_primitive_int() {
        let result = parse_ty("Int");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(
            matches!(
                result,
                Ok(Type::Primitive {
                    name: PrimitiveType::Int,
                    ..
                })
            ),
            "expected Primitive::Int, got {result:?}"
        );
    }

    // ── T2: primitive Text ───────────────────────────────────────────────────

    #[test]
    fn parse_type_primitive_text() {
        let result = parse_ty("Text");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(
            matches!(
                result,
                Ok(Type::Primitive {
                    name: PrimitiveType::Text,
                    ..
                })
            ),
            "expected Primitive::Text, got {result:?}"
        );
    }

    // ── T3: unit via `()` ────────────────────────────────────────────────────

    #[test]
    fn parse_type_primitive_unit() {
        let result = parse_ty("()");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(
            matches!(
                result,
                Ok(Type::Primitive {
                    name: PrimitiveType::Unit,
                    ..
                })
            ),
            "expected Primitive::Unit from `()`, got {result:?}"
        );
    }

    // ── T4: named type (upper, no args) ─────────────────────────────────────

    #[test]
    fn parse_type_named_upper() {
        let result = parse_ty("User");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Named { name, .. }) = result {
            assert_eq!(name.text, "User");
        } else {
            unreachable!("expected Type::Named, got {result:?}");
        }
    }

    // ── T5: type variable (lower) ────────────────────────────────────────────

    #[test]
    fn parse_type_var_lower() {
        let result = parse_ty("a");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Var { name, .. }) = result {
            assert_eq!(name.text, "a");
        } else {
            unreachable!("expected Type::Var, got {result:?}");
        }
    }

    // ── T6: type application with one arg ────────────────────────────────────

    #[test]
    fn parse_type_app_one_arg() {
        // `Option Int` → App { head: Option, args: [Int] }
        let result = parse_ty("Option Int");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::App { head, args, .. }) = result {
            assert_eq!(head.text, "Option");
            assert_eq!(args.len(), 1);
            assert!(matches!(
                args[0],
                Type::Primitive {
                    name: PrimitiveType::Int,
                    ..
                }
            ));
        } else {
            unreachable!("expected Type::App, got {result:?}");
        }
    }

    // ── T7: OQ-P003 — flat two-arg application ───────────────────────────────

    #[test]
    fn parse_type_app_two_args_flat() {
        // `Map k v` → App { head: Map, args: [Var(k), Var(v)] }
        // OQ-P003 check: NOT App(App(Map, k), v)
        let result = parse_ty("Map k v");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::App { head, args, .. }) = result {
            assert_eq!(head.text, "Map");
            assert_eq!(args.len(), 2, "args should be flat [k, v], not nested");
            assert!(matches!(&args[0], Type::Var { name, .. } if name.text == "k"));
            assert!(matches!(&args[1], Type::Var { name, .. } if name.text == "v"));
        } else {
            unreachable!("expected Type::App, got {result:?}");
        }
    }

    // ── T8: list sugar `[Int]` ───────────────────────────────────────────────

    #[test]
    fn parse_type_list() {
        // `[Int]` → List { elem: Primitive(Int) }
        let result = parse_ty("[Int]");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::List { elem, .. }) = result {
            assert!(matches!(
                *elem,
                Type::Primitive {
                    name: PrimitiveType::Int,
                    ..
                }
            ));
        } else {
            unreachable!("expected Type::List, got {result:?}");
        }
    }

    // ── T9: two-element tuple ────────────────────────────────────────────────

    #[test]
    fn parse_type_tuple_two() {
        // `(Int, Text)` → Tuple { elems: [Int, Text] }
        let result = parse_ty("(Int, Text)");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Tuple { elems, .. }) = result {
            assert_eq!(elems.len(), 2);
            assert!(matches!(
                elems[0],
                Type::Primitive {
                    name: PrimitiveType::Int,
                    ..
                }
            ));
            assert!(matches!(
                elems[1],
                Type::Primitive {
                    name: PrimitiveType::Text,
                    ..
                }
            ));
        } else {
            unreachable!("expected Type::Tuple, got {result:?}");
        }
    }

    // ── T10: three-element tuple ─────────────────────────────────────────────

    #[test]
    fn parse_type_tuple_three() {
        // `(Int, Text, Bool)` → Tuple { elems: [Int, Text, Bool] }
        let result = parse_ty("(Int, Text, Bool)");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Tuple { elems, .. }) = result {
            assert_eq!(elems.len(), 3);
        } else {
            unreachable!("expected Type::Tuple, got {result:?}");
        }
    }

    // ── T11: paren — single type, no comma ──────────────────────────────────

    #[test]
    fn parse_type_paren_single() {
        // `(Int)` → Paren { inner: Primitive(Int) }  (NOT a tuple)
        let result = parse_ty("(Int)");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Paren { inner, .. }) = result {
            assert!(matches!(
                *inner,
                Type::Primitive {
                    name: PrimitiveType::Int,
                    ..
                }
            ));
        } else {
            unreachable!("expected Type::Paren (not Tuple), got {result:?}");
        }
    }

    // ── T12: right-associativity of `->` ────────────────────────────────────

    #[test]
    fn parse_type_arrow_right_assoc() {
        // `Int -> Text -> Bool` must parse as `Int -> (Text -> Bool)`.
        // => Fn { params:[Int], ret: Fn { params:[Text], ret: Bool } }
        let result = parse_ty("Int -> Text -> Bool");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Fn { fn_ty, .. }) = &result {
            assert_eq!(fn_ty.caps.len(), 0);
            assert_eq!(fn_ty.params.len(), 1);
            assert!(matches!(
                fn_ty.params[0],
                Type::Primitive {
                    name: PrimitiveType::Int,
                    ..
                }
            ));
            // The return type must itself be a function type.
            if let Type::Fn { fn_ty: inner, .. } = fn_ty.ret.as_ref() {
                assert_eq!(inner.params.len(), 1);
                assert!(matches!(
                    inner.params[0],
                    Type::Primitive {
                        name: PrimitiveType::Text,
                        ..
                    }
                ));
                assert!(matches!(
                    inner.ret.as_ref(),
                    Type::Primitive {
                        name: PrimitiveType::Bool,
                        ..
                    }
                ));
            } else {
                unreachable!("expected right-nested Fn, got {:?}", fn_ty.ret);
            }
        } else {
            unreachable!("expected Type::Fn, got {result:?}");
        }
    }

    // ── T13: fn with caps ────────────────────────────────────────────────────

    #[test]
    fn parse_type_fn_with_caps() {
        // `fn io fs Text -> Unit`
        // => Fn { caps:[Io,Fs], params:[Text], ret: Primitive(Unit) }
        let result = parse_ty("fn io fs Text -> Unit");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Fn { fn_ty, .. }) = &result {
            assert_eq!(fn_ty.caps, vec![Capability::Io, Capability::Fs]);
            assert_eq!(fn_ty.params.len(), 1);
            assert!(matches!(
                fn_ty.params[0],
                Type::Primitive {
                    name: PrimitiveType::Text,
                    ..
                }
            ));
            assert!(matches!(
                fn_ty.ret.as_ref(),
                Type::Primitive {
                    name: PrimitiveType::Unit,
                    ..
                }
            ));
        } else {
            unreachable!("expected Type::Fn, got {result:?}");
        }
    }

    // ── T14: fn with caps, nested paren fn type ──────────────────────────────

    #[test]
    fn parse_type_fn_with_caps_nested() {
        // `fn io (Text -> Unit) -> Bool`
        // => Fn { caps:[Io], params:[Paren { inner: Fn { Text -> Unit } }], ret: Bool }
        let result = parse_ty("fn io (Text -> Unit) -> Bool");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Fn { fn_ty, .. }) = &result {
            assert_eq!(fn_ty.caps, vec![Capability::Io]);
            assert_eq!(fn_ty.params.len(), 1);
            // The single param is a Paren wrapping a plain function type.
            if let Type::Paren { inner, .. } = &fn_ty.params[0] {
                assert!(
                    matches!(inner.as_ref(), Type::Fn { .. }),
                    "inner of Paren should be Fn, got {inner:?}"
                );
            } else {
                unreachable!(
                    "expected Paren param wrapping a Fn type, got {:?}",
                    fn_ty.params[0]
                );
            }
            assert!(matches!(
                fn_ty.ret.as_ref(),
                Type::Primitive {
                    name: PrimitiveType::Bool,
                    ..
                }
            ));
        } else {
            unreachable!("expected Type::Fn, got {result:?}");
        }
    }

    // ── T15: Result type variable application ───────────────────────────────

    #[test]
    fn parse_type_result_two_vars() {
        // `Result a e` → App { head: Result, args: [Var(a), Var(e)] }
        let result = parse_ty("Result a e");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::App { head, args, .. }) = result {
            assert_eq!(head.text, "Result");
            assert_eq!(args.len(), 2);
            assert!(matches!(&args[0], Type::Var { name, .. } if name.text == "a"));
            assert!(matches!(&args[1], Type::Var { name, .. } if name.text == "e"));
        } else {
            unreachable!("expected Type::App, got {result:?}");
        }
    }

    // ── Negative T16: missing `]` ────────────────────────────────────────────

    #[test]
    fn parse_type_missing_rbrack() {
        // `[Int` — no closing `]` → P001
        let result = parse_ty("[Int");
        assert!(result.is_err(), "expected Err, got {result:?}");
        if let Err(e) = result {
            assert_eq!(e.code(), "P001", "expected P001, got {e:?}");
        }
    }

    // ── Negative T17: arrow with no return type ──────────────────────────────

    #[test]
    fn parse_type_arrow_missing_ret() {
        // `Int ->` — no return type after `->` → P002
        let result = parse_ty("Int ->");
        assert!(result.is_err(), "expected Err, got {result:?}");
        if let Err(e) = result {
            // P001 (Expected) or P002 (UnexpectedToken) — both acceptable
            // for "nothing after `->`".
            assert!(
                e.code() == "P001" || e.code() == "P002",
                "expected P001 or P002, got {e:?}"
            );
        }
    }

    // ── Inline record type — two fields ─────────────────────────────────────

    #[test]
    fn parse_type_inline_record_two_fields() {
        let result = parse_ty("{ x: Int, y: Int }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Record { fields, .. }) = result {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].name.text, "x");
            assert!(matches!(
                fields[0].ty,
                Type::Primitive {
                    name: PrimitiveType::Int,
                    ..
                }
            ));
            assert_eq!(fields[1].name.text, "y");
        } else {
            panic!("expected Type::Record, got {result:?}");
        }
    }

    // ── Inline record type — empty {} ────────────────────────────────────────

    #[test]
    fn parse_type_inline_record_empty() {
        let result = parse_ty("{}");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Record { fields, .. }) = result {
            assert!(fields.is_empty());
        } else {
            panic!("expected Type::Record, got {result:?}");
        }
    }

    // ── Inline record type — trailing comma ──────────────────────────────────

    #[test]
    fn parse_type_inline_record_trailing_comma() {
        let result = parse_ty("{ x: Int, }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::Record { fields, .. }) = result {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name.text, "x");
        } else {
            panic!("expected Type::Record, got {result:?}");
        }
    }

    // ── Inline record type — as generic argument `Option { id: Int }` ────────

    #[test]
    fn parse_type_inline_record_as_generic_arg() {
        // `Option { id: Int }` should parse as App { head: Option, args: [Record { id: Int }] }
        let result = parse_ty("Option { id: Int }");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        if let Ok(Type::App { head, args, .. }) = result {
            assert_eq!(head.text, "Option");
            assert_eq!(args.len(), 1);
            assert!(matches!(&args[0], Type::Record { .. }));
        } else {
            panic!("expected Type::App, got {result:?}");
        }
    }

    // ── Malformed inline record type — missing colon → P021 ──────────────────

    #[test]
    fn parse_type_inline_record_malformed_missing_colon() {
        let result = parse_ty("{ x Int }");
        assert!(result.is_err(), "expected Err, got {result:?}");
        if let Err(e) = result {
            assert_eq!(e.code(), "P021", "expected P021, got {e:?}");
        }
    }

    // ── Malformed inline record type — uppercase field name → P021 ───────────

    #[test]
    fn parse_type_inline_record_malformed_uppercase_field() {
        let result = parse_ty("{ X: Int }");
        assert!(result.is_err(), "expected Err, got {result:?}");
        if let Err(e) = result {
            assert_eq!(e.code(), "P021", "expected P021, got {e:?}");
        }
    }
}
