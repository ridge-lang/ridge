//! Declaration parsers (grammar §§2–5).
//!
//! Entry points:
//!
//! - [`parse_item`]         — dispatch on keyword to the correct declaration parser.
//! - [`parse_visibility`]   — `pub` / `pub(internal)` / (nothing = Private).
//! - [`parse_import`]       — grammar §2.2.
//! - [`parse_const`]        — grammar §2.4.
//! - [`parse_type_decl`]    — grammar §3.1.
//! - [`parse_fn_decl`]      — grammar §4.1.
//! - [`parse_actor_decl`]   — grammar §5.1.
//! - [`parse_actor_body`]   — grammar §5.1 `ActorBody`.
//! - [`parse_state_decl`]   — grammar §5.2.
//! - [`parse_init_decl`]    — grammar §5.3.
//! - [`parse_on_handler`]   — grammar §5.4.
//! - [`parse_cap_list`]     — `{ Capability }` (zero or more).
//! - [`parse_param_top`]    — top-level parameter (bare or annotated only).
//!
//! # Top-level parameter enforcement
//!
//! [`parse_param_top`] rejects tuple/constructor patterns with `P012
//! TopLevelPatternParam`.  Only `LOWER_IDENT` bare params and
//! `(LOWER_IDENT : Type)` annotated params are accepted at top-level fn /
//! on / init position.
//!
//! # Doc-comment attachment
//!
//! All `doc` fields are set to `None` during declaration parsing.  A later
//! pass owns the `DocComment` token peeling and attaches comments to
//! declarations.  Each `parse_*` function accepts an explicit
//! `doc: Option<DocComment>` argument so the signature is ready for that pass.

#![allow(dead_code)]
#![allow(clippy::redundant_pub_crate)]

use ridge_ast::{
    ActorDecl, ActorMember, Body, Capability, ConstDecl, Constructor, DocComment, Expr, FieldDecl,
    FnDecl, Ident, ImportDecl, InitDecl, Item, ModulePath, OnHandler, Param, RecordTypeBody,
    StateDecl, TypeBody, TypeDecl, UnionTypeBody, Visibility,
};
use ridge_lexer::Token;

// ── @ffi attribute ────────────────────────────────────────────────────────────

/// Parsed `@ffi("module", "name", arity)` attribute.
///
/// Carries the three arguments before they are moved into `Body::Ffi`.
struct FfiAttr {
    module: String,
    name: String,
    arity: u32,
    /// Span covering the full `@ffi(…)` token sequence.
    span: ridge_ast::Span,
}

/// Attempt to parse an `@ffi(StringLit, StringLit, IntLit)` attribute.
///
/// Grammar (additive — no lexer change):
/// ```ebnf
/// Attr = "@" "ffi" "(" TextLit "," TextLit "," IntLit ")" ;
/// ```
///
/// The cursor must be positioned at `Token::At` when this function is called.
/// On success the cursor is advanced past the closing `)`.  On failure a
/// `ParseError` is returned and the cursor position is unspecified (the caller
/// should propagate the error and not re-use the cursor).
///
/// Precondition: `cur.peek() == &Token::At`.
fn parse_ffi_attr(cur: &mut Cursor<'_>) -> Result<FfiAttr, ParseError> {
    let start = cur.span();
    cur.expect(&Token::At)?; // consume `@`

    // Expect the literal identifier "ffi".
    let span = cur.span();
    match cur.peek().clone() {
        Token::LowerIdent(ref s) if s == "ffi" => {
            cur.bump(); // consume `ffi`
        }
        _ => {
            return Err(ParseError::Expected {
                span,
                expected: "`ffi`",
                found: cur.peek().to_string(),
            });
        }
    }

    cur.expect(&Token::LParen)?; // consume `(`

    // First argument: module name (TextLit).
    let module_span = cur.span();
    let module = match cur.peek().clone() {
        Token::TextLit(s) => {
            cur.bump();
            s
        }
        _ => {
            return Err(ParseError::Expected {
                span: module_span,
                expected: "<string literal (module name)>",
                found: cur.peek().to_string(),
            });
        }
    };

    cur.expect(&Token::Comma)?; // consume `,`

    // Second argument: function name (TextLit).
    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::TextLit(s) => {
            cur.bump();
            s
        }
        _ => {
            return Err(ParseError::Expected {
                span: name_span,
                expected: "<string literal (function name)>",
                found: cur.peek().to_string(),
            });
        }
    };

    cur.expect(&Token::Comma)?; // consume `,`

    // Third argument: arity (IntDec literal — only decimal integers allowed here).
    let arity_span = cur.span();
    let arity: u32 = match cur.peek().clone() {
        Token::IntDec(ref s) => {
            let raw = s.clone();
            cur.bump();
            raw.parse::<u32>()
                .map_err(|_| ParseError::UnexpectedToken {
                    span: arity_span,
                    description: format!(
                        "@ffi arity must be a non-negative integer that fits in u32, found `{raw}`"
                    ),
                })?
        }
        _ => {
            return Err(ParseError::Expected {
                span: arity_span,
                expected: "<integer literal (arity)>",
                found: cur.peek().to_string(),
            });
        }
    };

    let end = cur.expect(&Token::RParen)?; // consume `)`

    Ok(FfiAttr {
        module,
        name,
        arity,
        span: start.merge(end),
    })
}

/// Parse the header of a function declaration that is preceded by `@ffi(...)`.
///
/// Called from [`parse_item`] after the `@ffi` attribute has been successfully
/// parsed and consumed.  The cursor must be positioned at `Visibility` (either
/// `pub` or `fn`).
///
/// Grammar for this case:
/// ```ebnf
/// FnDecl = Visibility "fn" [CapList] Ident { Param } [ "->" Type ] ;
///          (* no "=" or body — forbidden when @ffi is present *)
/// ```
///
/// Returns `Err` with a descriptive `ParseError` if:
/// - `pub` is absent or a non-`fn` keyword follows,
/// - a `=` body is found after the header (P002: `@ffi` + body is forbidden),
/// - any other unexpected token is encountered.
///
/// Precondition: `cur.peek()` is at the first token of the visibility.
/// `ffi` carries the already-parsed attribute; `doc` is the preceding doc comment.
fn parse_fn_decl_ffi(
    cur: &mut Cursor<'_>,
    ffi: FfiAttr,
    doc: Option<DocComment>,
) -> Result<FnDecl, ParseError> {
    let start = ffi.span;

    // Visibility — must be `pub` for an FFI decl (private @ffi makes no sense
    // at the language level, though we do not hard-enforce it here — the
    // crate-path check in T3 will catch invalid usage anyway).
    let vis = parse_visibility(cur)?;

    cur.expect(&Token::KwFn)?; // consume `fn`

    // Capability list.
    let caps = parse_cap_list(cur);

    // Function name.
    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            Ident::new(s, name_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<function name>",
                found: cur.peek().to_string(),
            });
        }
    };

    // Parameters.
    let mut params: Vec<Param> = Vec::new();
    if cur.peek() == &Token::LParen && cur.peek_n(1) == Some(&Token::RParen) {
        cur.bump(); // consume `(`
        cur.bump(); // consume `)`
    } else {
        while can_start_param(cur) {
            params.push(parse_param_top(cur)?);
        }
    }

    // Optional return type `-> Type`.
    let ret = if cur.peek() == &Token::Arrow {
        cur.bump(); // consume `->`
        Some(parse_type(cur)?)
    } else {
        None
    };

    // Reject body: `@ffi` decls must NOT have an `=`-introduced body.
    if cur.peek() == &Token::Assign {
        return Err(ParseError::UnexpectedToken {
            span: cur.span(),
            description: "function annotated with `@ffi(...)` must not have an `=` body (T2 §5.2)"
                .to_string(),
        });
    }

    let end_span = ret.as_ref().map_or(name_span, ridge_ast::Type::span);

    Ok(FnDecl {
        vis,
        caps,
        name,
        params,
        ret,
        body: Body::Ffi {
            module: ffi.module,
            name: ffi.name,
            arity: ffi.arity,
        },
        span: start.merge(end_span),
        doc,
    })
}

use crate::{
    block::parse_block,
    ctrl::parse_branch_body,
    cursor::Cursor,
    error::ParseError,
    ty::{parse_type, parse_type_atom},
};

// ── parse_item ────────────────────────────────────────────────────────────────

