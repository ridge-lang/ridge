//! Parse error types.
//!
//! `ParseError` codes are **stable across releases** — downstream tooling
//! (LSP, `ariadne` renderer) keys on these strings.  Never renumber an
//! assigned code; only append new ones.
//!
//! T2 implements the four variants required by §6 T2 and §4.7:
//! - `P001 Expected`
//! - `P002 UnexpectedToken`
//! - `P006 LayoutMismatch`
//! - `P999 InternalLayoutInvariantViolated`
//!
//! T5 adds:
//! - `P018 BareRecordPattern`
//!
//! T6 adds:
//! - `P009 NonAssociativeChain`
//!
//! T7 adds:
//! - `P014 EmptyBlock`
//!
//! T10 adds:
//! - `P005 MissingType`
//! - `P012 TopLevelPatternParam`
//! - `P013 DeferredFeature`
//!
//! T11 adds:
//! - `P019 OrphanDocComment`
//!
//! Bounded mailboxes add:
//! - `P022 MailboxPolicyMissing`
//! - `P023 MailboxBoundInvalid`
//!
//! Parser hardening adds:
//! - `P028 ExpressionTooDeep`
//!
//! Opaque types add:
//! - `P032 OpaqueOnAlias`
//!
//! Guidance for forms other languages spell differently adds:
//! - `P033 LetInNotSupported`
//! - `P034 GuardKeywordInMatch`
//! - `P035 RecordUpdateSyntax`
//!
//! Later tasks (T3–T12) will extend this enum; adding variants is
//! non-breaking because the enum is not `#[non_exhaustive]` — the parser
//! crate owns all construction sites.

use ridge_ast::Span;

