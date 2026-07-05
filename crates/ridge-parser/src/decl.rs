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
    typeclass::{ClassConstraint, ClassDecl, FunDep, InstanceDecl, MethodDef, MethodSig},
    ActorDecl, ActorMember, Attribute, Body, Capability, ConstDecl, Constructor, DocComment, Expr,
    FieldDecl, FnDecl, Ident, ImportDecl, InitDecl, Item, MailboxConfig, MailboxDecl,
    MailboxPolicy, ModulePath, OnHandler, Param, RecordTypeBody, StateDecl, Type, TypeBody,
    TypeDecl, UnionTypeBody, Visibility,
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
        attrs: vec![],
        vis,
        caps,
        name,
        params,
        ret,
        constraints: vec![],
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
    ty::{is_type_atom_start, parse_type, parse_type_atom},
};

// ── @test attribute ───────────────────────────────────────────────────────────

/// Attempt to parse an `@test "<display-name>"` attribute.
///
/// Grammar:
/// ```ebnf
/// TestAttr = "@" "test" TextLit ;
/// ```
///
/// The cursor must be positioned at `Token::At` when this function is called.
/// On success the cursor is advanced past the closing string literal.
/// On failure, a `ParseError` is returned.
///
/// Precondition: `cur.peek() == &Token::At` and the token after `@` is
/// the identifier `test`.
fn parse_test_attr(cur: &mut Cursor<'_>) -> Result<Attribute, ParseError> {
    let start = cur.span();
    cur.expect(&Token::At)?; // consume `@`

    // Consume the literal identifier "test" (verified by caller).
    cur.bump();

    // Argument: string literal display name.
    let name_span = cur.span();
    match cur.peek().clone() {
        Token::TextLit(s) => {
            let end = cur.span();
            cur.bump();
            Ok(Attribute::Test {
                name: s,
                span: start.merge(end),
            })
        }
        _ => Err(ParseError::TestAttrArgNotString { span: name_span }),
    }
}

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
    // ── Attribute handling — `@ffi` and `@test` ───────────────────────────────
    // When the current token is `@`, inspect the following identifier to decide
    // which attribute to parse.  The visibility passed in will be `Private`
    // (because `@` is not a visibility keyword); visibility is re-parsed after
    // any `@test` attributes, or inside `parse_fn_decl_ffi` for `@ffi`.
    if cur.peek() == &Token::At {
        // Peek past `@` to see the attribute name.
        let attr_name = cur.peek_n(1).cloned();
        if matches!(&attr_name, Some(Token::LowerIdent(s)) if s == "ffi") {
            // `@ffi(...)` — existing path, unchanged.
            let ffi = parse_ffi_attr(cur)?;
            while cur.peek() == &Token::Newline {
                cur.bump();
            }
            return Ok(Item::Fn(parse_fn_decl_ffi(cur, ffi, doc)?));
        }

        if matches!(&attr_name, Some(Token::LowerIdent(s)) if s == "test") {
            // Collect zero or more `@test` attributes (only one is expected in
            // practice, but the grammar allows the same form multiple times).
            let mut attrs: Vec<Attribute> = Vec::new();
            while cur.peek() == &Token::At
                && matches!(cur.peek_n(1), Some(Token::LowerIdent(s)) if s == "test")
            {
                attrs.push(parse_test_attr(cur)?);
                while cur.peek() == &Token::Newline {
                    cur.bump();
                }
            }
            // Re-parse visibility, then dispatch to the normal fn parser with
            // the collected attrs.
            let fn_vis = parse_visibility(cur)?;
            return Ok(Item::Fn(parse_fn_decl_with_attrs(cur, fn_vis, doc, attrs)?));
        }
    }

    match cur.peek() {
        Token::KwImport => Ok(Item::Import(parse_import(cur, doc)?)),
        Token::KwConst => Ok(Item::Const(parse_const(cur, vis, doc)?)),
        Token::KwType => Ok(Item::Type(parse_type_decl(cur, vis, doc, false)?)),
        Token::KwOpaque => {
            cur.bump(); // consume `opaque`; `parse_type_decl` expects `type` next
            Ok(Item::Type(parse_type_decl(cur, vis, doc, true)?))
        }
        Token::KwFn => Ok(Item::Fn(parse_fn_decl(cur, vis, doc)?)),
        Token::KwActor => Ok(Item::Actor(parse_actor_decl(cur, vis, doc)?)),

        Token::KwClass => Ok(Item::ClassDecl(parse_class_decl(cur, vis, doc)?)),
        Token::KwInstance => Ok(Item::InstanceDecl(parse_instance_decl(cur, doc)?)),
        // `deriving` is only valid as a trailing clause on a type declaration,
        // never as a standalone top-level item.
        Token::KwDeriving => Err(ParseError::DeferredFeature {
            span: cur.span(),
            feature: "standalone deriving",
            since: "0.2.13",
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
            "db" => Some(Capability::Db),
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

        // ── Parenthesised form: `(name: Type)` or `(pat: Type)` ──────────────
        Token::LParen => {
            let start = cur.span();
            cur.bump(); // consume `(`

            // A reserved keyword in this position (e.g. `(init: Int)`) is
            // recognised separately so the user sees the actual cause instead
            // of a misleading pattern error.
            if let Some(keyword) = cur.peek().keyword_text() {
                return Err(ParseError::ReservedKeywordAsIdent {
                    span: cur.span(),
                    keyword,
                    position: "a function parameter",
                });
            }

            // Fast path: `(name: Type)` — a bare annotated param. Kept distinct
            // from the pattern path so the common case stays `Param::Annotated`.
            if let Token::LowerIdent(name_text) = cur.peek().clone() {
                if matches!(cur.peek_n(1), Some(Token::Colon)) {
                    let name_span = cur.span();
                    cur.bump(); // consume name
                    cur.bump(); // consume `:`
                    let ty = parse_type(cur)?;
                    let end_span = cur.expect(&Token::RParen)?;
                    return Ok(Param::Annotated {
                        name: Ident::new(name_text, name_span),
                        ty,
                        span: start.merge(end_span),
                    });
                }
            }

            // Otherwise: an irrefutable destructuring param `(pat : Type)`.
            // Parse a full pattern, then require the `: Type` annotation. The
            // pattern's irrefutability is checked later in typecheck, where the
            // type is known. Without the annotation it is the historical
            // un-annotated pattern param, still rejected with P012.
            let pat = crate::pattern::parse_pattern(cur)?;
            if matches!(cur.peek(), Token::Colon) {
                cur.bump(); // consume `:`
                let ty = parse_type(cur)?;
                let end_span = cur.expect(&Token::RParen)?;
                Ok(Param::PatternAnnotated {
                    pat,
                    ty,
                    span: start.merge(end_span),
                })
            } else {
                Err(ParseError::TopLevelPatternParam {
                    span: start.merge(cur.span()),
                })
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
/// Return the text of a token that can appear as a module-path segment in an
/// `import` declaration.
///
/// Module paths accept any identifier plus a small set of global keywords
/// whose lower-case spelling is a valid stdlib module name. The pattern keeps
/// the keyword reserved in expression position while letting an import like
/// `import std.actor as Actor` still parse cleanly.
fn module_path_segment_text(token: &Token) -> Option<String> {
    match token {
        Token::LowerIdent(s) | Token::UpperIdent(s) => Some(s.clone()),
        Token::KwActor => Some("actor".to_string()),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn parse_import(
    cur: &mut Cursor<'_>,
    doc: Option<DocComment>,
) -> Result<ImportDecl, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwImport)?;

    // ── ModulePath ────────────────────────────────────────────────────────────
    // Each segment is an identifier; selected keywords whose lower-case spelling
    // is a valid stdlib module name are also accepted here so paths like
    // `std.actor` can be written even though `actor` is a global keyword.
    let mut segments: Vec<Ident> = Vec::new();
    let seg_span = cur.span();
    let first_seg = match module_path_segment_text(cur.peek()) {
        Some(text) => {
            cur.bump();
            Ident::new(text, seg_span)
        }
        None => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<module name>",
                found: cur.peek().to_string(),
            });
        }
    };
    segments.push(first_seg);

    // Subsequent segments: `.` + identifier-like token. Bail out on `.` followed
    // by anything else (e.g. `(.name)` field-accessor sugar at expression sites).
    while cur.peek() == &Token::Dot {
        let Some(text) = cur.peek_n(1).and_then(module_path_segment_text) else {
            break;
        };
        cur.bump(); // consume `.`
        let seg_span = cur.span();
        cur.bump(); // consume segment
        segments.push(Ident::new(text, seg_span));
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
    opaque: bool,
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
    let body_end_span = match &body {
        TypeBody::Record(r) => r.span,
        TypeBody::Union(u) => u.span,
        TypeBody::Alias(t) => t.span(),
    };

    // `opaque` hides a constructor and fields; an alias has neither, so the
    // modifier is meaningless there (P032).
    if opaque && matches!(body, TypeBody::Alias(_)) {
        return Err(ParseError::OpaqueOnAlias {
            span: start.merge(body_end_span),
        });
    }

    // Optional trailing `deriving ( ClassName, … )` clause.
    let deriving = if cur.peek() == &Token::KwDeriving {
        parse_deriving_clause(cur)?
    } else {
        vec![]
    };

    let end_span = deriving.last().map_or(body_end_span, |id: &Ident| id.span);

    Ok(TypeDecl {
        vis,
        opaque,
        name,
        params,
        body,
        deriving,
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
/// `(…)`, and record-variant bodies `{…}` — tracking bracket/paren/brace depth.
/// Returns `true` if a `|` is found at depth 0 before any line terminator.
///
/// The brace tracking is what lets a record-variant constructor appear before
/// the `|`, e.g. `type Shape = Circle { radius: Int } | Square`. Without it the
/// scan stopped at the opening `{` and misclassified the union as a type alias
/// (`App(Circle, [Record …])` with a dangling `| …`).
///
/// A line terminator (`Newline`, `Dedent`, `Assign`) ends the scan only at
/// depth 0; inside a bracketed group a layout newline is content, not a
/// terminator (record-variant bodies may span lines). `Eof` and end-of-stream
/// always stop.
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
            None | Some(Token::Eof) => return false,
            Some(Token::Newline | Token::Dedent | Token::Assign) if depth == 0 => return false,
            Some(Token::Pipe) if depth == 0 => return true,
            Some(Token::LParen | Token::LBrack | Token::LBrace) => {
                depth += 1;
                n += 1;
            }
            Some(Token::RParen | Token::RBrack | Token::RBrace) => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
                n += 1;
            }
            // Any token inside brackets/braces (including layout newlines): keep scanning.
            _ if depth > 0 => {
                n += 1;
            }
            // Type-atom tokens at depth 0: skip past them.
            Some(Token::UpperIdent(_) | Token::LowerIdent(_)) => {
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

    // Optional `where ClassConstraint { "," ClassConstraint }` clause.
    let constraints = if cur.peek() == &Token::KwWhere {
        parse_where_clause(cur)?
    } else {
        vec![]
    };

    // `=` then body (inline or INDENT-delimited block).
    cur.expect(&Token::Assign)?;

    let expr = parse_branch_body(cur)?;
    let end_span = expr.span();

    Ok(FnDecl {
        attrs: vec![],
        vis,
        caps,
        name,
        params,
        ret,
        constraints,
        body: Body::Expr(expr),
        span: start.merge(end_span),
        doc,
    })
}

/// Parse a function declaration that has pre-parsed attributes.
///
/// Called from [`parse_item`] after one or more `@test` attributes have been
/// collected.  The cursor must be positioned at `fn` (after any visibility has
/// already been parsed by the caller).
///
/// Precondition: `cur.peek() == &Token::KwFn`.
fn parse_fn_decl_with_attrs(
    cur: &mut Cursor<'_>,
    vis: Visibility,
    doc: Option<DocComment>,
    attrs: Vec<Attribute>,
) -> Result<FnDecl, ParseError> {
    let start = cur.span();
    cur.expect(&Token::KwFn)?;

    let caps = parse_cap_list(cur);

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

    let mut params: Vec<Param> = Vec::new();
    if cur.peek() == &Token::LParen && cur.peek_n(1) == Some(&Token::RParen) {
        cur.bump();
        cur.bump();
    } else {
        while can_start_param(cur) {
            params.push(parse_param_top(cur)?);
        }
    }

    let ret = if cur.peek() == &Token::Arrow {
        cur.bump();
        Some(parse_type(cur)?)
    } else {
        None
    };

    // Optional `where` clause (same as plain parse_fn_decl).
    let constraints = if cur.peek() == &Token::KwWhere {
        parse_where_clause(cur)?
    } else {
        vec![]
    };

    cur.expect(&Token::Assign)?;

    let expr = parse_branch_body(cur)?;
    let end_span = expr.span();

    Ok(FnDecl {
        attrs,
        vis,
        caps,
        name,
        params,
        ret,
        constraints,
        body: Body::Expr(expr),
        span: start.merge(end_span),
        doc,
    })
}

// ── Typeclass helpers ─────────────────────────────────────────────────────────

/// Desugar `Show` to `ToText` so it never propagates past the parser.
///
/// All other class names are returned unchanged. The span of the returned
/// `Ident` is the span of the original token.
fn desugar_class_name(ident: Ident) -> Ident {
    if ident.text == "Show" {
        Ident::new("ToText", ident.span)
    } else {
        ident
    }
}

/// Parse a `where SuperList` clause used both in class heads and fn signatures.
///
/// ```ebnf
/// WhereClause  ::= "where" ClassConstraint { "," ClassConstraint }
/// ClassConstraint ::= UpperIdent TyVar
/// ```
///
/// Precondition: `cur.peek() == &Token::KwWhere`.
fn parse_where_clause(cur: &mut Cursor<'_>) -> Result<Vec<ClassConstraint>, ParseError> {
    cur.bump(); // consume `where`
    let mut constraints = Vec::new();
    loop {
        // UpperIdent = class name.
        let class_span = cur.span();
        let class = match cur.peek().clone() {
            Token::UpperIdent(s) => {
                cur.bump();
                desugar_class_name(Ident::new(s, class_span))
            }
            _ => {
                return Err(ParseError::Expected {
                    span: cur.span(),
                    expected: "<class name>",
                    found: cur.peek().to_string(),
                });
            }
        };

        // One or more LowerIdent type variables. A multi-parameter constraint
        // such as `Convert a b` carries several; they end at the next `,`, the
        // `=`, or the start of the indented body.
        let mut ty_vars: Vec<Ident> = Vec::new();
        let mut last_span = class_span;
        while let Token::LowerIdent(s) = cur.peek().clone() {
            let sp = cur.span();
            cur.bump();
            ty_vars.push(Ident::new(s, sp));
            last_span = sp;
        }
        if ty_vars.is_empty() {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<type variable>",
                found: cur.peek().to_string(),
            });
        }

        let span = class_span.merge(last_span);
        constraints.push(ClassConstraint {
            class,
            ty_vars,
            span,
        });

        // Another constraint follows if there is a `,`.
        if cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
        } else {
            break;
        }
    }
    Ok(constraints)
}

/// Parse a `deriving ( UpperIdent { "," UpperIdent } )` clause.
///
/// ```ebnf
/// Deriving ::= "deriving" "(" UpperIdent { "," UpperIdent } ")"
/// ```
///
/// Precondition: `cur.peek() == &Token::KwDeriving`.
fn parse_deriving_clause(cur: &mut Cursor<'_>) -> Result<Vec<Ident>, ParseError> {
    cur.bump(); // consume `deriving`

    cur.expect(&Token::LParen)?;

    let mut classes: Vec<Ident> = Vec::new();

    // Allow empty `()`.
    if cur.peek() != &Token::RParen {
        let class_span = cur.span();
        let name = match cur.peek().clone() {
            Token::UpperIdent(s) => {
                cur.bump();
                desugar_class_name(Ident::new(s, class_span))
            }
            _ => {
                return Err(ParseError::Expected {
                    span: cur.span(),
                    expected: "<class name>",
                    found: cur.peek().to_string(),
                });
            }
        };
        classes.push(name);

        while cur.peek() == &Token::Comma {
            cur.bump(); // consume `,`
            if cur.peek() == &Token::RParen {
                break; // trailing comma allowed
            }
            let class_span = cur.span();
            let name = match cur.peek().clone() {
                Token::UpperIdent(s) => {
                    cur.bump();
                    desugar_class_name(Ident::new(s, class_span))
                }
                _ => {
                    return Err(ParseError::Expected {
                        span: cur.span(),
                        expected: "<class name>",
                        found: cur.peek().to_string(),
                    });
                }
            };
            classes.push(name);
        }
    }

    cur.expect(&Token::RParen)?;

    Ok(classes)
}

/// Parse a method signature (bare — no `fn` keyword, no body).
///
/// ```ebnf
/// MethodSig ::= LowerIdent ParamList "->" Type
/// ```
///
/// Precondition: `cur.peek()` is a `LowerIdent`.
fn parse_method_sig(cur: &mut Cursor<'_>) -> Result<MethodSig, ParseError> {
    let start = cur.span();

    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            Ident::new(s, name_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<method name>",
                found: cur.peek().to_string(),
            });
        }
    };

    // Parameters: `()` is the zero-param marker; consume it and leave params empty.
    let mut params: Vec<Param> = Vec::new();
    if cur.peek() == &Token::LParen && cur.peek_n(1) == Some(&Token::RParen) {
        cur.bump(); // consume `(`
        cur.bump(); // consume `)`
    } else {
        while can_start_param(cur) {
            params.push(parse_param_top(cur)?);
        }
    }

    // Required `->`.
    cur.expect(&Token::Arrow)?;

    let ret = parse_type(cur)?;
    let end_span = ret.span();

    Ok(MethodSig {
        name,
        params,
        ret,
        span: start.merge(end_span),
    })
}