/// Parse a single top-level item (grammar §2.1 `TopLevel`).
///
/// Precondition: leading `DocComment` tokens and `Newline` tokens have already
/// been consumed by the caller (`parse_module`).
///
/// `doc` carries any immediately-preceding doc comment (T10: always `None`).
/// `vis` carries any visibility modifier already parsed by the caller.
pub(crate) fn parse_item(
    cur: &mut Cursor<'_>,
    doc: Option<DocComment>,
    vis: Visibility,
) -> Result<Item, ParseError> {
    // ── @ffi attribute — must precede a `pub fn` declaration ─────────────────
    // When the current token is `@`, try to parse an `@ffi(...)` attribute.
    // The visibility passed in from the caller will be `Private` (because `@`
    // is not a visibility keyword); we re-parse the visibility after the attr.
    if cur.peek() == &Token::At {
        let ffi = parse_ffi_attr(cur)?;
        // Skip any Newline between the attribute line and the `pub fn` line.
        while cur.peek() == &Token::Newline {
            cur.bump();
        }
        return Ok(Item::Fn(parse_fn_decl_ffi(cur, ffi, doc)?));
    }

    match cur.peek() {
        Token::KwImport => Ok(Item::Import(parse_import(cur, doc)?)),
        Token::KwConst => Ok(Item::Const(parse_const(cur, vis, doc)?)),
        Token::KwType => Ok(Item::Type(parse_type_decl(cur, vis, doc)?)),
        Token::KwFn => Ok(Item::Fn(parse_fn_decl(cur, vis, doc)?)),
        Token::KwActor => Ok(Item::Actor(parse_actor_decl(cur, vis, doc)?)),

        // Reserved keywords deferred to 0.2.0.
        Token::KwClass => Err(ParseError::DeferredFeature {
            span: cur.span(),
            feature: "class",
            since: "0.2.0",
        }),
        Token::KwInstance => Err(ParseError::DeferredFeature {
            span: cur.span(),
            feature: "instance",
            since: "0.2.0",
        }),
        Token::KwDeriving => Err(ParseError::DeferredFeature {
            span: cur.span(),
            feature: "deriving",
            since: "0.2.0",
        }),

        other => Err(ParseError::UnexpectedToken {
            span: cur.span(),
            description: format!("expected a top-level declaration, found `{other}`"),
        }),
    }
}

// ── parse_visibility ──────────────────────────────────────────────────────────

/// Parse an optional visibility modifier (grammar §2.3).
///
/// ```ebnf
/// Visibility ::= "pub"
///              | "pub" "(" "internal" ")"
///              (* (nothing) = Private *)
/// ```
///
/// Returns `Visibility::Private` if no keyword is present.
pub(crate) fn parse_visibility(cur: &mut Cursor<'_>) -> Result<Visibility, ParseError> {
    if cur.peek() != &Token::KwPub {
        return Ok(Visibility::Private);
    }

    cur.bump(); // consume `pub`

    if cur.peek() == &Token::LParen {
        cur.bump(); // consume `(`

        // Expect the identifier "internal".
        let span = cur.span();
        match cur.peek().clone() {
            Token::LowerIdent(ref s) if s == "internal" => {
                cur.bump(); // consume `internal`
            }
            _ => {
                return Err(ParseError::Expected {
                    span,
                    expected: "`internal`",
                    found: cur.peek().to_string(),
                });
            }
        }

        cur.expect(&Token::RParen)?;
        return Ok(Visibility::PubInternal);
    }

    Ok(Visibility::Pub)
}

// ── parse_cap_list ────────────────────────────────────────────────────────────

/// Parse zero or more capability names (grammar §5.3 `CapList`).
///
/// Capabilities are `LowerIdent` tokens whose text is one of the nine capability
/// keywords.  Collection stops as soon as a non-capability token is seen.
pub(crate) fn parse_cap_list(cur: &mut Cursor<'_>) -> Vec<Capability> {
    let mut caps = Vec::new();
    while let Some(cap) = peek_capability(cur) {
        caps.push(cap);
        cur.bump();
    }
    caps
}

/// Peek at the current token and return a `Capability` if it is a capability
/// keyword, without advancing the cursor.
///
/// `spawn` has a dedicated lexer token (`Token::KwSpawn`) because it is also
/// used in spawn-expression position.  All other capabilities are emitted as
/// plain `LowerIdent` by the lexer, so both arms are needed here.
fn peek_capability(cur: &Cursor<'_>) -> Option<Capability> {
    match cur.peek() {
        // `spawn` is lexed as KwSpawn (it doubles as an expression keyword).
        Token::KwSpawn => Some(Capability::Spawn),
        Token::LowerIdent(s) => match s.as_str() {
            "io" => Some(Capability::Io),
            "fs" => Some(Capability::Fs),
            "net" => Some(Capability::Net),
            "time" => Some(Capability::Time),
            "random" => Some(Capability::Random),
            "env" => Some(Capability::Env),
            "proc" => Some(Capability::Proc),
            "spawn" => Some(Capability::Spawn), // fallback if ever emitted as LowerIdent
            "ffi" => Some(Capability::Ffi),
            _ => None,
        },
        _ => None,
    }
}

// ── parse_param_top ───────────────────────────────────────────────────────────

/// Parse a single top-level function parameter.
///
/// At the top-level fn declaration, `Param` is a bare name or an
/// annotated name only.  Full patterns (tuples, constructors) are **not**
/// allowed; use a `let` binding in the body instead.
///
/// Grammar §4.1 line 459:
/// ```ebnf
/// Param ::= LOWER_IDENT | PRIV_IDENT | "(" LOWER_IDENT ":" Type ")" ;
/// ```
///
/// Returns `Err(P012 TopLevelPatternParam)` if the form inside `(…)` is not
/// `LOWER_IDENT : Type` (e.g. if it is a tuple or constructor pattern).
pub(crate) fn parse_param_top(cur: &mut Cursor<'_>) -> Result<Param, ParseError> {
    let span = cur.span();

    match cur.peek() {
        // ── Bare identifier (LOWER_IDENT or `_`) ─────────────────────────────
        Token::LowerIdent(_) | Token::Underscore => {
            let text = match cur.bump() {
                Token::LowerIdent(s) => s.clone(),
                Token::Underscore => "_".to_string(),
                _ => unreachable!(),
            };
            Ok(Param::Bare(Ident::new(text, span)))
        }

        // ── Parenthesised form: `(name: Type)` or invalid pattern ────────────
        Token::LParen => {
            let start = cur.span();
            cur.bump(); // consume `(`

            // Disambiguate: look for `LOWER_IDENT :` (annotated param)
            // vs anything else (pattern — rejected with P012).
            match cur.peek().clone() {
                Token::LowerIdent(ref name_text) => {
                    // Check if next token (after the ident) is `:`.
                    match cur.peek_n(1) {
                        Some(Token::Colon) => {
                            // Annotated param: `(name: Type)`.
                            let name_span = cur.span();
                            let name_text = name_text.clone();
                            cur.bump(); // consume name
                            cur.bump(); // consume `:`
                            let ty = parse_type(cur)?;
                            let end_span = cur.expect(&Token::RParen)?;
                            Ok(Param::Annotated {
                                name: Ident::new(name_text, name_span),
                                ty,
                                span: start.merge(end_span),
                            })
                        }
                        _ => {
                            // Not `name:` — it's a pattern (P012 violation).
                            Err(ParseError::TopLevelPatternParam {
                                span: start.merge(cur.span()),
                            })
                        }
                    }
                }
                // Any other token inside `(` is a pattern — P012.
                _ => Err(ParseError::TopLevelPatternParam {
                    span: start.merge(cur.span()),
                }),
            }
        }

        _ => Err(ParseError::UnexpectedToken {
            span,
            description: format!("expected a parameter name, found `{}`", cur.peek()),
        }),
    }
}

/// Return `true` if the current token can begin a top-level parameter.
///
/// The set is: `LowerIdent`, `Underscore`, `LParen`.
pub(crate) fn can_start_param(cur: &Cursor<'_>) -> bool {
    matches!(
        cur.peek(),
        Token::LowerIdent(_) | Token::Underscore | Token::LParen
    )
}

// ── parse_import ──────────────────────────────────────────────────────────────

