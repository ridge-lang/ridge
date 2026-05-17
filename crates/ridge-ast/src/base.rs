//! Base types shared across many AST nodes:
//! `Visibility`, `Capability`, `PrimitiveType`, and `DocComment`.

use crate::Span;

// ── Visibility ────────────────────────────────────────────────────────────────

/// Visibility modifier on a declaration.
///
/// The default (no keyword) is `Private`, i.e. visible only within the current
/// module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Visibility {
    /// Not exported; visible only within the current module (default).
    #[default]
    Private,
    /// Exported; visible to all importers.
    Pub,
    /// Exported only within the same package (the `pub(internal)` modifier).
    PubInternal,
}

// ── Capability ────────────────────────────────────────────────────────────────

/// An effect capability that a function or handler may require.
///
/// Capability lists appear on `fn`, `on`, and `init` declarations.  Checking
/// that the body actually uses (or does not exceed) the declared capabilities
/// is a Phase 4 concern; the parser only captures them here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Stdin / stdout / stderr I/O.
    Io,
    /// Filesystem access.
    Fs,
    /// Network access.
    Net,
    /// Clock / timer access.
    Time,
    /// Random number generation.
    Random,
    /// Environment variable access.
    Env,
    /// Process spawning / management.
    Proc,
    /// Actor spawning.
    Spawn,
    /// Foreign Function Interface.
    Ffi,
}

// ── PrimitiveType ─────────────────────────────────────────────────────────────

/// Built-in primitive types recognised by the parser via their `UPPER_IDENT`
/// spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimitiveType {
    /// 64-bit signed integer (`Int`).
    Int,
    /// 64-bit IEEE 754 double (`Float`).
    Float,
    /// Boolean (`Bool`).
    Bool,
    /// UTF-8 text string (`Text`).
    Text,
    /// Unit type (`Unit`).
    Unit,
    /// Timestamp / instant in time (`Timestamp`).
    Timestamp,
}

// ── DocComment ────────────────────────────────────────────────────────────────

/// A documentation comment block.
///
/// In Ridge, doc comments are delimited by `---` … `---` lines.  The raw body
/// text between those delimiters is stored here verbatim; rendering / Markdown
/// interpretation is deferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocComment {
    /// Raw body text between the opening and closing `---` delimiters.
    pub text: String,
    /// Source span covering the entire doc comment (including delimiters).
    pub span: Span,
}