/// Parse a method definition (bare name, params, `->` type, `=` body).
///
/// ```ebnf
/// MethodDef ::= LowerIdent ParamList "->" Type "=" Expr
/// ```
///
/// Precondition: `cur.peek()` is a `LowerIdent`.
fn parse_method_def(cur: &mut Cursor<'_>) -> Result<MethodDef, ParseError> {
    let start = cur.span();

    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            Ident::new(s, name_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<method name>",
                found: cur.peek().to_string(),
            });
        }
    };

    // Parameters.
    let mut params: Vec<Param> = Vec::new();
    if cur.peek() == &Token::LParen && cur.peek_n(1) == Some(&Token::RParen) {
        cur.bump();
        cur.bump();
    } else {
        while can_start_param(cur) {
            params.push(parse_param_top(cur)?);
        }
    }

    // Required `->`.
    cur.expect(&Token::Arrow)?;

    let ret = parse_type(cur)?;

    // Required `=` and body expression.
    cur.expect(&Token::Assign)?;

    let body = parse_branch_body(cur)?;
    let end_span = body.span();

    Ok(MethodDef {
        name,
        params,
        ret,
        body,
        span: start.merge(end_span),
    })
}

/// Parse a `class` declaration.
///
/// ```ebnf
/// ClassDecl ::= "class" UpperIdent TyVar [ "where" SuperList ] "=" NEWLINE
///               INDENT MethodSig+ DEDENT
/// ```
///
/// Precondition: `cur.peek() == &Token::KwClass`.
#[allow(clippy::too_many_lines)] // exhaustive error paths for each structural fault
fn parse_class_decl(
    cur: &mut Cursor<'_>,
    _vis: Visibility,
    doc: Option<DocComment>,
) -> Result<ClassDecl, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `class`

    // Class name: UpperIdent.
    let name_span = cur.span();
    let name = match cur.peek().clone() {
        Token::UpperIdent(s) => {
            cur.bump();
            desugar_class_name(Ident::new(s, name_span))
        }
        _ => {
            return Err(ParseError::MalformedClassDecl {
                span: cur.span(),
                reason: "expected a class name (`UpperIdent`) after `class`".to_string(),
            });
        }
    };

    // Type variables: one or more LowerIdent. A multi-parameter class such as
    // `Convert a b` declares several; they end at `where`, `=`, or the body.
    let mut ty_vars: Vec<Ident> = Vec::new();
    while let Token::LowerIdent(s) = cur.peek().clone() {
        let sp = cur.span();
        cur.bump();
        ty_vars.push(Ident::new(s, sp));
    }
    if ty_vars.is_empty() {
        return Err(ParseError::MalformedClassDecl {
            span: cur.span(),
            reason: "expected at least one type variable (`lowerIdent`) after the class name"
                .to_string(),
        });
    }

    // Optional functional dependencies: `| from… -> to… (, …)*`. They sit
    // between the type variables and the `where` superclass list, e.g.
    // `class Refinable q p | q -> p where … =`.
    let fundeps = if cur.peek() == &Token::Pipe {
        parse_fundeps(cur)?
    } else {
        vec![]
    };

    // Optional `where SuperList` (superclass constraints).
    let superclasses = if cur.peek() == &Token::KwWhere {
        parse_where_clause(cur)?
    } else {
        vec![]
    };

    // `=` then indented body.
    cur.expect(&Token::Assign)?;

    // Consume optional NEWLINE before INDENT.
    if cur.peek() == &Token::Newline && cur.peek_n(1) == Some(&Token::Indent) {
        cur.bump();
    }

    let indent_span = cur.span();
    if cur.peek() != &Token::Indent {
        return Err(ParseError::MalformedClassDecl {
            span: indent_span,
            reason: "class body must be an indented block of method signatures".to_string(),
        });
    }
    cur.bump(); // consume INDENT

    // Must have at least one method signature.
    if cur.peek() == &Token::Dedent {
        let empty_span = cur.span();
        cur.bump(); // consume DEDENT
        return Err(ParseError::MalformedClassDecl {
            span: empty_span,
            reason: "class body must contain at least one method signature; \
                     write `methodName (param: Type) -> RetType`"
                .to_string(),
        });
    }

    // Reject `fn` keyword at the start of the first member (clearer error).
    if cur.peek() == &Token::KwFn {
        return Err(ParseError::MalformedClassDecl {
            span: cur.span(),
            reason: "method signatures in a class body must be bare (no `fn` keyword); \
                     write `methodName (param: Type) -> RetType`"
                .to_string(),
        });
    }

    let mut methods: Vec<MethodSig> = Vec::new();

    loop {
        // Reject `fn` keyword before each method.
        if cur.peek() == &Token::KwFn {
            return Err(ParseError::MalformedClassDecl {
                span: cur.span(),
                reason: "method signatures in a class body must be bare (no `fn` keyword); \
                         write `methodName (param: Type) -> RetType`"
                    .to_string(),
            });
        }

        // Parse one method signature.
        let sig = parse_method_sig(cur)?;

        // Reject a body expression after the signature (`= Expr`).
        if cur.peek() == &Token::Assign {
            return Err(ParseError::MalformedClassDecl {
                span: cur.span(),
                reason: "class method signatures must not have a body; \
                         default method bodies are not supported in this release"
                    .to_string(),
            });
        }

        methods.push(sig);

        // Consume the separating NEWLINE between signatures.
        if cur.peek() == &Token::Newline {
            cur.bump();
        }

        // Stop at DEDENT (end of class body).
        if cur.peek() == &Token::Dedent {
            break;
        }

        // Anything else that cannot start a method → stop; let DEDENT check
        // handle the error path below.
        if !matches!(cur.peek(), Token::LowerIdent(_)) {
            break;
        }
    }

    let end_span = cur.span();
    cur.expect(&Token::Dedent)?;

    Ok(ClassDecl {
        name,
        ty_vars,
        fundeps,
        superclasses,
        methods,
        span: start.merge(end_span),
        doc,
    })
}