/// Parse an `import` declaration (grammar §2.2, `docs/grammar.ebnf`).
///
/// ```ebnf
/// ImportDecl ::= "import" ModulePath [ "as" UPPER_IDENT ] [ "(" ImportList ")" ] ;
/// ModulePath ::= UPPER_IDENT { "." UPPER_IDENT }
///              | LOWER_IDENT { "." ( LOWER_IDENT | UPPER_IDENT ) }
/// ImportList ::= ImportItem { "," ImportItem }
/// ImportItem ::= LOWER_IDENT | UPPER_IDENT
/// ```
///
/// `ImportItem` accepts both `LOWER_IDENT` (functions, constants) and
/// `UPPER_IDENT` (types, constructors), aligning with Haskell / Elm / Rust
/// import-list idioms.  Example:
/// `import std.net.http (Request, Response, listen, respond) as Http`.
///
/// Precondition: `cur.peek() == &Token::KwImport`.
#[allow(clippy::too_many_lines)] // Import parsing is inherently verbose due to multiple optional parts.
pub(crate) fn parse_import(
    cur: &mut Cursor<'_>,
    doc: Option<DocComment>,
) -> Result<ImportDecl, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwImport)?;

    // ── ModulePath ────────────────────────────────────────────────────────────
    // First segment: any identifier (upper or lower).
    let mut segments: Vec<Ident> = Vec::new();
    let seg_span = cur.span();
    let first_seg = match cur.peek().clone() {
        Token::LowerIdent(s) | Token::UpperIdent(s) => {
            cur.bump();
            Ident::new(s, seg_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<module name>",
                found: cur.peek().to_string(),
            });
        }
    };
    segments.push(first_seg);

    // Subsequent segments: `.` + (lower or upper ident).
    while cur.peek() == &Token::Dot {
        // Only consume Dot if followed by an identifier (not `(.name)` accessor).
        if matches!(
            cur.peek_n(1),
            Some(Token::LowerIdent(_) | Token::UpperIdent(_))
        ) {
            cur.bump(); // consume `.`
            let seg_span = cur.span();
            let seg = match cur.peek().clone() {
                Token::LowerIdent(s) | Token::UpperIdent(s) => {
                    cur.bump();
                    Ident::new(s, seg_span)
                }
                _ => unreachable!(),
            };
            segments.push(seg);
        } else {
            break;
        }
    }

    let path_span = segments.iter().fold(start, |acc, seg| acc.merge(seg.span));
    let path = ModulePath {
        span: path_span,
        segments,
    };

    // ── Optional `as UPPER_IDENT` ─────────────────────────────────────────────
    let alias = if cur.peek() == &Token::KwAs {
        cur.bump(); // consume `as`
        let alias_span = cur.span();
        match cur.peek().clone() {
            Token::UpperIdent(s) => {
                cur.bump();
                Some(Ident::new(s, alias_span))
            }
            Token::LowerIdent(s) => {
                // Allow lowercase alias too (grammar says UPPER_IDENT but be lenient).
                cur.bump();
                Some(Ident::new(s, alias_span))
            }
            _ => {
                return Err(ParseError::Expected {
                    span: cur.span(),
                    expected: "<alias identifier>",
                    found: cur.peek().to_string(),
                });
            }
        }
    } else {
        None
    };

    // ── Optional `( ImportList )` ─────────────────────────────────────────────
    let items = if cur.peek() == &Token::LParen {
        cur.bump(); // consume `(`
        let mut list: Vec<Ident> = Vec::new();

        // Allow empty `()`.
        if cur.peek() != &Token::RParen {
            // First item.
            let item_span = cur.span();
            match cur.peek().clone() {
                Token::LowerIdent(s) | Token::UpperIdent(s) => {
                    cur.bump();
                    list.push(Ident::new(s, item_span));
                }
                _ => {
                    return Err(ParseError::Expected {
                        span: cur.span(),
                        expected: "<imported name>",
                        found: cur.peek().to_string(),
                    });
                }
            }
            // Subsequent items separated by `,`.
            while cur.peek() == &Token::Comma {
                cur.bump(); // consume `,`
                if cur.peek() == &Token::RParen {
                    break; // trailing comma allowed
                }
                let item_span = cur.span();
                match cur.peek().clone() {
                    Token::LowerIdent(s) | Token::UpperIdent(s) => {
                        cur.bump();
                        list.push(Ident::new(s, item_span));
                    }
                    _ => {
                        return Err(ParseError::Expected {
                            span: cur.span(),
                            expected: "<imported name>",
                            found: cur.peek().to_string(),
                        });
                    }
                }
            }
        }

        let end = cur.expect(&Token::RParen)?;
        let _ = end;
        Some(list)
    } else {
        None
    };

    let end_span = cur.span();
    Ok(ImportDecl {
        path,
        alias,
        items,
        span: start.merge(end_span),
        doc,
    })
}

// ── parse_const ───────────────────────────────────────────────────────────────

/// Parse a constant declaration (grammar §2.4 line 340).
///
/// ```ebnf
/// ConstDecl ::= [ Visibility ] "const" LOWER_IDENT ":" Type "=" Expr ;
/// ```
///
/// Returns `P005 MissingType` if `:` is absent.
///
/// Precondition: `cur.peek() == &Token::KwConst`.
pub(crate) fn parse_const(
    cur: &mut Cursor<'_>,
    vis: Visibility,
    doc: Option<DocComment>,
) -> Result<ConstDecl, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwConst)?;

    // Name: LOWER_IDENT.
    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            Ident::new(s, name_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<constant name>",
                found: cur.peek().to_string(),
            });
        }
    };

    // Required `:`.
    if cur.peek() != &Token::Colon {
        return Err(ParseError::MissingType {
            span: cur.span(),
            context: "const",
        });
    }
    cur.bump(); // consume `:`

    let ty = parse_type(cur)?;

    cur.expect(&Token::Assign)?;

    let value = parse_branch_body(cur)?;
    let end_span = value.span();

    Ok(ConstDecl {
        vis,
        name,
        ty,
        value,
        span: start.merge(end_span),
        doc,
    })
}

// ── parse_type_decl ───────────────────────────────────────────────────────────

/// Parse a type declaration (grammar §3.1 line 352).
///
/// ```ebnf
/// TypeDecl  ::= [ Visibility ] "type" UPPER_IDENT { TypeParam } "=" TypeBody ;
/// TypeParam ::= LOWER_IDENT ;
/// TypeBody  ::= RecordType | UnionType | Type ;
/// ```
///
/// Precondition: `cur.peek() == &Token::KwType`.
pub(crate) fn parse_type_decl(
    cur: &mut Cursor<'_>,
    vis: Visibility,
    doc: Option<DocComment>,
) -> Result<TypeDecl, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwType)?;

    // Name: UPPER_IDENT.
    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::UpperIdent(s) => {
            cur.bump();
            Ident::new(s, name_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<type name>",
                found: cur.peek().to_string(),
            });
        }
    };

    // Collect type parameters: zero or more LOWER_IDENT until `=`.
    let mut params: Vec<Ident> = Vec::new();
    while matches!(cur.peek(), Token::LowerIdent(_)) {
        let p_span = cur.span();
        if let Token::LowerIdent(s) = cur.bump().clone() {
            params.push(Ident::new(s, p_span));
        }
    }

    cur.expect(&Token::Assign)?;

    let body = parse_type_body(cur)?;
    let end_span = match &body {
        TypeBody::Record(r) => r.span,
        TypeBody::Union(u) => u.span,
        TypeBody::Alias(t) => t.span(),
    };

    Ok(TypeDecl {
        vis,
        name,
        params,
        body,
        span: start.merge(end_span),
        doc,
    })
}

