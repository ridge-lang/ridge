//! Completion support: cursor-context detection and item shaping.
//!
//! The context is detected by scanning the current line backwards from the
//! cursor — no re-parse — because the buffer is usually syntactically
//! incomplete while the user is typing. Mis-detection degrades to offering the
//! broad expression candidates, which is never wrong, only wider.

use tower_lsp::lsp_types::CompletionItemKind;

/// A completion candidate before conversion to an LSP `CompletionItem`.
pub struct CompletionItemData {
    /// Inserted/displayed text.
    pub label: String,
    /// Item category (drives the editor's icon).
    pub kind: CompletionItemKind,
    /// Sort key — a leading digit groups locals before module symbols, etc.
    pub sort_text: String,
    /// Optional one-line detail (e.g. a rendered type).
    pub detail: Option<String>,
}

/// What the cursor position calls for.
pub(crate) enum Context {
    /// After `Alias.` — complete that module's exported symbols.
    Member { alias: String, prefix: String },
    /// After `:` or `->` — complete type names.
    Type { prefix: String },
    /// Default — complete locals, module symbols, imports, keywords.
    Expr { prefix: String },
    /// Inside a comment or string, or at a definition site: offer nothing.
    None,
}

/// Detect the completion context at byte `offset` within `src`.
#[must_use]
pub(crate) fn detect_context(src: &str, offset: usize) -> Context {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map_or(0, |nl| nl + 1);
    // Text on the current line up to (not including) the cursor.
    let line = &src[line_start..offset];

    // Inside a comment or string → no completion.
    if in_comment_or_string(line) {
        return Context::None;
    }

    // The partial identifier being typed (trailing ident run).
    let prefix = trailing_ident(line);
    let before = &line[..line.len() - prefix.len()];
    let trimmed = before.trim_end();

    // `Alias.partial` — member access on a (probable) module alias.
    if let Some(rest) = trimmed.strip_suffix('.') {
        let alias = trailing_ident(rest);
        if !alias.is_empty() {
            return Context::Member {
                alias: alias.to_owned(),
                prefix: prefix.to_owned(),
            };
        }
    }

    // Type position: an annotation `: T` or a return arrow `-> T`.
    if trimmed.ends_with("->") || trimmed.ends_with(':') {
        return Context::Type {
            prefix: prefix.to_owned(),
        };
    }

    // Definition site: the name right after `fn` / `let` / `var` is being typed,
    // so suggesting existing names (or the half-typed name) is unhelpful.
    if at_definition_site(line) {
        return Context::None;
    }

    Context::Expr {
        prefix: prefix.to_owned(),
    }
}

/// The trailing run of identifier characters (`[A-Za-z0-9_]`).
fn trailing_ident(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = bytes.len();
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    &s[start..]
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `true` when the cursor sits inside a `--` comment or an open string literal
/// on this line.
fn in_comment_or_string(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'-' && bytes.get(i + 1) == Some(&b'-') {
            // A line comment opens here and runs to end-of-line, so the cursor
            // (further right) is inside it.
            return true;
        }
        i += 1;
    }
    in_str
}

/// `true` when the line is introducing a binding name (`fn`/`let`/`var`) and the
/// cursor is still on the name, before any `=` or parameter list.
fn at_definition_site(line: &str) -> bool {
    let t = line.trim_start();
    let t = t.strip_prefix("pub ").map_or(t, str::trim_start);
    for kw in ["fn ", "let ", "var "] {
        if let Some(rest) = t.strip_prefix(kw) {
            // Once an `=` appears we are past the name into the body.
            return !rest.contains('=');
        }
    }
    false
}

/// Maps a Ridge symbol kind to an LSP completion-item kind.
#[must_use]
pub(crate) const fn symbol_kind(kind: &ridge_resolve::SymbolKind) -> CompletionItemKind {
    use ridge_resolve::SymbolKind;
    match kind {
        SymbolKind::Fn { .. } => CompletionItemKind::FUNCTION,
        SymbolKind::Const => CompletionItemKind::CONSTANT,
        SymbolKind::Type { .. } | SymbolKind::Actor { .. } => CompletionItemKind::CLASS,
        SymbolKind::Constructor { .. } => CompletionItemKind::ENUM_MEMBER,
        SymbolKind::FieldAccessor { .. } => CompletionItemKind::FIELD,
        // `SymbolKind` is #[non_exhaustive]; any future kind shows as a value.
        _ => CompletionItemKind::VALUE,
    }
}

/// Ridge keywords offered in expression position.
pub(crate) const KEYWORDS: &[&str] = &[
    "fn", "let", "var", "if", "then", "else", "match", "with", "type", "actor", "on", "init",
    "pub", "import", "as", "in", "try", "guard", "spawn", "return",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(line_with_cursor: &str) -> Context {
        // `|` marks the cursor.
        let offset = line_with_cursor.find('|').expect("cursor marker");
        let src = line_with_cursor.replace('|', "");
        detect_context(&src, offset)
    }

    #[test]
    fn member_access_detected() {
        match ctx("  List.ma|") {
            Context::Member { alias, prefix } => {
                assert_eq!(alias, "List");
                assert_eq!(prefix, "ma");
            }
            _ => panic!("expected member access"),
        }
    }

    #[test]
    fn type_position_detected() {
        assert!(matches!(ctx("fn f (x: I|)"), Context::Type { .. }));
        assert!(matches!(ctx("fn f x -> T|"), Context::Type { .. }));
    }

    #[test]
    fn comment_and_string_are_none() {
        assert!(matches!(ctx("-- a comment here|"), Context::None));
        assert!(matches!(ctx("x = \"in a string |"), Context::None));
    }

    #[test]
    fn definition_site_is_none() {
        assert!(matches!(ctx("fn gre|"), Context::None));
        assert!(matches!(ctx("  let coun|"), Context::None));
        // Past the `=`, the body is an expression position again.
        assert!(matches!(ctx("let x = co|"), Context::Expr { .. }));
    }

    #[test]
    fn expression_position_default() {
        match ctx("  resul|") {
            Context::Expr { prefix } => assert_eq!(prefix, "resul"),
            _ => panic!("expected expr"),
        }
    }
}