/// Parse a functional-dependency list following a class header's type
/// variables: `| from… -> to… (, from… -> to…)*`.
///
/// Each dependency lists one or more determining variables, then `->`, then one
/// or more determined variables. The variable names are captured as written;
/// the typecheck collect pass resolves them against the class's `ty_vars` and
/// rejects any that are not declared.
///
/// Precondition: `cur.peek() == &Token::Pipe`.
fn parse_fundeps(cur: &mut Cursor<'_>) -> Result<Vec<FunDep>, ParseError> {
    cur.bump(); // consume `|`

    let mut deps: Vec<FunDep> = Vec::new();
    loop {
        let dep_start = cur.span();

        // Determining variables (left of `->`): one or more LowerIdent.
        let mut from: Vec<Ident> = Vec::new();
        while let Token::LowerIdent(s) = cur.peek().clone() {
            let sp = cur.span();
            cur.bump();
            from.push(Ident::new(s, sp));
        }
        if from.is_empty() {
            return Err(ParseError::MalformedClassDecl {
                span: cur.span(),
                reason: "expected a type variable before `->` in the functional dependency; \
                         write `| determining -> determined`"
                    .to_string(),
            });
        }

        // `->` separates the determining and determined variables.
        if cur.peek() != &Token::Arrow {
            return Err(ParseError::MalformedClassDecl {
                span: cur.span(),
                reason: "expected `->` in the functional dependency; \
                         write `| determining -> determined`"
                    .to_string(),
            });
        }
        cur.bump(); // consume `->`

        // Determined variables (right of `->`): one or more LowerIdent.
        let mut to: Vec<Ident> = Vec::new();
        let mut to_end = cur.span();
        while let Token::LowerIdent(s) = cur.peek().clone() {
            let sp = cur.span();
            cur.bump();
            to_end = sp;
            to.push(Ident::new(s, sp));
        }
        if to.is_empty() {
            return Err(ParseError::MalformedClassDecl {
                span: cur.span(),
                reason: "expected a type variable after `->` in the functional dependency; \
                         write `| determining -> determined`"
                    .to_string(),
            });
        }

        deps.push(FunDep {
            from,
            to,
            span: dep_start.merge(to_end),
        });

        // Further dependencies are comma-separated.
        if cur.peek() == &Token::Comma {
            cur.bump();
            continue;
        }
        break;
    }

    Ok(deps)
}