/// Parse a type body: record `{ … }`, union `| A | B`, or alias `Type`.
fn parse_type_body(cur: &mut Cursor<'_>) -> Result<TypeBody, ParseError> {
    match cur.peek() {
        // Record type: `{ field: Type, … }`
        Token::LBrace => Ok(TypeBody::Record(parse_record_type_body(cur)?)),

        // Union type: leading `|` (optional) or `UPPER_IDENT` starting a constructor.
        // Leading `|` is optional; trailing `|` is forbidden.
        Token::Pipe => Ok(TypeBody::Union(parse_union_type_body(cur)?)),

        // UPPER_IDENT can start a union (no leading `|`) or an alias type.
        //
        // The one-token lookahead (`peek_n(1) == Pipe`) only handles nullary
        // constructors such as `Red | Green | Blue`. For positional constructors
        // like `Circle Int | Rectangle Int Int` the pipe sits beyond the first
        // constructor's type arguments, so a deeper scan is required.
        //
        // `looks_like_union` scans forward past type-atom tokens (handling
        // balanced brackets/parens) and returns `true` if it finds a `|`
        // before any line terminator. The disambiguation rule is preserved:
        // `type Wrapper = Inner Int` (single constructor, no `|`) is still
        // treated as a type alias.
        Token::UpperIdent(_) => {
            if looks_like_union(cur) {
                Ok(TypeBody::Union(parse_union_type_body(cur)?))
            } else {
                Ok(TypeBody::Alias(parse_type(cur)?))
            }
        }

        // All other starters (LOWER_IDENT, `[`, `(`, `fn`) → alias.
        _ => Ok(TypeBody::Alias(parse_type(cur)?)),
    }
}

/// Return `true` when the token stream starting at the current cursor position
/// looks like the body of a union type declaration.
///
/// Scans forward past type-atom tokens — `UPPER_IDENT`, `LOWER_IDENT`, `[…]`,
/// and `(…)` — tracking bracket/paren depth.  Returns `true` if a `|` is found
/// at depth 0 before any line terminator (`Newline`, `Dedent`, `Eof`, `Assign`).
///
/// The depth bound of 64 prevents pathological inputs from scanning an entire
/// file; returning `false` in that case safely falls through to the alias branch.
///
/// Does NOT consume any tokens — uses `peek_n(n)` for pure lookahead.
fn looks_like_union(cur: &Cursor<'_>) -> bool {
    let mut n: usize = 0;
    let mut depth: i32 = 0;
    loop {
        if n > 64 {
            return false;
        }
        match cur.peek_n(n) {
            None | Some(Token::Newline) | Some(Token::Dedent) | Some(Token::Eof)
            | Some(Token::Assign) => return false,
            Some(Token::Pipe) if depth == 0 => return true,
            Some(Token::LParen) | Some(Token::LBrack) => {
                depth += 1;
                n += 1;
            }
            Some(Token::RParen) | Some(Token::RBrack) => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
                n += 1;
            }
            // Any token inside brackets: keep scanning.
            _ if depth > 0 => {
                n += 1;
            }
            // Type-atom tokens at depth 0: skip past them.
            Some(Token::UpperIdent(_)) | Some(Token::LowerIdent(_)) => {
                n += 1;
            }
            // Anything else at depth 0 is not a type-atom; stop.
            _ => return false,
        }
    }
}

/// Parse a record type body `{ FieldDecl, … }` (grammar §3.2 line 363).
fn parse_record_type_body(cur: &mut Cursor<'_>) -> Result<RecordTypeBody, ParseError> {
    let start = cur.span();
    cur.expect(&Token::LBrace)?;

    let mut fields: Vec<FieldDecl> = Vec::new();

    // Parse at least one field; trailing comma allowed.
    loop {
        // Skip any Newline tokens inside `{ … }` (bracket suppression handles layout).
        while cur.peek() == &Token::Newline {
            cur.bump();
        }
        if cur.peek() == &Token::RBrace {
            break;
        }

        let field_start = cur.span();
        let field_name = match cur.peek().clone() {
            Token::LowerIdent(s) => {
                cur.bump();
                Ident::new(s, field_start)
            }
            _ => {
                return Err(ParseError::Expected {
                    span: cur.span(),
                    expected: "<field name>",
                    found: cur.peek().to_string(),
                });
            }
        };

        cur.expect(&Token::Colon)?;

        let ty = parse_type(cur)?;
        let field_end = ty.span();

        fields.push(FieldDecl {
            name: field_name,
            ty,
            span: field_start.merge(field_end),
        });

        // Consume optional comma then loop.
        if cur.peek() == &Token::Comma {
            cur.bump();
        } else {
            // No comma: expect `}` or Newline then `}`.
            while cur.peek() == &Token::Newline {
                cur.bump();
            }
            break;
        }
    }

    let end = cur.expect(&Token::RBrace)?;
    Ok(RecordTypeBody {
        fields,
        span: start.merge(end),
    })
}

/// Parse a union type body (grammar §3.3 line 376).
///
/// Leading `|` is optional; trailing `|` is forbidden; min 1 alternative.
///
/// ```ebnf
/// UnionType ::= [ "|" ] Constructor { "|" Constructor } ;
/// ```
fn parse_union_type_body(cur: &mut Cursor<'_>) -> Result<UnionTypeBody, ParseError> {
    let start = cur.span();

    // Consume optional leading `|`.
    if cur.peek() == &Token::Pipe {
        cur.bump();
    }

    let mut alts: Vec<Constructor> = Vec::new();

    // Parse first constructor (required).
    alts.push(parse_constructor(cur)?);

    // Parse subsequent constructors separated by `|`.
    while cur.peek() == &Token::Pipe {
        let pipe_span = cur.span();
        cur.bump(); // consume `|`

        // Trailing `|` is forbidden.
        match cur.peek() {
            Token::Newline | Token::Dedent | Token::Eof | Token::Assign => {
                return Err(ParseError::UnexpectedToken {
                    span: pipe_span,
                    description: "unexpected trailing `|` in union type".to_string(),
                });
            }
            _ => {}
        }

        alts.push(parse_constructor(cur)?);
    }

    let end_span = alts.last().map_or(start, |c| match c {
        Constructor::Positional { span, .. } | Constructor::Record { span, .. } => *span,
    });

    Ok(UnionTypeBody {
        alternatives: alts,
        span: start.merge(end_span),
    })
}

/// Parse a single union constructor (grammar §3.3 line 378).
///
/// ```ebnf
/// Constructor ::= UPPER_IDENT { Type }   (* positional *)
///               | UPPER_IDENT RecordType  (* inline record *)
/// ```
fn parse_constructor(cur: &mut Cursor<'_>) -> Result<Constructor, ParseError> {
    let start = cur.span();
    let name = match cur.peek().clone() {
        Token::UpperIdent(s) => {
            cur.bump();
            Ident::new(s, start)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<constructor name>",
                found: cur.peek().to_string(),
            });
        }
    };

    // Record constructor?
    if cur.peek() == &Token::LBrace {
        let body = parse_record_type_body(cur)?;
        let span = start.merge(body.span);
        return Ok(Constructor::Record { name, body, span });
    }

    // Positional constructor: greedily collect TypeAtom arguments
    // until `|`, `Newline`, `Dedent`, `Eof`, or `Assign`.
    let mut args: Vec<ridge_ast::Type> = Vec::new();
    while can_start_type_atom(cur) {
        args.push(parse_type_atom(cur)?);
    }

    let end_span = args.last().map_or(start, ridge_ast::Type::span);
    Ok(Constructor::Positional {
        name,
        args,
        span: start.merge(end_span),
    })
}

/// Return `true` if the current token can begin a `TypeAtom`.
fn can_start_type_atom(cur: &Cursor<'_>) -> bool {
    matches!(
        cur.peek(),
        Token::UpperIdent(_) | Token::LowerIdent(_) | Token::LBrack | Token::LParen
    )
}

// ── parse_fn_decl ─────────────────────────────────────────────────────────────

/// Parse a function declaration (grammar §4.1 line 450).
///
/// ```ebnf
/// FnDecl ::= [ Visibility ] "fn" { Capability } FnName { Param } [ "->" Type ] "=" Body ;
/// FnName ::= LOWER_IDENT | PRIV_IDENT ;
/// Body   ::= Expr ;
/// ```
///
/// Precondition: `cur.peek() == &Token::KwFn`.
/// `vis` is already parsed by the caller; `doc` is set to `None` in T10.
pub(crate) fn parse_fn_decl(
    cur: &mut Cursor<'_>,
    vis: Visibility,
    doc: Option<DocComment>,
) -> Result<FnDecl, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwFn)?;

    // Capability list.
    let caps = parse_cap_list(cur);

    // Function name: LOWER_IDENT (including `_foo` private names).
    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            Ident::new(s, name_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<function name>",
                found: cur.peek().to_string(),
            });
        }
    };

    // Parameters: zero or more Params.
    // `()` is the zero-param marker; consume it and leave params empty.
    let mut params: Vec<Param> = Vec::new();
    if cur.peek() == &Token::LParen && cur.peek_n(1) == Some(&Token::RParen) {
        cur.bump(); // consume `(`
        cur.bump(); // consume `)`
    } else {
        while can_start_param(cur) {
            params.push(parse_param_top(cur)?);
        }
    }

    // Optional return type `-> Type`.
    let ret = if cur.peek() == &Token::Arrow {
        cur.bump(); // consume `->`
        Some(parse_type(cur)?)
    } else {
        None
    };

    // `=` then body (inline or INDENT-delimited block).
    cur.expect(&Token::Assign)?;

    let expr = parse_branch_body(cur)?;
    let end_span = expr.span();

    Ok(FnDecl {
        vis,
        caps,
        name,
        params,
        ret,
        body: Body::Expr(expr),
        span: start.merge(end_span),
        doc,
    })
}