/// A parse error produced by `ridge-parser`.
///
/// Every variant carries a [`Span`] pointing to the offending source location
/// and a stable error code returned by [`ParseError::code`].
///
/// `Display` produces a human-readable message suitable for terminal output.
/// `ridge-diagnostics` (Phase 3) will later render these with `ariadne`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ParseError {
    /// P001 — the parser expected a specific token but found something else.
    #[error("expected {expected} but found `{found}`")]
    Expected {
        /// Source location of the unexpected token.
        span: Span,
        /// Static description of the expected token (e.g. `"<EOF>"`, `"->"`).
        expected: &'static str,
        /// The actual token's `Display` representation.
        found: String,
    },

    /// P002 — an unexpected token was encountered with no specific expectation.
    #[error("{description}")]
    UnexpectedToken {
        /// Source location of the unexpected token.
        span: Span,
        /// Human-readable description of the error.
        description: String,
    },

    /// P005 — a type annotation is required but was absent.
    ///
    /// Used by `const` declarations (which always require `: Type`) and
    /// `FieldDecl` in record types.
    #[error("missing type annotation in {context}: expected `: Type`")]
    MissingType {
        /// Source location where the type annotation was expected.
        span: Span,
        /// The syntactic context where the type was expected (e.g. `"const"`,
        /// `"field"`).
        context: &'static str,
    },

    /// P006 — an `Indent`, `Dedent`, or `Newline` token appeared in a context
    /// where the layout invariant was violated.
    #[error("layout mismatch: {hint}")]
    LayoutMismatch {
        /// Source location of the offending layout token.
        span: Span,
        /// Short description of the violation.
        hint: &'static str,
    },

    /// P009 — a non-associative operator was chained without parentheses.
    ///
    /// Ridge comparison operators (`==`, `!=`, `<`, `>`, `<=`, `>=`) are
    /// non-associative (§4.5).  Chaining them — `a == b == c` or `a < b < c`
    /// — is a parse error.  Users must parenthesise: `(a == b) == c`.
    #[error("non-associative operator `{op}` cannot be chained; add parentheses")]
    NonAssociativeChain {
        /// Source location of the second (chained) operator.
        span: Span,
        /// The operator spelling (e.g. `"=="`, `"<"`).
        op: &'static str,
    },

    /// P014 — an `INDENT`/`DEDENT` block contained no statements.
    ///
    /// A block must have at least one expression.  An immediate `DEDENT` after
    /// `INDENT` is a structural error (`P014 EmptyBlock`).
    #[error("empty block: expected at least one statement")]
    EmptyBlock {
        /// Source location of the empty block.
        span: Span,
    },

    /// P012 — a top-level function parameter was a tuple or constructor pattern.
    ///
    /// Top-level `fn` parameters must be bare identifiers or annotated
    /// identifiers only.  Full patterns (tuples, constructors, `@` bindings)
    /// are only allowed in lambda parameters.  Use a `let` binding in the body
    /// instead.
    ///
    /// Example: `fn foo (x, y) = x` is invalid; write `fn foo pair = let (x, y) = pair …`.
    #[error("tuple and constructor patterns are not allowed in top-level fn parameters")]
    TopLevelPatternParam {
        /// Source location of the invalid pattern parameter.
        span: Span,
    },

    /// P013 — a language feature is reserved but deferred to a future version.
    ///
    /// Currently: `class`, `instance`, `deriving`, and `trait` are reserved
    /// keywords (or keyword-like identifiers) with no grammar productions in
    /// 0.1.0.
    #[error("feature `{feature}` is deferred to {since}")]
    DeferredFeature {
        /// Source location of the deferred keyword.
        span: Span,
        /// Short name of the deferred feature (e.g. `"class"`, `"instance"`).
        feature: &'static str,
        /// Target version string (e.g. `"0.2.0"`).
        since: &'static str,
    },

    /// P018 — retired in 0.2.12.  Constructor-less record patterns are now
    /// fully supported.  This code is reserved and will not be reused.
    ///
    /// Previously: a record-body pattern `{ … }` was rejected when it
    /// appeared without a leading constructor name.
    // P018 retired in 0.2.12 — code reserved, will not be reused.
    #[error("P018 retired — bare record patterns are now supported")]
    BareRecordPattern {
        /// Source location of the bare `{` (kept for diagnostic-wire compat).
        span: Span,
    },

    /// P019 — a doc comment appears at a position where it cannot be attached
    /// to any declaration (e.g., trailing at end of file after the last item,
    /// or as the sole content of a file that also has items).
    ///
    /// Doc comments must immediately precede a top-level declaration.  An
    /// orphan doc comment is a warning-level error (the parser does not halt,
    /// but the comment is lost).
    #[error("doc comment at invalid position — not attached to any declaration")]
    OrphanDocComment {
        /// Source location of the orphan doc comment.
        span: Span,
    },

    /// P020 — a reserved keyword (e.g. `init`, `state`, `on`) appeared in a
    /// position that expects a plain identifier (a `let` pattern, a `fn`
    /// parameter name, a lambda parameter, …).
    ///
    /// The historical surfaces were `P002 unexpected token` (let patterns)
    /// and `P012 TopLevelPatternParam` (fn parameters), both of which read
    /// as unrelated structural errors and sent users hunting for missing
    /// braces or stray punctuation.  P020 names the cause directly and
    /// hints at the canonical fix: rename the binding.
    #[error("reserved keyword `{keyword}` cannot be used as an identifier in {position}; rename the binding")]
    ReservedKeywordAsIdent {
        /// Source location of the keyword token.
        span: Span,
        /// The keyword text (e.g. `"init"`, `"state"`).
        keyword: &'static str,
        /// Where the keyword appeared (e.g. `"a let-binding pattern"`,
        /// `"a function parameter"`).
        position: &'static str,
    },

    /// P021 — an inline record type `{ … }` in type position is syntactically
    /// malformed.  Emitted for:
    /// - A field missing the `:` separator (`{ x Int }` instead of `{ x: Int }`).
    /// - A field name that is not a lowercase identifier.
    /// - An unterminated record body (EOF or unexpected token before `}`).
    ///
    /// Well-formed inline record types are now part of the grammar; this code
    /// names parse-level faults within the record body rather than rejecting
    /// the entire form.
    #[error("P021: malformed inline record type — {description}")]
    MalformedInlineRecordType {
        /// Source location of the fault.
        span: Span,
        /// Human-readable description of the specific fault.
        description: String,
    },

    /// Kept for diagnostic-wire compatibility; no longer emitted.
    ///
    /// In versions before 0.2.12 this variant was emitted whenever `{` appeared
    /// in a type position.  It is retained so that code that matches on
    /// `ParseError` exhaustively does not require immediate updates.
    #[doc(hidden)]
    #[error("inline record types are not supported in type positions; declare a named type with `type Foo = {{ … }}` and use `Foo` here instead")]
    InlineRecordTypeInTypePosition {
        /// Source location of the opening `{`.
        span: Span,
    },

    /// P022 — `mailbox bounded N` was declared without an overflow policy.
    ///
    /// A bounded mailbox must specify how it handles overflow. The valid
    /// policies are `drop newest`, `drop oldest`, and `error`. There is no
    /// default policy: requiring an explicit choice forces the author to
    /// decide what the actor does when the bound is reached.
    #[error(
        "bounded mailbox requires an overflow policy: `drop newest`, `drop oldest`, or `error`"
    )]
    MailboxPolicyMissing {
        /// Source location where the policy was expected.
        span: Span,
    },

    /// P024 — a list pattern contains more than one `..` rest element.
    ///
    /// A list pattern may contain at most one `..`.  Write `[first, ..]` or
    /// `[first, rest @ ..]` with a single rest.
    #[error("list pattern may contain at most one `..` rest element")]
    MultipleRestInListPattern {
        /// Source location of the second `..` token.
        span: Span,
    },

    /// P025 — reserved; previously used for suffix/middle rest (now supported).
    ///
    /// This variant is kept for wire-format and downstream-tool compatibility
    /// but is never emitted by the current parser.
    #[error("rest element position restriction (unused in current version)")]
    RestSuffixNotSupported {
        /// Source location.
        span: Span,
    },

    /// P026 — a suffix or middle element in a list pattern is a refutable
    /// sub-pattern (literal, constructor, tuple, …).
    ///
    /// Suffix and middle positions — the elements that come after `..` in
    /// `[.., last]` or `[first, .., last]` — must be irrefutable (a variable
    /// or `_`) in this version.  Refutable sub-patterns there require runtime
    /// element extraction that cannot be expressed cleanly as an Erlang case
    /// clause pattern, so they are rejected with this diagnostic.
    ///
    /// Workaround: bind the element to a variable in the slice pattern and
    /// match further with a nested `match` or a `when` guard.
    #[error(
        "refutable patterns are not allowed in suffix or middle positions of a list slice pattern; \
         use a variable or `_` here"
    )]
    RefutableSliceElement {
        /// Source location of the refutable sub-pattern.
        span: Span,
    },

    /// P023 — `mailbox bounded N` was given a capacity that is not a positive
    /// `i64` literal.
    ///
    /// Capacity must be a literal integer in the range `1..=i64::MAX`. Zero,
    /// negative, and overflowing values are rejected at parse time.
    #[error("mailbox capacity must be a positive integer literal (got `{raw}`)")]
    MailboxBoundInvalid {
        /// Source location of the offending integer literal.
        span: Span,
        /// The raw text of the rejected literal.
        raw: String,
    },

    /// P027 — `@test` was not given a string-literal argument.
    ///
    /// The `@test` attribute requires a string literal immediately after the
    /// keyword: `@test "my test name"`.  Any other token at that position
    /// produces this diagnostic.
    #[error("`@test` requires a string literal argument — write `@test \"<name>\"`")]
    TestAttrArgNotString {
        /// Source location of the unexpected token.
        span: Span,
    },

    /// P028 — syntax nested deeper than the parser's recursion limit.
    ///
    /// Expressions, types, and patterns are all parsed by recursive descent, so
    /// deeply nested input (thousands of nested parentheses, lists, operators,
    /// arrow types, or list patterns) would otherwise overflow the native stack
    /// and abort the whole compiler with no diagnostic.  A fixed depth limit,
    /// shared across all three, stops the descent and reports this error
    /// instead.  No hand-written or formatter-produced program reaches the
    /// limit; only pathological or adversarial input does.
    #[error("syntax nesting too deep (limit {limit})")]
    ExpressionTooDeep {
        /// Source location at which the limit was reached.
        span: Span,
        /// The maximum nesting depth the parser allows.
        limit: u32,
    },

    /// P030 — a `class` declaration is structurally malformed.
    ///
    /// Fired when the body is empty, a method uses the `fn` keyword, or a
    /// method signature contains a body expression. Write bare signatures:
    /// `methodName (param: ParamType) -> RetType`.
    #[error("malformed class declaration: {reason}")]
    MalformedClassDecl {
        /// Source location of the fault.
        span: Span,
        /// Human-readable description of the specific fault.
        reason: String,
    },

    /// P031 — an `instance` declaration is structurally malformed.
    ///
    /// Fired when the body is empty, a method is missing its body expression,
    /// or a `where` clause appears on the instance head (instance heads cannot
    /// carry constraints in this release). Write full definitions:
    /// `methodName (param: ParamType) -> RetType = body`.
    #[error("malformed instance declaration: {reason}")]
    MalformedInstanceDecl {
        /// Source location of the fault.
        span: Span,
        /// Human-readable description of the specific fault.
        reason: String,
    },

    /// P032 — `opaque` was applied to a type alias. Opacity hides a type's
    /// constructor and fields from other modules; an alias has neither, so the
    /// modifier is meaningless there. Only records and unions can be opaque.
    #[error("`opaque` is not allowed on a type alias; only records and unions can be opaque")]
    OpaqueOnAlias {
        /// Source location of the offending declaration.
        span: Span,
    },

    /// P033 — a `let … in …` expression was written. Ridge `let` bindings are
    /// layout-based: the body follows on the next line at the same indentation,
    /// so there is no `in` keyword. Coming from Haskell, OCaml, F#, or Elm this
    /// is a natural mistake; the diagnostic names the Ridge shape directly
    /// instead of surfacing a bare "unexpected `in`".
    #[error(
        "`let … in` is not a Ridge expression — `let` uses layout: put the body on the next line at the same indentation as `let`"
    )]
    LetInNotSupported {
        /// Source location of the stray `in` keyword.
        span: Span,
    },

    /// P034 — a match arm used `if` to introduce its guard. Ridge match guards
    /// use `when` (`<pattern> when <condition> ->`); `if` is only the
    /// conditional expression. The offending token is the `if`, which a
    /// quick-fix can replace with `when` in place.
    #[error("match guards use `when`, not `if` — write `<pattern> when <condition> ->`")]
    GuardKeywordInMatch {
        /// Source location of the `if` keyword.
        span: Span,
    },

    /// P035 — record update was written `{ record with … }` (the OCaml/Elm/F#
    /// spelling). Ridge writes it `record with { field = value }`: the record
    /// comes first and the braces wrap only the updated fields. A quick-fix
    /// moves the record out of the braces.
    #[error(
        "record update is written `record with {{ field = value }}`, not `{{ record with … }}` — move the record out of the braces"
    )]
    RecordUpdateSyntax {
        /// Source location of the misplaced `with` keyword (the primary caret).
        span: Span,
        /// Source location of the opening `{`, used to build the quick-fix edit.
        open_brace: Span,
        /// The record expression's identifier text (the single field-shorthand
        /// name parsed before `with`), used to render the corrected form.
        record: String,
    },

    /// P999 — the lexer's bracket-suppression invariant was violated (should
    /// be unreachable; signals a lexer bug, not a user error).
    #[error("internal error: layout invariant violated inside bracketed region")]
    InternalLayoutInvariantViolated {
        /// Source location of the invariant violation.
        span: Span,
    },
}