/// Parse an `instance` declaration.
///
/// ```ebnf
/// InstanceDecl ::= "instance" UpperIdent Type "=" NEWLINE
///                  INDENT MethodDef+ DEDENT
/// ```
///
/// Precondition: `cur.peek() == &Token::KwInstance`.
#[allow(clippy::too_many_lines)] // exhaustive error paths for each structural fault
fn parse_instance_decl(
    cur: &mut Cursor<'_>,
    doc: Option<DocComment>,
) -> Result<InstanceDecl, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `instance`

    // Class name: UpperIdent.
    let class_span = cur.span();
    let class = match cur.peek().clone() {
        Token::UpperIdent(s) => {
            cur.bump();
            desugar_class_name(Ident::new(s, class_span))
        }
        _ => {
            return Err(ParseError::MalformedInstanceDecl {
                span: cur.span(),
                reason: "expected a class name (`UpperIdent`) after `instance`".to_string(),
            });
        }
    };

    // Instance head: one or more type atoms, one per class parameter. An
    // ordinary instance has a single atom (`Eq Int`, `Encode (List a)`); a
    // multi-parameter instance has several (`Convert Celsius Fahrenheit`).
    let mut head: Vec<Type> = Vec::new();
    head.push(
        parse_type_atom(cur).map_err(|_| ParseError::MalformedInstanceDecl {
            span: cur.span(),
            reason: "expected a type after the class name in the instance head".to_string(),
        })?,
    );
    while is_type_atom_start(cur) {
        head.push(
            parse_type_atom(cur).map_err(|_| ParseError::MalformedInstanceDecl {
                span: cur.span(),
                reason: "expected a type atom in the instance head".to_string(),
            })?,
        );
    }

    // Optional `where` clause — lists context constraints for parametric
    // instances, e.g. `instance Encode (List a) where Encode a`.
    let constraints = if cur.peek() == &Token::KwWhere {
        parse_where_clause(cur).map_err(|e| ParseError::MalformedInstanceDecl {
            span: e.span(),
            reason: "invalid `where` clause on instance head".to_string(),
        })?
    } else {
        vec![]
    };

    // `=` then indented body.
    cur.expect(&Token::Assign)?;

    // Consume optional NEWLINE before INDENT.
    if cur.peek() == &Token::Newline && cur.peek_n(1) == Some(&Token::Indent) {
        cur.bump();
    }

    let indent_span = cur.span();
    if cur.peek() != &Token::Indent {
        return Err(ParseError::MalformedInstanceDecl {
            span: indent_span,
            reason: "instance body must be an indented block of method definitions".to_string(),
        });
    }
    cur.bump(); // consume INDENT

    // Must have at least one method definition.
    if cur.peek() == &Token::Dedent {
        let empty_span = cur.span();
        cur.bump();
        return Err(ParseError::MalformedInstanceDecl {
            span: empty_span,
            reason: "instance body must contain at least one method definition; \
                     write `methodName (param: Type) -> RetType = body`"
                .to_string(),
        });
    }

    let mut methods: Vec<MethodDef> = Vec::new();

    loop {
        let def = parse_method_def(cur).map_err(|e| {
            // If the method definition is missing its `=` body, surface a
            // clear P031 rather than the generic P001.
            ParseError::MalformedInstanceDecl {
                span: e.span(),
                reason: "each method in an instance must have a body; \
                         write `methodName (param: Type) -> RetType = body`"
                    .to_string(),
            }
        })?;

        methods.push(def);

        // Consume the separating NEWLINE between definitions.
        if cur.peek() == &Token::Newline {
            cur.bump();
        }

        // Stop at DEDENT (end of instance body).
        if cur.peek() == &Token::Dedent {
            break;
        }

        if !matches!(cur.peek(), Token::LowerIdent(_)) {
            break;
        }
    }

    let end_span = cur.span();
    cur.expect(&Token::Dedent)?;

    Ok(InstanceDecl {
        class,
        head,
        constraints,
        methods,
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
        ActorMember::Mailbox(mb) => mb.span,
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
/// keyword, `mailbox` member-introducer, `DEDENT`, or `EOF`.
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
            Token::LowerIdent(s) if s == "mailbox" => return,
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
        Token::LowerIdent(s) if s == "mailbox" => {
            Ok(ActorMember::Mailbox(parse_mailbox_decl(cur)?))
        }
        _ => Err(ParseError::UnexpectedToken {
            span: cur.span(),
            description: format!(
                "expected `state`, `init`, `on`, or `mailbox` in actor body, found `{}`",
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

// ── parse_mailbox_decl ────────────────────────────────────────────────────────

/// Parse a `mailbox` configuration member of an actor.
///
/// Grammar:
///
/// ```ebnf
/// MailboxDecl    ::= "mailbox" MailboxConfig ;
/// MailboxConfig  ::= "unbounded"
///                  | "bounded" IntLit MailboxPolicy ;
/// MailboxPolicy  ::= "drop" ("newest" | "oldest")
///                  | "error" ;
/// ```
///
/// `mailbox` is a contextual member-introducer, recognized only at actor-body
/// member position. Outside actor bodies it remains an ordinary identifier.
/// The configuration words (`unbounded`, `bounded`, `drop`, `newest`,
/// `oldest`, `error`) are equally contextual.
///
/// `drop oldest` is accepted lexically so the parser produces a clean
/// diagnostic later — the typechecker rejects it until the broker mechanism
/// ships in a future cut.
///
/// Precondition: `cur.peek()` is `Token::LowerIdent("mailbox")`.
pub(crate) fn parse_mailbox_decl(cur: &mut Cursor<'_>) -> Result<MailboxDecl, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `mailbox`

    let kind_span = cur.span();
    let (config, end) = match cur.peek().clone() {
        Token::LowerIdent(ref s) if s == "unbounded" => {
            let span = kind_span;
            cur.bump();
            (MailboxConfig::Unbounded, span)
        }
        Token::LowerIdent(ref s) if s == "bounded" => {
            cur.bump();
            let (capacity, _cap_span) = parse_mailbox_capacity(cur)?;
            let (policy, policy_span) = parse_mailbox_policy(cur)?;
            (MailboxConfig::Bounded { capacity, policy }, policy_span)
        }
        _ => {
            return Err(ParseError::Expected {
                span: kind_span,
                expected: "`unbounded` or `bounded`",
                found: cur.peek().to_string(),
            });
        }
    };

    Ok(MailboxDecl {
        config,
        span: start.merge(end),
    })
}

/// Parse the capacity `N` of a `bounded N` mailbox.
///
/// `N` must be a positive `i64` literal. Zero, negative, or overflowing
/// values surface as `P023 MailboxBoundInvalid`. Non-integer tokens surface
/// as `P001 Expected`. The four integer token shapes (`IntDec`, `IntBin`,
/// `IntOct`, `IntHex`) are dispatched here in the same way `ridge-lower`
/// dispatches literal lowering.
fn parse_mailbox_capacity(cur: &mut Cursor<'_>) -> Result<(i64, ridge_ast::Span), ParseError> {
    let span = cur.span();
    let (raw, radix, prefix) = match cur.peek().clone() {
        Token::IntDec(raw) => (raw, 10, ""),
        Token::IntBin(raw) => (raw, 2, "0b"),
        Token::IntOct(raw) => (raw, 8, "0o"),
        Token::IntHex(raw) => (raw, 16, "0x"),
        _ => {
            return Err(ParseError::Expected {
                span,
                expected: "<positive integer literal>",
                found: cur.peek().to_string(),
            });
        }
    };
    cur.bump();
    let cleaned = raw.trim_start_matches(prefix).replace('_', "");
    match i64::from_str_radix(&cleaned, radix) {
        Ok(n) if n >= 1 => Ok((n, span)),
        _ => Err(ParseError::MailboxBoundInvalid { span, raw }),
    }
}

/// Parse the policy keyword(s) of a `bounded N <policy>` mailbox.
///
/// Returns the policy and the span of its last consumed token. Missing or
/// unknown policies surface as `P022 MailboxPolicyMissing`.
fn parse_mailbox_policy(
    cur: &mut Cursor<'_>,
) -> Result<(MailboxPolicy, ridge_ast::Span), ParseError> {
    let head_span = cur.span();
    match cur.peek().clone() {
        Token::LowerIdent(ref s) if s == "drop" => {
            cur.bump();
            let p_span = cur.span();
            match cur.peek().clone() {
                Token::LowerIdent(ref t) if t == "newest" => {
                    cur.bump();
                    Ok((MailboxPolicy::DropNewest, p_span))
                }
                Token::LowerIdent(ref t) if t == "oldest" => {
                    cur.bump();
                    Ok((MailboxPolicy::DropOldest, p_span))
                }
                _ => Err(ParseError::Expected {
                    span: p_span,
                    expected: "`newest` or `oldest`",
                    found: cur.peek().to_string(),
                }),
            }
        }
        Token::LowerIdent(ref s) if s == "error" => {
            cur.bump();
            Ok((MailboxPolicy::Error, head_span))
        }
        _ => Err(ParseError::MailboxPolicyMissing { span: head_span }),
    }
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
        parse_type_decl(&mut cur, vis, None, false)
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

    #[test]
    fn parse_fn_with_db_cap() {
        let f = parse_fn("fn db queryUser id = id").expect("should parse");
        assert_eq!(f.name.text, "queryUser");
        assert_eq!(f.caps, vec![Capability::Db]);
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

    /// Parse the first parameter of `fn foo <here> = …`.
    fn first_param(src: &str) -> Result<Param, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        let _ = parse_visibility(&mut cur).unwrap();
        cur.expect(&Token::KwFn).unwrap();
        let _caps = parse_cap_list(&mut cur);
        // skip the fn name
        match cur.peek().clone() {
            Token::LowerIdent(_) => cur.bump(),
            _ => panic!("expected fn name"),
        };
        parse_param_top(&mut cur)
    }

    #[test]
    fn parse_annotated_name_param_stays_annotated() {
        // The common `(name: Type)` form is unchanged by the L9 pattern path.
        match first_param("fn foo (count: Int) = count").unwrap() {
            Param::Annotated { name, .. } => assert_eq!(name.text, "count"),
            other => panic!("expected Param::Annotated, got {other:?}"),
        }
    }

    #[test]
    fn parse_record_pattern_param() {
        // `(Point { x, y }: Point)` destructures in the binder.
        match first_param("fn area (Point { x, y }: Point) = x").unwrap() {
            Param::PatternAnnotated { pat, ty, .. } => {
                assert!(
                    matches!(
                        pat,
                        ridge_ast::Pattern::Constructor {
                            fields: Some(_),
                            ..
                        }
                    ),
                    "expected a record-body constructor pattern, got {pat:?}"
                );
                assert!(matches!(ty, Type::Named { .. }));
            }
            other => panic!("expected Param::PatternAnnotated, got {other:?}"),
        }
    }

    #[test]
    fn parse_constructor_pattern_param() {
        // `(Json body: Json NewUser)` unwraps a single-constructor type.
        match first_param("fn handle (Json body: Json NewUser) = body").unwrap() {
            Param::PatternAnnotated { pat, .. } => {
                assert!(
                    matches!(pat, ridge_ast::Pattern::Constructor { fields: None, .. }),
                    "expected a constructor pattern, got {pat:?}"
                );
            }
            other => panic!("expected Param::PatternAnnotated, got {other:?}"),
        }
    }

    #[test]
    fn parse_wildcard_pattern_param() {
        // `(_: Unit)` is now an irrefutable wildcard param rather than P012.
        match first_param("fn ignore (_: Unit) = 0").unwrap() {
            Param::PatternAnnotated { pat, .. } => {
                assert!(matches!(pat, ridge_ast::Pattern::Wildcard { .. }));
            }
            other => panic!("expected Param::PatternAnnotated, got {other:?}"),
        }
    }

    #[test]
    fn parse_unannotated_pattern_param_still_p012() {
        // A pattern param without a type annotation is still rejected.
        let result = first_param("fn f (Some x) = x");
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
            other @ Constructor::Record { .. } => panic!("expected Positional, got {other:?}"),
        }
        match &alts[1] {
            Constructor::Positional { name, args, .. } => {
                assert_eq!(name.text, "Rectangle");
                assert_eq!(args.len(), 2);
            }
            other @ Constructor::Record { .. } => panic!("expected Positional, got {other:?}"),
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
            other @ Constructor::Record { .. } => panic!("expected Positional, got {other:?}"),
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
    // parse_type_union_record_variant_not_last — regression guard
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_union_record_variant_not_last() {
        // `Circle { radius: Int } | Square` — a record-variant constructor
        // followed by another alternative. The `|` sits past the record body,
        // so the union dispatcher must scan across the balanced `{ … }` to see
        // it. A brace-blind lookahead misclassifies this as a type alias.
        let td = parse_td("type Shape = Circle { radius: Int } | Square").expect("should parse");
        assert_eq!(td.name.text, "Shape");
        let alts = match &td.body {
            TypeBody::Union(u) => &u.alternatives,
            other => panic!("expected Union, got {other:?}"),
        };
        assert_eq!(alts.len(), 2);
        match &alts[0] {
            Constructor::Record { name, body, .. } => {
                assert_eq!(name.text, "Circle");
                assert_eq!(body.fields.len(), 1);
                assert_eq!(body.fields[0].name.text, "radius");
            }
            other @ Constructor::Positional { .. } => panic!("expected Record, got {other:?}"),
        }
        assert!(
            matches!(&alts[1], Constructor::Positional { name, args, .. }
                if name.text == "Square" && args.is_empty())
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_union_record_variant_both — regression guard
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_union_record_variant_both() {
        // Two record variants in one union; the multi-field record body of the
        // first must not swallow the `|` separator.
        let td = parse_td("type Shape = Circle { radius: Int } | Rect { w: Int, h: Int }")
            .expect("should parse");
        let alts = match &td.body {
            TypeBody::Union(u) => &u.alternatives,
            other => panic!("expected Union, got {other:?}"),
        };
        assert_eq!(alts.len(), 2);
        match &alts[0] {
            Constructor::Record { name, body, .. } => {
                assert_eq!(name.text, "Circle");
                assert_eq!(body.fields.len(), 1);
            }
            other @ Constructor::Positional { .. } => panic!("expected Record, got {other:?}"),
        }
        match &alts[1] {
            Constructor::Record { name, body, .. } => {
                assert_eq!(name.text, "Rect");
                assert_eq!(body.fields.len(), 2);
            }
            other @ Constructor::Positional { .. } => panic!("expected Record, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // parse_type_union_record_variant_last — regression guard
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_type_union_record_variant_last() {
        // The already-working case: a record variant as the final alternative.
        // The leading `|` is found before the brace, so this parsed before the
        // brace-aware fix too; keep it green so the fix doesn't regress it.
        let td = parse_td("type Shape = Square | Circle { radius: Int }").expect("should parse");
        let alts = match &td.body {
            TypeBody::Union(u) => &u.alternatives,
            other => panic!("expected Union, got {other:?}"),
        };
        assert_eq!(alts.len(), 2);
        assert!(matches!(&alts[0], Constructor::Positional { name, .. } if name.text == "Square"));
        assert!(matches!(&alts[1], Constructor::Record { name, .. } if name.text == "Circle"));
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
    // parse_class_keyword_dispatches_to_class_decl
    // The `class` keyword now parses a real class declaration rather than
    // producing P013 DeferredFeature.
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_class_keyword_dispatches_to_class_decl() {
        let toks = lex("class Eq a =\n    eq (x: a) (y: a) -> Bool\n");
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        let result = parse_item(&mut cur, None, vis);
        assert!(result.is_ok(), "class declaration should parse: {result:?}");
        assert!(
            matches!(result.unwrap(), Item::ClassDecl(_)),
            "expected Item::ClassDecl"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // opaque types (O1): the `opaque` modifier sets TypeDecl.opaque, is valid
    // on records and unions, and is rejected on aliases with P032.
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_opaque_record_sets_flag() {
        let toks = lex("opaque type Id = { raw: Int }\n");
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        match parse_item(&mut cur, None, vis).expect("opaque record should parse") {
            Item::Type(td) => {
                assert!(td.opaque, "expected opaque flag set");
                assert_eq!(td.name.text, "Id");
                assert!(matches!(td.body, TypeBody::Record(_)));
            }
            other => panic!("expected Item::Type, got {other:?}"),
        }
    }

    #[test]
    fn parse_opaque_union_sets_flag() {
        let toks = lex("opaque type Color = Red | Green\n");
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        let item = parse_item(&mut cur, None, vis).expect("opaque union should parse");
        assert!(matches!(item, Item::Type(td) if td.opaque));
    }

    #[test]
    fn parse_plain_type_has_opaque_false() {
        let toks = lex("type Id = { raw: Int }\n");
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        let item = parse_item(&mut cur, None, vis).expect("type should parse");
        assert!(matches!(item, Item::Type(td) if !td.opaque));
    }

    #[test]
    fn parse_opaque_alias_is_rejected_p032() {
        let toks = lex("opaque type Id = Int\n");
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        let err = parse_item(&mut cur, None, vis).expect_err("opaque alias must be rejected");
        assert_eq!(err.code(), "P032");
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

    // ── Typeclass helpers ─────────────────────────────────────────────────────

    fn parse_cls(src: &str) -> Result<ClassDecl, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        let vis = parse_visibility(&mut cur).unwrap();
        parse_class_decl(&mut cur, vis, None)
    }

    fn parse_inst(src: &str) -> Result<InstanceDecl, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        parse_instance_decl(&mut cur, None)
    }

    // ── parse_class_decl_basic ────────────────────────────────────────────────
    // `class Show a = \n    toText (x: a) -> Text` — minimal class.
    #[test]
    fn parse_class_decl_basic() {
        // "Show" must be desugared to "ToText" in the parsed AST.
        let src = "class Show a =\n    toText (x: a) -> Text\n";
        let cd = parse_cls(src).expect("should parse");
        assert_eq!(cd.name.text, "ToText", "Show must desugar to ToText");
        assert_eq!(cd.ty_vars.len(), 1);
        assert_eq!(cd.ty_vars[0].text, "a");
        assert!(cd.superclasses.is_empty());
        assert_eq!(cd.methods.len(), 1);
        assert_eq!(cd.methods[0].name.text, "toText");
    }

    // ── parse_class_decl_with_superclass ─────────────────────────────────────
    // `class Ord a where Eq a = compare (x: a) (y: a) -> Ordering`
    #[test]
    fn parse_class_decl_with_superclass() {
        let src = "class Ord a where Eq a =\n    compare (x: a) (y: a) -> Ordering\n";
        let cd = parse_cls(src).expect("should parse");
        assert_eq!(cd.name.text, "Ord");
        assert_eq!(cd.superclasses.len(), 1);
        assert_eq!(cd.superclasses[0].class.text, "Eq");
        assert_eq!(cd.superclasses[0].ty_vars[0].text, "a");
        assert_eq!(cd.methods.len(), 1);
        assert_eq!(cd.methods[0].name.text, "compare");
    }

    // ── parse_class_decl_with_fundep ─────────────────────────────────────────
    // `class Refinable q p | q -> p = …` — a single functional dependency.
    #[test]
    fn parse_class_decl_with_fundep() {
        let src = "class Refinable q p | q -> p =\n    refine (pred: p) (x: q) -> q\n";
        let cd = parse_cls(src).expect("should parse");
        assert_eq!(cd.ty_vars.len(), 2);
        assert_eq!(cd.fundeps.len(), 1);
        let from: Vec<&str> = cd.fundeps[0].from.iter().map(|i| i.text.as_str()).collect();
        let to: Vec<&str> = cd.fundeps[0].to.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(from, vec!["q"]);
        assert_eq!(to, vec!["p"]);
    }

    // ── parse_class_decl_fundep_multi ────────────────────────────────────────
    // Several determining variables and comma-separated dependencies.
    #[test]
    fn parse_class_decl_fundep_multi() {
        let src = "class C a b c | a b -> c, c -> a =\n    f (x: a) -> c\n";
        let cd = parse_cls(src).expect("should parse");
        assert_eq!(cd.fundeps.len(), 2);
        let from0: Vec<&str> = cd.fundeps[0].from.iter().map(|i| i.text.as_str()).collect();
        let to0: Vec<&str> = cd.fundeps[0].to.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(from0, vec!["a", "b"]);
        assert_eq!(to0, vec!["c"]);
        assert_eq!(cd.fundeps[1].from[0].text, "c");
        assert_eq!(cd.fundeps[1].to[0].text, "a");
    }

    // ── parse_class_decl_fundep_missing_arrow_p030 ───────────────────────────
    // A functional dependency without `->` must produce P030.
    #[test]
    fn parse_class_decl_fundep_missing_arrow_p030() {
        let src = "class C a b | a b =\n    f (x: a) -> b\n";
        let err = parse_cls(src).expect_err("should fail");
        assert_eq!(err.code(), "P030");
    }

    // ── parse_class_decl_empty_body_p030 ─────────────────────────────────────
    // An empty body must produce P030.
    #[test]
    fn parse_class_decl_empty_body_p030() {
        let src = "class Foo a =\n    \n";
        let err = parse_cls(src).expect_err("should fail");
        assert_eq!(err.code(), "P030");
    }

    // ── parse_class_decl_fn_keyword_p030 ─────────────────────────────────────
    // A method using the `fn` keyword must produce P030.
    #[test]
    fn parse_class_decl_fn_keyword_p030() {
        let src = "class Foo a =\n    fn bar (x: a) -> Text\n";
        let err = parse_cls(src).expect_err("should fail");
        assert_eq!(err.code(), "P030");
    }

    // ── parse_instance_decl_basic ─────────────────────────────────────────────
    // `instance Show Color = toText (c: Color) -> Text = "red"` — minimal instance.
    #[test]
    fn parse_instance_decl_basic() {
        // Show→ToText desugar applies in instance heads too.
        let src = "instance Show Color =\n    toText (c: Color) -> Text = \"red\"\n";
        let id = parse_inst(src).expect("should parse");
        assert_eq!(id.class.text, "ToText", "Show must desugar to ToText");
        assert_eq!(id.methods.len(), 1);
        assert_eq!(id.methods[0].name.text, "toText");
    }

    // ── parse_instance_decl_empty_body_p031 ───────────────────────────────────
    // An empty instance body must produce P031.
    #[test]
    fn parse_instance_decl_empty_body_p031() {
        let src = "instance Foo Color =\n    \n";
        let err = parse_inst(src).expect_err("should fail");
        assert_eq!(err.code(), "P031");
    }

    // ── parse_instance_where_on_head ─────────────────────────────────────────
    // A `where` clause on an instance head is parsed successfully and the
    // constraints are attached to the returned `InstanceDecl`.
    #[test]
    fn parse_instance_where_on_head() {
        let src = "instance Foo (Bar a) where Baz a =\n    foo (x: Bar a) -> Text = \"x\"\n";
        let decl = parse_inst(src).expect("should parse");
        assert_eq!(decl.constraints.len(), 1);
        assert_eq!(decl.constraints[0].class.text, "Baz");
        assert_eq!(decl.constraints[0].ty_vars[0].text, "a");
        assert_eq!(decl.head.len(), 1, "single-atom parametric head");
    }

    // ── parse_instance_non_parametric_no_constraints ─────────────────────────
    // A plain non-parametric instance (`instance Encode Int`) produces an empty
    // `constraints` list — no regression.
    #[test]
    fn parse_instance_non_parametric_no_constraints() {
        let src = "instance Encode Int =\n    encode (x: Int) -> Text = \"0\"\n";
        let decl = parse_inst(src).expect("should parse");
        assert!(
            decl.constraints.is_empty(),
            "non-parametric instance must have empty constraints"
        );
    }

    // ── parse_instance_encode_list_a ─────────────────────────────────────────
    // `instance Encode (List a) where Encode a` is the canonical parametric
    // instance form.
    #[test]
    fn parse_instance_encode_list_a() {
        let src = "instance Encode (List a) where Encode a =\n    encode (xs: a) -> Text = \"x\"\n";
        let decl = parse_inst(src).expect("should parse");
        assert_eq!(decl.constraints.len(), 1);
        assert_eq!(decl.constraints[0].class.text, "Encode");
        assert_eq!(decl.constraints[0].ty_vars[0].text, "a");
    }

    // ── parse_fn_where_clause ─────────────────────────────────────────────────
    // `fn f (x: a) -> Text where Show a = x` — desugars Show→ToText.
    #[test]
    fn parse_fn_where_clause() {
        let src = "fn f (x: a) -> Text where Show a = x\n";
        let fd = parse_fn(src).expect("should parse");
        assert_eq!(fd.constraints.len(), 1);
        assert_eq!(
            fd.constraints[0].class.text, "ToText",
            "Show must desugar to ToText in where clause"
        );
        assert_eq!(fd.constraints[0].ty_vars[0].text, "a");
    }

    // ── parse_fn_no_where_clause ──────────────────────────────────────────────
    // An ordinary fn without a where clause has empty constraints.
    #[test]
    fn parse_fn_no_where_clause() {
        let src = "fn greet (name: Text) -> Text = \"hi\"\n";
        let fd = parse_fn(src).expect("should parse");
        assert!(
            fd.constraints.is_empty(),
            "unconstrained fn must have empty constraints"
        );
    }

    // ── parse_deriving_basic ──────────────────────────────────────────────────
    // `type Color = Red | Green | Blue deriving (Show, Eq, Ord)`.
    #[test]
    fn parse_deriving_basic() {
        let src = "type Color = Red | Green | Blue deriving (Show, Eq, Ord)\n";
        let td = parse_td(src).expect("should parse");
        assert_eq!(td.deriving.len(), 3);
        assert_eq!(
            td.deriving[0].text, "ToText",
            "Show must desugar to ToText in deriving list"
        );
        assert_eq!(td.deriving[1].text, "Eq");
        assert_eq!(td.deriving[2].text, "Ord");
    }

    // ── parse_show_alias_desugars ─────────────────────────────────────────────
    // Confirm the AST never contains the string "Show" as a class name.
    #[test]
    fn parse_show_alias_desugars() {
        // Class declaration with Show.
        let src_class = "class Show a =\n    toText (x: a) -> Text\n";
        let cd = parse_cls(src_class).expect("class should parse");
        assert_ne!(
            cd.name.text, "Show",
            "class name must not be Show after desugar"
        );

        // Instance declaration with Show.
        let src_inst = "instance Show Color =\n    toText (c: Color) -> Text = \"x\"\n";
        let id = parse_inst(src_inst).expect("instance should parse");
        assert_ne!(
            id.class.text, "Show",
            "instance class must not be Show after desugar"
        );

        // Deriving clause with Show.
        let src_type = "type C = A | B deriving (Show)\n";
        let td = parse_td(src_type).expect("type should parse");
        assert!(
            td.deriving.iter().all(|n| n.text != "Show"),
            "deriving list must not contain Show after desugar"
        );

        // Where clause with Show.
        let src_fn = "fn f (x: a) -> Text where Show a = x\n";
        let fd = parse_fn(src_fn).expect("fn should parse");
        assert!(
            fd.constraints.iter().all(|c| c.class.text != "Show"),
            "where clause must not contain Show after desugar"
        );
    }

    // ── parse_type_no_deriving ────────────────────────────────────────────────
    // A type without a deriving clause has an empty deriving list.
    #[test]
    fn parse_type_no_deriving() {
        let src = "type Color = Red | Green | Blue\n";
        let td = parse_td(src).expect("should parse");
        assert!(
            td.deriving.is_empty(),
            "type without deriving must have empty deriving list"
        );
    }
}