// ── parse_actor_decl ──────────────────────────────────────────────────────────

/// Parse an actor declaration (grammar §5.1 line 487).
///
/// ```ebnf
/// ActorDecl ::= [ Visibility ] "actor" UPPER_IDENT "=" ActorBody ;
/// ActorBody ::= INDENT { ActorMember NEWLINE } DEDENT ;
/// ```
///
/// Precondition: `cur.peek() == &Token::KwActor`.
pub(crate) fn parse_actor_decl(
    cur: &mut Cursor<'_>,
    vis: Visibility,
    doc: Option<DocComment>,
) -> Result<ActorDecl, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwActor)?;

    // Name: UPPER_IDENT.
    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::UpperIdent(s) => {
            cur.bump();
            Ident::new(s, name_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<actor name>",
                found: cur.peek().to_string(),
            });
        }
    };

    cur.expect(&Token::Assign)?;

    let mut member_errors: Vec<ParseError> = Vec::new();
    let members = parse_actor_body_recovering(cur, &mut member_errors);
    // member_errors are collected but we can only propagate one upward here.
    // The module-level loop in parse_module calls sync_to_next_item after an
    // error so we propagate the first one and drop the rest — the plan's
    // "at most one diagnostic per statement boundary" principle means actor
    // bodies would ideally surface all errors, but the current ParseResult
    // architecture requires callers to aggregate via parse_module.  For now,
    // the first member error is returned and subsequent ones are silently
    // dropped; future T14 work can add an errors_out parameter to the
    // top-level parse_item chain if needed.
    if let Some(first_err) = member_errors.into_iter().next() {
        return Err(first_err);
    }

    let end_span = members.last().map_or(name_span, |m| match m {
        ActorMember::State(s) => s.span,
        ActorMember::Init(i) => i.span,
        ActorMember::On(o) => o.span,
    });

    Ok(ActorDecl {
        vis,
        name,
        members,
        span: start.merge(end_span),
        doc,
    })
}

// ── parse_actor_body ──────────────────────────────────────────────────────────

/// Parse the body of an actor (grammar §5.1 line 489).
///
/// ```ebnf
/// ActorBody ::= INDENT { ActorMember NEWLINE } DEDENT ;
/// ```
///
/// Precondition: `cur.peek() == &Token::Indent`.
///
/// This function short-circuits on the first error (original behaviour).
/// Use [`parse_actor_body_recovering`] when multiple errors should be
/// collected (T12 recovery path).
pub(crate) fn parse_actor_body(cur: &mut Cursor<'_>) -> Result<Vec<ActorMember>, ParseError> {
    let mut errors: Vec<ParseError> = Vec::new();
    let members = parse_actor_body_recovering(cur, &mut errors);
    errors.into_iter().next().map_or(Ok(members), Err)
}

/// Parse the body of an actor, collecting errors into `errors_out` instead of
/// aborting on the first failure (T12 panic-mode recovery).
///
/// On member parse failure, tokens are skipped to the next `state`/`init`/`on`
/// keyword or `DEDENT` (§4.7 actor-body sync points).
fn parse_actor_body_recovering(
    cur: &mut Cursor<'_>,
    errors_out: &mut Vec<ParseError>,
) -> Vec<ActorMember> {
    match cur.expect(&Token::Indent) {
        Ok(_) => {}
        Err(e) => {
            errors_out.push(e);
            return vec![];
        }
    }

    let mut members: Vec<ActorMember> = Vec::new();

    loop {
        // Skip blank lines (Newline tokens) inside the actor body.
        while cur.peek() == &Token::Newline {
            cur.bump();
        }
        if cur.peek() == &Token::Dedent {
            break;
        }
        if cur.at_eof() {
            break;
        }

        match parse_actor_member(cur) {
            Ok(member) => members.push(member),
            Err(e) => {
                errors_out.push(e);
                // Recovery: skip to next actor-member keyword or DEDENT.
                sync_to_next_actor_member(cur);
            }
        }

        // Consume trailing Newline after member.
        if cur.peek() == &Token::Newline {
            cur.bump();
        }
    }

    match cur.expect(&Token::Dedent) {
        Ok(_) => {}
        Err(e) => errors_out.push(e),
    }

    members
}

/// Skip tokens to the next actor-member sync point: `state`/`init`/`on`
/// keyword, `DEDENT`, or `EOF`.
fn sync_to_next_actor_member(cur: &mut Cursor<'_>) {
    loop {
        match cur.peek() {
            // All actor-member sync points: return and let the caller handle.
            Token::KwState
            | Token::KwInit
            | Token::KwOn
            | Token::Dedent
            | Token::Eof
            | Token::Newline => return,
            _ => {
                cur.bump();
            }
        }
    }
}

/// Parse a single actor member (grammar §5.1 line 493).
fn parse_actor_member(cur: &mut Cursor<'_>) -> Result<ActorMember, ParseError> {
    match cur.peek() {
        Token::KwState => Ok(ActorMember::State(parse_state_decl(cur)?)),
        Token::KwInit => Ok(ActorMember::Init(parse_init_decl(cur)?)),
        Token::KwOn => Ok(ActorMember::On(parse_on_handler(cur, None)?)),
        _ => Err(ParseError::UnexpectedToken {
            span: cur.span(),
            description: format!(
                "expected `state`, `init`, or `on` in actor body, found `{}`",
                cur.peek()
            ),
        }),
    }
}

// ── parse_state_decl ──────────────────────────────────────────────────────────

/// Parse a `state` declaration (grammar §5.2 line 500).
///
/// Grammar: `"state" LOWER_IDENT ":" Type "=" Expr`
///
/// The plan allows `default` to be `None` (OQ-P006 — permissive parse for the
/// case where an `init` block provides the initial value at runtime).
///
/// Precondition: `cur.peek() == &Token::KwState`.
pub(crate) fn parse_state_decl(cur: &mut Cursor<'_>) -> Result<StateDecl, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwState)?;

    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            Ident::new(s, name_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<state field name>",
                found: cur.peek().to_string(),
            });
        }
    };

    cur.expect(&Token::Colon)?;

    let ty = parse_type(cur)?;

    // Optional `= Expr` default (OQ-P006: permissive).
    let default = if cur.peek() == &Token::Assign {
        cur.bump(); // consume `=`
        Some(parse_branch_body(cur)?)
    } else {
        None
    };

    let end_span = default
        .as_ref()
        .map_or_else(|| ty.span(), |e: &Expr| e.span());

    Ok(StateDecl {
        name,
        ty,
        default,
        span: start.merge(end_span),
    })
}

// ── parse_init_decl ───────────────────────────────────────────────────────────

/// Parse an `init` declaration (grammar §5.3 line 511).
///
/// Note: The grammar shows `"init" [ CapList ] "(" [ ParamList ] ")" "=" Block`,
/// but the canonical example (`rate_limiter.ridge`) uses separate annotated params
/// `init (cap: Int) (rate: Float) =`, consistent with `FnDecl` param style.
/// We implement the fn-param style (zero or more `Param`s) to match actual usage.
///
/// Precondition: `cur.peek() == &Token::KwInit`.
pub(crate) fn parse_init_decl(cur: &mut Cursor<'_>) -> Result<InitDecl, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwInit)?;

    let caps = parse_cap_list(cur);

    // Parameters: zero or more top-level params (same as fn).
    // `()` is the zero-param marker; consume it and leave params empty.
    let mut params: Vec<Param> = Vec::new();
    if cur.peek() == &Token::LParen && cur.peek_n(1) == Some(&Token::RParen) {
        cur.bump(); // consume `(`
        cur.bump(); // consume `)`
    } else {
        while can_start_param(cur) {
            params.push(parse_param_top(cur)?);
        }
    }

    cur.expect(&Token::Assign)?;

    let body = parse_block(cur)?;
    let end_span = body.span;

    Ok(InitDecl {
        caps,
        params,
        body,
        span: start.merge(end_span),
    })
}