impl ParseError {
    /// Return the stable error code string for this variant.
    ///
    /// Codes are **stable across releases** — never renumber an assigned code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Expected { .. } => "P001",
            Self::UnexpectedToken { .. } => "P002",
            Self::MissingType { .. } => "P005",
            Self::LayoutMismatch { .. } => "P006",
            Self::NonAssociativeChain { .. } => "P009",
            Self::TopLevelPatternParam { .. } => "P012",
            Self::DeferredFeature { .. } => "P013",
            Self::EmptyBlock { .. } => "P014",
            Self::BareRecordPattern { .. } => "P018",
            Self::OrphanDocComment { .. } => "P019",
            Self::ReservedKeywordAsIdent { .. } => "P020",
            Self::MalformedInlineRecordType { .. }
            | Self::InlineRecordTypeInTypePosition { .. } => "P021",
            Self::MailboxPolicyMissing { .. } => "P022",
            Self::MailboxBoundInvalid { .. } => "P023",
            Self::MultipleRestInListPattern { .. } => "P024",
            Self::RestSuffixNotSupported { .. } => "P025",
            Self::RefutableSliceElement { .. } => "P026",
            Self::TestAttrArgNotString { .. } => "P027",
            Self::ExpressionTooDeep { .. } => "P028",
            Self::MalformedClassDecl { .. } => "P030",
            Self::MalformedInstanceDecl { .. } => "P031",
            Self::OpaqueOnAlias { .. } => "P032",
            Self::LetInNotSupported { .. } => "P033",
            Self::GuardKeywordInMatch { .. } => "P034",
            Self::RecordUpdateSyntax { .. } => "P035",
            Self::InternalLayoutInvariantViolated { .. } => "P999",
        }
    }

    /// Return the source span associated with this error.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Expected { span, .. }
            | Self::UnexpectedToken { span, .. }
            | Self::MissingType { span, .. }
            | Self::LayoutMismatch { span, .. }
            | Self::NonAssociativeChain { span, .. }
            | Self::TopLevelPatternParam { span }
            | Self::DeferredFeature { span, .. }
            | Self::EmptyBlock { span }
            | Self::BareRecordPattern { span }
            | Self::OrphanDocComment { span }
            | Self::ReservedKeywordAsIdent { span, .. }
            | Self::MalformedInlineRecordType { span, .. }
            | Self::InlineRecordTypeInTypePosition { span }
            | Self::MailboxPolicyMissing { span }
            | Self::MailboxBoundInvalid { span, .. }
            | Self::MultipleRestInListPattern { span }
            | Self::RestSuffixNotSupported { span }
            | Self::RefutableSliceElement { span }
            | Self::TestAttrArgNotString { span }
            | Self::ExpressionTooDeep { span, .. }
            | Self::MalformedClassDecl { span, .. }
            | Self::MalformedInstanceDecl { span, .. }
            | Self::OpaqueOnAlias { span }
            | Self::LetInNotSupported { span }
            | Self::GuardKeywordInMatch { span }
            | Self::RecordUpdateSyntax { span, .. }
            | Self::InternalLayoutInvariantViolated { span } => *span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p001_code_and_display() {
        let e = ParseError::Expected {
            span: Span::point(0),
            expected: "<EOF>",
            found: "fn".to_string(),
        };
        assert_eq!(e.code(), "P001");
        assert!(e.to_string().contains("<EOF>"));
        assert!(e.to_string().contains("fn"));
    }

    #[test]
    fn p002_code_and_display() {
        let e = ParseError::UnexpectedToken {
            span: Span::point(5),
            description: "unexpected `}`".to_string(),
        };
        assert_eq!(e.code(), "P002");
        assert!(e.to_string().contains("`}`"));
    }

    #[test]
    fn p006_code_and_display() {
        let e = ParseError::LayoutMismatch {
            span: Span::new(10, 15),
            hint: "DEDENT outside block",
        };
        assert_eq!(e.code(), "P006");
        assert!(e.to_string().contains("DEDENT outside block"));
    }

    #[test]
    fn p021_code_and_display() {
        let e = ParseError::InlineRecordTypeInTypePosition {
            span: Span::new(27, 28),
        };
        assert_eq!(e.code(), "P021");
        let msg = e.to_string();
        assert!(msg.contains("inline record"));
        assert!(msg.contains("named type"));
    }

    #[test]
    fn p020_code_and_display() {
        let e = ParseError::ReservedKeywordAsIdent {
            span: Span::new(4, 8),
            keyword: "init",
            position: "a pattern",
        };
        assert_eq!(e.code(), "P020");
        let msg = e.to_string();
        assert!(msg.contains("`init`"));
        assert!(msg.contains("a pattern"));
        assert!(msg.contains("rename"));
    }

    #[test]
    fn p028_code_and_display() {
        let e = ParseError::ExpressionTooDeep {
            span: Span::new(0, 1),
            limit: 256,
        };
        assert_eq!(e.code(), "P028");
        let msg = e.to_string();
        assert!(msg.contains("too deep"));
        assert!(msg.contains("256"));
    }

    #[test]
    fn p999_code_and_display() {
        let e = ParseError::InternalLayoutInvariantViolated {
            span: Span::point(0),
        };
        assert_eq!(e.code(), "P999");
        assert!(e.to_string().contains("invariant"));
    }

    #[test]
    fn span_accessor_returns_carried_span() {
        let span = Span::new(3, 7);
        let e = ParseError::LayoutMismatch { span, hint: "test" };
        assert_eq!(e.span(), span);
    }

    #[test]
    fn p030_code_and_display() {
        let e = ParseError::MalformedClassDecl {
            span: Span::new(0, 5),
            reason: "class body must contain at least one method signature".to_string(),
        };
        assert_eq!(e.code(), "P030");
        let msg = e.to_string();
        assert!(msg.contains("malformed class declaration"));
        assert!(msg.contains("at least one method"));
    }

    #[test]
    fn p031_code_and_display() {
        let e = ParseError::MalformedInstanceDecl {
            span: Span::new(0, 8),
            reason: "instance body must contain at least one method definition".to_string(),
        };
        assert_eq!(e.code(), "P031");
        let msg = e.to_string();
        assert!(msg.contains("malformed instance declaration"));
        assert!(msg.contains("at least one method"));
    }

    #[test]
    fn p033_code_and_display() {
        let e = ParseError::LetInNotSupported {
            span: Span::new(10, 12),
        };
        assert_eq!(e.code(), "P033");
        let msg = e.to_string();
        assert!(msg.contains("let"));
        assert!(msg.contains("layout"));
    }

    #[test]
    fn p034_code_and_display() {
        let e = ParseError::GuardKeywordInMatch {
            span: Span::new(4, 6),
        };
        assert_eq!(e.code(), "P034");
        let msg = e.to_string();
        assert!(msg.contains("when"));
        assert!(msg.contains("if"));
    }

    #[test]
    fn p035_code_and_display() {
        let e = ParseError::RecordUpdateSyntax {
            span: Span::new(8, 12),
            open_brace: Span::new(4, 5),
            record: "p".to_string(),
        };
        assert_eq!(e.code(), "P035");
        assert_eq!(e.span(), Span::new(8, 12));
        let msg = e.to_string();
        assert!(msg.contains("record update"));
        assert!(msg.contains("with"));
    }
}