// ── parse_on_handler ──────────────────────────────────────────────────────────

/// Parse an `on` message handler (grammar §5.4 line 522).
///
/// ```ebnf
/// OnHandler ::= "on" { Capability } HandlerName { Param } [ "->" Type ] "=" Body ;
/// HandlerName ::= LOWER_IDENT ;
/// ```
///
/// Precondition: `cur.peek() == &Token::KwOn`.
pub(crate) fn parse_on_handler(
    cur: &mut Cursor<'_>,
    doc: Option<DocComment>,
) -> Result<OnHandler, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwOn)?;

    let caps = parse_cap_list(cur);

    // Handler name: LOWER_IDENT.
    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            Ident::new(s, name_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<handler name>",
                found: cur.peek().to_string(),
            });
        }
    };

    // Parameters.
    // `()` is the zero-param marker; consume it and leave params empty.
    let mut params: Vec<Param> = Vec::new();
    if cur.peek() == &Token::LParen && cur.peek_n(1) == Some(&Token::RParen) {
        cur.bump(); // consume `(`
        cur.bump(); // consume `)`
    } else {
        while can_start_param(cur) {
            params.push(parse_param_top(cur)?);
        }
    }

    // Optional `-> Type`.
    let ret = if cur.peek() == &Token::Arrow {
        cur.bump();
        Some(parse_type(cur)?)
    } else {
        None
    };

    cur.expect(&Token::Assign)?;

    let body = parse_branch_body(cur)?;
    let end_span = body.span();

    Ok(OnHandler {
        caps,
        name,
        params,
        ret,
        body,
        span: start.merge(end_span),
        doc,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::panic)]
#[allow(clippy::unwrap_used)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use ridge_ast::Span;
    use ridge_lexer::tokenize;

    fn lex(src: &str) -> Vec<(Token, Span)> {
        tokenize(src).tokens
    }

    fn lex_cur(src: &str) -> (Vec<(Token, Span)>, Cursor<'static>) {
        // We need the tokens to outlive the cursor, so we box them.
        // In tests we use a helper that takes &str and returns the parsed result directly.
        let _ = src;
        unreachable!("use parse_* helpers instead")
    }

    // Helper: lex and apply parse_visibility.
    fn parse_vis(src: &str) -> Result<Visibility, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        parse_visibility(&mut cur)
    }

    // Helper: lex and apply parse_import.
    fn parse_imp(src: &str) -> Result<ImportDecl, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        parse_import(&mut cur, None)
    }

    // Helper: lex and apply parse_const.
    fn parse_cst(src: &str) -> Result<ConstDecl, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        parse_const(&mut cur, vis, None)
    }

    // Helper: lex and apply parse_fn_decl.
    fn parse_fn(src: &str) -> Result<FnDecl, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        parse_fn_decl(&mut cur, vis, None)
    }

    // Helper: lex and apply parse_type_decl.
    fn parse_td(src: &str) -> Result<TypeDecl, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        parse_type_decl(&mut cur, vis, None)
    }

    // Helper: lex and apply parse_actor_decl.
    fn parse_actor(src: &str) -> Result<ActorDecl, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        parse_actor_decl(&mut cur, vis, None)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_import_simple
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_import_simple() {
        let imp = parse_imp("import std.list").expect("should parse");
        assert_eq!(imp.path.segments.len(), 2);
        assert_eq!(imp.path.segments[0].text, "std");
        assert_eq!(imp.path.segments[1].text, "list");
        assert!(imp.alias.is_none());
        assert!(imp.items.is_none());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_import_with_items
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_import_with_items() {
        let imp = parse_imp("import std.map (get, insert)").expect("should parse");
        assert_eq!(imp.path.segments[1].text, "map");
        let items = imp.items.expect("should have items");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "get");
        assert_eq!(items[1].text, "insert");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_import_with_alias
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_import_with_alias() {
        let imp = parse_imp("import std.list as List").expect("should parse");
        assert_eq!(imp.path.segments[1].text, "list");
        let alias = imp.alias.expect("should have alias");
        assert_eq!(alias.text, "List");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ImportItem accepts both LOWER_IDENT and UPPER_IDENT
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_import_with_mixed_case_items() {
        // Import lists may include UPPER_IDENT (types, constructors) alongside
        // LOWER_IDENT (fns, consts).
        // Grammar §2.2 puts `as Alias` BEFORE `(items)`.
        let imp = parse_imp("import std.net.http as Http (Request, Response, listen, respond)")
            .expect("should parse mixed-case import list");
        assert_eq!(imp.path.segments.last().unwrap().text, "http");
        let items = imp.items.expect("should have items");
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].text, "Request");
        assert_eq!(items[1].text, "Response");
        assert_eq!(items[2].text, "listen");
        assert_eq!(items[3].text, "respond");
        let alias = imp.alias.expect("should have alias");
        assert_eq!(alias.text, "Http");
    }

    #[test]
    fn parse_import_with_upper_only_items() {
        // Pure type/constructor import — no fns at all.
        let imp = parse_imp("import acme.shared.Types (UserId, OrderId)").expect("should parse");
        let items = imp.items.expect("should have items");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "UserId");
        assert_eq!(items[1].text, "OrderId");
        assert!(imp.alias.is_none());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_const_simple
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_const_simple() {
        // Grammar §2.4: const name is LOWER_IDENT (e.g. camelCase).
        let c = parse_cst("const pi: Float = 3.14").expect("should parse");
        assert_eq!(c.name.text, "pi");
        assert_eq!(c.vis, Visibility::Private);
        assert!(matches!(
            c.ty,
            ridge_ast::Type::Primitive {
                name: ridge_ast::PrimitiveType::Float,
                ..
            }
        ));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_const_pub
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_const_pub() {
        // Grammar §2.4: const name is LOWER_IDENT.
        let c = parse_cst("pub const maxRetries: Int = 100").expect("should parse");
        assert_eq!(c.vis, Visibility::Pub);
        assert_eq!(c.name.text, "maxRetries");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_simple
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_simple() {
        let f = parse_fn(r#"fn greet name = "hi""#).expect("should parse");
        assert_eq!(f.name.text, "greet");
        assert_eq!(f.params.len(), 1);
        assert!(matches!(&f.params[0], Param::Bare(id) if id.text == "name"));
        assert!(f.ret.is_none());
        assert!(f.caps.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_with_caps
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_with_caps() {
        let f = parse_fn("fn io log msg = msg").expect("should parse");
        assert_eq!(f.name.text, "log");
        assert_eq!(f.caps, vec![Capability::Io]);
        assert_eq!(f.params.len(), 1);
        assert!(matches!(&f.params[0], Param::Bare(id) if id.text == "msg"));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_with_params_and_ret
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_with_params_and_ret() {
        let f = parse_fn("fn add (x: Int) (y: Int) -> Int = x").expect("should parse");
        assert_eq!(f.name.text, "add");
        assert_eq!(f.params.len(), 2);
        assert!(matches!(&f.params[0], Param::Annotated { name, .. } if name.text == "x"));
        assert!(matches!(&f.params[1], Param::Annotated { name, .. } if name.text == "y"));
        assert!(f.ret.is_some());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_reject_tuple_param_p012
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_reject_tuple_param_p012() {
        let toks = lex("fn foo (x, y) = x");
        let mut cur = Cursor::new(&toks);
        let _ = parse_visibility(&mut cur).unwrap();
        cur.expect(&Token::KwFn).unwrap();
        // caps
        let _caps = parse_cap_list(&mut cur);
        // name
        let name_span = cur.span();
        let _name = match cur.peek().clone() {
            Token::LowerIdent(s) => {
                cur.bump();
                Ident::new(s, name_span)
            }
            _ => panic!("expected fn name"),
        };
        // try first param — should be P012
        let result = parse_param_top(&mut cur);
        assert!(result.is_err(), "expected Err(P012), got {result:?}");
        assert_eq!(result.unwrap_err().code(), "P012");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_alias
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_alias() {
        let td = parse_td("type UserId = Text").expect("should parse");
        assert_eq!(td.name.text, "UserId");
        assert!(td.params.is_empty());
        assert!(matches!(td.body, TypeBody::Alias(_)));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_record
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_record() {
        let td = parse_td("type User = { name: Text, age: Int }").expect("should parse");
        assert_eq!(td.name.text, "User");
        let body = match &td.body {
            TypeBody::Record(r) => r,
            other => panic!("expected Record, got {other:?}"),
        };
        assert_eq!(body.fields.len(), 2);
        assert_eq!(body.fields[0].name.text, "name");
        assert_eq!(body.fields[1].name.text, "age");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_union_leading_bar
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_union_leading_bar() {
        let td = parse_td("type Color = | Red | Green | Blue").expect("should parse");
        assert_eq!(td.name.text, "Color");
        let alts = match &td.body {
            TypeBody::Union(u) => &u.alternatives,
            other => panic!("expected Union, got {other:?}"),
        };
        assert_eq!(alts.len(), 3);
        assert!(matches!(&alts[0], Constructor::Positional { name, .. } if name.text == "Red"));
        assert!(matches!(&alts[1], Constructor::Positional { name, .. } if name.text == "Green"));
        assert!(matches!(&alts[2], Constructor::Positional { name, .. } if name.text == "Blue"));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_union_no_leading_bar
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_union_no_leading_bar() {
        let td = parse_td("type Color = Red | Green | Blue").expect("should parse");
        let alts = match &td.body {
            TypeBody::Union(u) => &u.alternatives,
            other => panic!("expected Union, got {other:?}"),
        };
        assert_eq!(alts.len(), 3);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_union_positional_two_ctors
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_union_positional_two_ctors() {
        // `Circle Int | Rectangle Int Int` — positional union, pipe is beyond
        // the first constructor's argument, so one-token lookahead would miss it.
        let td = parse_td("type Shape = Circle Int | Rectangle Int Int").expect("should parse");
        assert_eq!(td.name.text, "Shape");
        let alts = match &td.body {
            TypeBody::Union(u) => &u.alternatives,
            other => panic!("expected Union, got {other:?}"),
        };
        assert_eq!(alts.len(), 2);
        match &alts[0] {
            Constructor::Positional { name, args, .. } => {
                assert_eq!(name.text, "Circle");
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected Positional, got {other:?}"),
        }
        match &alts[1] {
            Constructor::Positional { name, args, .. } => {
                assert_eq!(name.text, "Rectangle");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected Positional, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_union_positional_three_ctors
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_union_positional_three_ctors() {
        let td = parse_td("type Shape = Circle Int | Rectangle Int Int | Triangle Int Int Int")
            .expect("should parse");
        assert_eq!(td.name.text, "Shape");
        let alts = match &td.body {
            TypeBody::Union(u) => &u.alternatives,
            other => panic!("expected Union, got {other:?}"),
        };
        assert_eq!(alts.len(), 3);
        match &alts[2] {
            Constructor::Positional { name, args, .. } => {
                assert_eq!(name.text, "Triangle");
                assert_eq!(args.len(), 3);
            }
            other => panic!("expected Positional, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_alias_single_positional_ctor_no_pipe — regression guard
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_alias_single_positional_ctor_no_pipe() {
        // `type Wrapper = Inner Int` has a single constructor with no `|`.
        // It must still be parsed as a type alias, not a union.
        let td = parse_td("type Wrapper = Inner Int").expect("should parse");
        assert_eq!(td.name.text, "Wrapper");
        assert!(
            matches!(td.body, TypeBody::Alias(_)),
            "expected Alias, got {:?}",
            td.body
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_union_nullary_no_leading_bar — regression guard
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_union_nullary_no_leading_bar_regression() {
        // Nullary unions without a leading `|` must still parse correctly.
        let td = parse_td("type Color = Red | Green | Blue").expect("should parse");
        let alts = match &td.body {
            TypeBody::Union(u) => &u.alternatives,
            other => panic!("expected Union, got {other:?}"),
        };
        assert_eq!(alts.len(), 3);
        assert!(matches!(&alts[0], Constructor::Positional { name, .. } if name.text == "Red"));
        assert!(matches!(&alts[1], Constructor::Positional { name, .. } if name.text == "Green"));
        assert!(matches!(&alts[2], Constructor::Positional { name, .. } if name.text == "Blue"));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_union_with_args
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_union_with_args() {
        let td = parse_td("type Option a = None | Some a").expect("should parse");
        assert_eq!(td.name.text, "Option");
        assert_eq!(td.params.len(), 1);
        assert_eq!(td.params[0].text, "a");
        let alts = match &td.body {
            TypeBody::Union(u) => &u.alternatives,
            other => panic!("expected Union, got {other:?}"),
        };
        assert_eq!(alts.len(), 2);
        // None has no args; Some has one arg.
        match &alts[0] {
            Constructor::Positional { name, args, .. } => {
                assert_eq!(name.text, "None");
                assert!(args.is_empty());
            }
            other @ Constructor::Record { .. } => panic!("expected Positional, got {other:?}"),
        }
        match &alts[1] {
            Constructor::Positional { name, args, .. } => {
                assert_eq!(name.text, "Some");
                assert_eq!(args.len(), 1);
            }
            other @ Constructor::Record { .. } => panic!("expected Positional, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_actor_without_init (url_shortener.ridge Store shape)
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_actor_without_init() {
        let src = "actor Store =\n    state table: Map Text Text = Map.empty\n    on shorten (url: Text) -> Text = url\n";
        let a = parse_actor(src).expect("should parse");
        assert_eq!(a.name.text, "Store");
        assert_eq!(a.members.len(), 2);
        assert!(matches!(&a.members[0], ActorMember::State(_)));
        assert!(matches!(&a.members[1], ActorMember::On(_)));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_actor_with_init (rate_limiter.ridge Limiter shape)
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_actor_with_init() {
        let src = "actor Limiter =\n    state capacity: Int\n    init (cap: Int) (rate: Float) =\n        capacity <- cap\n";
        let a = parse_actor(src).expect("should parse");
        assert_eq!(a.name.text, "Limiter");
        // At least state + init.
        assert!(
            a.members.len() >= 2,
            "expected ≥2 members, got {}",
            a.members.len()
        );
        assert!(matches!(&a.members[0], ActorMember::State(_)));
        assert!(matches!(&a.members[1], ActorMember::Init(_)));
        if let ActorMember::Init(init) = &a.members[1] {
            assert_eq!(init.params.len(), 2);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_visibility_pub_internal
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_visibility_pub_internal() {
        let c = parse_cst("pub(internal) const x: Int = 1").expect("should parse");
        assert_eq!(c.vis, Visibility::PubInternal);
        assert_eq!(c.name.text, "x");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_inner_fn_expr — InnerFn is also tested in expr.rs
    // This test verifies that parse_fn_decl itself works for inner fn shape.
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_inner_fn_decl_shape() {
        // parse_fn_decl with vis=Private, doc=None (as used by InnerFn).
        let toks = lex("fn foo x = x");
        let mut cur = Cursor::new(&toks);
        let f = parse_fn_decl(&mut cur, Visibility::Private, None).expect("should parse");
        assert_eq!(f.name.text, "foo");
        assert_eq!(f.vis, Visibility::Private);
        assert_eq!(f.params.len(), 1);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Example-program first-declaration tests
    // ─────────────────────────────────────────────────────────────────────────

    // log_analyzer.ridge — first import `import std.fs as Fs`
    #[test]
    fn parse_log_analyzer_first_decl() {
        let imp = parse_imp("import std.fs as Fs").expect("should parse");
        assert_eq!(imp.path.segments[0].text, "std");
        assert_eq!(imp.path.segments[1].text, "fs");
        assert_eq!(imp.alias.as_ref().map(|a| a.text.as_str()), Some("Fs"));
    }

    // url_shortener.ridge — first import `import std.io as Io`
    #[test]
    fn parse_url_shortener_first_decl() {
        let imp = parse_imp("import std.io as Io").expect("should parse");
        assert_eq!(imp.path.segments[1].text, "io");
        assert_eq!(imp.alias.as_ref().map(|a| a.text.as_str()), Some("Io"));
    }

    // game_of_life.ridge — first import `import std.io as Io`
    // and first declaration `type Grid = { rows: Int, cols: Int, cells: List (List Bool) }`
    #[test]
    fn parse_game_of_life_first_decl() {
        let td = parse_td("type Grid = { rows: Int, cols: Int }").expect("should parse");
        assert_eq!(td.name.text, "Grid");
        let body = match &td.body {
            TypeBody::Record(r) => r,
            other => panic!("expected Record, got {other:?}"),
        };
        assert_eq!(body.fields.len(), 2);
    }

    // rate_limiter.ridge — actor Limiter with init block
    #[test]
    fn parse_rate_limiter_first_decl() {
        let src = "actor Limiter =\n    state capacity: Int\n    state tokens: Float\n    init (cap: Int) (rate: Float) =\n        capacity <- cap\n        tokens <- Float.fromInt cap\n";
        let a = parse_actor(src).expect("should parse");
        assert_eq!(a.name.text, "Limiter");
        assert!(a.members.len() >= 3, "expected ≥3 members");
        // Find init member.
        let has_init = a.members.iter().any(|m| matches!(m, ActorMember::Init(_)));
        assert!(has_init, "expected an Init member");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_no_params_no_ret
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_no_params_no_ret() {
        let f = parse_fn("fn main = 42").expect("should parse");
        assert_eq!(f.name.text, "main");
        assert!(f.params.is_empty());
        assert!(f.ret.is_none());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_const_missing_colon_p005
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_const_missing_colon_p005() {
        let toks = lex("const x = 1");
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        let result = parse_const(&mut cur, vis, None);
        assert!(result.is_err(), "expected Err(P005), got {result:?}");
        assert_eq!(result.unwrap_err().code(), "P005");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_record_multiline (grid type from game_of_life)
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_record_multiline() {
        // Multiline record body (lexer produces Newline inside {…} — but the
        // bracket suppression rule means Newline tokens ARE suppressed.
        // We test with the inline form.
        let td =
            parse_td("type Grid = { rows: Int, cols: Int, cells: [Bool] }").expect("should parse");
        assert_eq!(td.name.text, "Grid");
        match &td.body {
            TypeBody::Record(r) => assert_eq!(r.fields.len(), 3),
            other => panic!("expected Record, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_on_handler_with_ret
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_on_handler_with_ret() {
        let toks = lex("on get -> Int = 42");
        let mut cur = Cursor::new(&toks);
        let handler = parse_on_handler(&mut cur, None).expect("should parse");
        assert_eq!(handler.name.text, "get");
        assert!(handler.params.is_empty());
        assert!(handler.ret.is_some());
        assert!(matches!(&handler.body, Expr::Literal(_)));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_on_handler_with_caps_and_params
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_on_handler_with_caps_and_params() {
        let toks = lex("on io report (allowed: Int) -> Unit = allowed");
        let mut cur = Cursor::new(&toks);
        let handler = parse_on_handler(&mut cur, None).expect("should parse");
        assert_eq!(handler.name.text, "report");
        assert_eq!(handler.caps, vec![Capability::Io]);
        assert_eq!(handler.params.len(), 1);
        assert!(handler.ret.is_some());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_state_decl_no_default
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_state_decl_no_default() {
        let toks = lex("state capacity: Int");
        let mut cur = Cursor::new(&toks);
        let s = parse_state_decl(&mut cur).expect("should parse");
        assert_eq!(s.name.text, "capacity");
        assert!(s.default.is_none());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_state_decl_with_default
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_state_decl_with_default() {
        let toks = lex("state count: Int = 0");
        let mut cur = Cursor::new(&toks);
        let s = parse_state_decl(&mut cur).expect("should parse");
        assert_eq!(s.name.text, "count");
        assert!(s.default.is_some());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_visibility_pub_variants
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_visibility_pub_variants() {
        assert_eq!(parse_vis("pub fn").expect("ok"), Visibility::Pub);
        assert_eq!(parse_vis("fn").expect("ok"), Visibility::Private);
        assert_eq!(
            parse_vis("pub(internal) fn").expect("ok"),
            Visibility::PubInternal
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_import_text_items (std.text)
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_import_text_items() {
        let imp = parse_imp("import std.text (split, trim, lines)").expect("should parse");
        assert_eq!(imp.path.segments[1].text, "text");
        let items = imp.items.expect("should have items");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].text, "split");
        assert_eq!(items[1].text, "trim");
        assert_eq!(items[2].text, "lines");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_zero_params_paren
    // `fn main () = 42` should parse to FnDecl { params: [], ... }
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_zero_params_paren() {
        let f = parse_fn("fn main () = 42").expect("should parse");
        assert_eq!(f.name.text, "main");
        assert!(
            f.params.is_empty(),
            "expected no params, got {:?}",
            f.params
        );
        assert!(f.ret.is_none());
        assert!(f.caps.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_zero_params_with_caps
    // `fn io fs main () -> Result Unit Error = 42` parses cleanly.
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_zero_params_with_caps() {
        let f = parse_fn("fn io fs main () -> Result Unit Error = 42").expect("should parse");
        assert_eq!(f.name.text, "main");
        assert!(
            f.params.is_empty(),
            "expected no params, got {:?}",
            f.params
        );
        assert!(f.ret.is_some(), "expected return type");
        assert_eq!(f.caps, vec![Capability::Io, Capability::Fs]);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_zero_params_with_caps_and_body_block
    // Multi-line body version with `()` zero-param marker.
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_zero_params_with_caps_and_body_block() {
        let src = "fn env io main () -> Result Unit Error =\n    42";
        let f = parse_fn(src).expect("should parse");
        assert_eq!(f.name.text, "main");
        assert!(
            f.params.is_empty(),
            "expected no params, got {:?}",
            f.params
        );
        assert!(f.ret.is_some(), "expected return type");
        assert!(f.caps.contains(&Capability::Io));
        assert!(f.caps.contains(&Capability::Env));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_on_zero_params
    // `on tick () = count` — on handler with zero-param marker.
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_on_zero_params() {
        let toks = lex("on tick () = count");
        let mut cur = Cursor::new(&toks);
        let handler = parse_on_handler(&mut cur, None).expect("should parse");
        assert_eq!(handler.name.text, "tick");
        assert!(
            handler.params.is_empty(),
            "expected no params, got {:?}",
            handler.params
        );
        assert!(handler.ret.is_none());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_deferred_class_keyword_p013
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_deferred_class_keyword_p013() {
        let toks = lex("class Eq a = ...");
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        let result = parse_item(&mut cur, None, vis);
        assert!(result.is_err(), "expected Err(P013), got {result:?}");
        assert_eq!(result.unwrap_err().code(), "P013");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_with_spawn_capability
    // `fn spawn io time main () = 42` — `spawn` is emitted as KwSpawn by the
    // lexer; parse_cap_list must accept it alongside plain-LowerIdent caps.
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_with_spawn_capability() {
        let f = parse_fn("fn spawn io time main () = 42").expect("should parse");
        assert_eq!(f.name.text, "main");
        assert!(
            f.params.is_empty(),
            "expected no params, got {:?}",
            f.params
        );
        assert!(
            f.caps.contains(&Capability::Spawn),
            "expected Spawn in caps, got {:?}",
            f.caps
        );
        assert!(
            f.caps.contains(&Capability::Io),
            "expected Io in caps, got {:?}",
            f.caps
        );
        assert!(
            f.caps.contains(&Capability::Time),
            "expected Time in caps, got {:?}",
            f.caps
        );
        assert_eq!(f.caps.len(), 3, "expected 3 caps, got {:?}", f.caps);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_fn_spawn_net_io_time_caps
    // `fn spawn net io time main () = 42` — 4-capability case from
    // url_shortener.ridge / rate_limiter.ridge main function.
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_fn_spawn_net_io_time_caps() {
        let f = parse_fn("fn spawn net io time main () = 42").expect("should parse");
        assert_eq!(f.name.text, "main");
        assert!(
            f.params.is_empty(),
            "expected no params, got {:?}",
            f.params
        );
        assert!(
            f.caps.contains(&Capability::Spawn),
            "expected Spawn in caps, got {:?}",
            f.caps
        );
        assert!(
            f.caps.contains(&Capability::Net),
            "expected Net in caps, got {:?}",
            f.caps
        );
        assert!(
            f.caps.contains(&Capability::Io),
            "expected Io in caps, got {:?}",
            f.caps
        );
        assert!(
            f.caps.contains(&Capability::Time),
            "expected Time in caps, got {:?}",
            f.caps
        );
        assert_eq!(f.caps.len(), 4, "expected 4 caps, got {:?}", f.caps);
    }
}
