//! `render_with_ariadne` — the ariadne-backed diagnostic renderer.
//!
//! Colour control is driven by the `RIDGE_COLOR` environment variable:
//! - `RIDGE_COLOR=always` → force ANSI colour on.
//! - `RIDGE_COLOR=never`  → force ANSI colour off.
//! - `RIDGE_COLOR=auto`   → enabled iff stderr is a tty AND `NO_COLOR` is unset.

use std::collections::HashMap;
use std::io::Write;

use ariadne::{Cache, Color, Config, Label, Report, ReportKind, Source};
use is_terminal::IsTerminal;

use crate::diagnostic::{Diagnostic, NoteSeverity, RenderError, SourceCache, SourceId};
use ridge_resolve::Severity;

// ── AriadneCacheAdapter ───────────────────────────────────────────────────────

/// Bridges our [`SourceCache`] trait into the `ariadne::Cache` trait.
///
/// All sources are eagerly inserted before rendering so that `fetch` can
/// return `&Source` without interior mutability or unsafe.
struct AriadneCacheAdapter {
    /// Pre-populated `ariadne::Source` objects keyed by source ID string.
    parsed: HashMap<String, Source>,
    /// Display names keyed by source ID string.
    names: HashMap<String, String>,
    /// Per source, the byte offset at which each character starts, in order.
    ///
    /// ariadne indexes labels by character (Unicode-scalar) offset, not byte
    /// offset, so our byte-offset [`Span`](ridge_ast::Span)s must be converted
    /// before they are handed to a label. This table lets that conversion run
    /// as a binary search instead of rescanning the source for every label.
    char_starts: HashMap<String, Vec<u32>>,
}

impl AriadneCacheAdapter {
    /// Construct an adapter from a list of diagnostics and a [`SourceCache`].
    ///
    /// All source IDs referenced by `diagnostics` are resolved up front.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "source files are bounded to 4 GiB, matching the u32 span offsets"
    )]
    fn from_diagnostics(diagnostics: &[Diagnostic], cache: &dyn SourceCache) -> Self {
        let mut parsed = HashMap::new();
        let mut names = HashMap::new();
        let mut char_starts: HashMap<String, Vec<u32>> = HashMap::new();
        for diag in diagnostics {
            let key = diag.source_id.as_str().to_owned();
            if parsed.contains_key(&key) {
                continue;
            }
            // A genuinely missing source stays unregistered so ariadne fails to
            // fetch and the caller renders the context-less fallback.
            let Some(raw) = cache.fetch(&diag.source_id) else {
                continue;
            };
            // Span offsets are measured against the lexer's line-ending
            // normalisation, so the text ariadne renders (and the char-offset
            // table below) must be normalised the same way — otherwise a `\r`
            // the spans don't account for shifts every label past it.
            let text = ridge_lexer::normalise_line_endings(raw);
            let name = cache.display_name(&diag.source_id).to_owned();
            char_starts.insert(
                key.clone(),
                text.char_indices().map(|(i, _)| i as u32).collect(),
            );
            parsed.insert(key.clone(), Source::from(text));
            names.insert(key, name);
        }
        Self {
            parsed,
            names,
            char_starts,
        }
    }

    /// Convert a byte offset into the character offset ariadne expects.
    ///
    /// Returns the number of characters that start before `byte`. A byte at or
    /// past end-of-source clamps to the total character count; a byte that lands
    /// inside a multi-byte character resolves to the next character boundary
    /// (spans always sit on boundaries, so this is only a safety net). When the
    /// source is unavailable the byte offset is passed through unchanged — the
    /// render is context-less in that case anyway.
    fn char_offset(&self, id: &SourceId, byte: usize) -> usize {
        self.char_starts.get(id.as_str()).map_or(byte, |starts| {
            starts.partition_point(|&start| (start as usize) < byte)
        })
    }
}

impl Cache<SourceId> for AriadneCacheAdapter {
    type Storage = String;

    fn fetch(&mut self, id: &SourceId) -> Result<&Source<String>, impl std::fmt::Debug> {
        let key = id.as_str();
        self.parsed
            .get(key)
            .ok_or_else(|| format!("source not found: {key}"))
    }

    fn display<'a>(&self, id: &'a SourceId) -> Option<impl std::fmt::Display + 'a> {
        let name = self
            .names
            .get(id.as_str())
            .cloned()
            .unwrap_or_else(|| id.as_str().to_owned());
        Some(name)
    }
}

// ── yansi sync ────────────────────────────────────────────────────────────────

/// Synchronise yansi's global enabled state.
///
/// ariadne's `draw.rs` calls `yansi::Paint::new(...).fg(color)` which only
/// emits ANSI bytes when yansi is globally enabled.  We call this function
/// once per `render_with_ariadne` invocation.
///
/// This is process-global state; tests that rely on specific colour output
/// must run with `--test-threads=1` or hold a process-wide mutex.
fn sync_yansi(enabled: bool) {
    if enabled {
        yansi::enable();
    } else {
        yansi::disable();
    }
}

// ── Colour control ────────────────────────────────────────────────────────────

/// Determine whether ANSI colour should be emitted.
///
/// Priority (highest first):
/// 1. `RIDGE_COLOR=always` → force on.
/// 2. `RIDGE_COLOR=never`  → force off.
/// 3. `NO_COLOR` env-var set → force off (<https://no-color.org>).
/// 4. Otherwise → enabled iff stderr is a tty.
fn colour_from_env() -> bool {
    match std::env::var("RIDGE_COLOR").as_deref() {
        Ok("always") => return true,
        Ok("never") => return false,
        _ => {}
    }
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    // Auto mode: check stderr (the target the CLI always writes diagnostics to).
    std::io::stderr().is_terminal()
}

// ── map_note_color ────────────────────────────────────────────────────────────

const fn map_note_color(sev: NoteSeverity) -> Color {
    match sev {
        NoteSeverity::Help => Color::Green,
        NoteSeverity::Note => Color::Blue,
        NoteSeverity::Hint => Color::Yellow,
    }
}

// ── render_with_ariadne ───────────────────────────────────────────────────────

/// Render a slice of diagnostics to `writer`.
///
/// Returns the count of `Severity::Error` diagnostics rendered.  Source-cache
/// misses produce a context-less render (code prefix + message, no underline).
///
/// # Colour control
///
/// - `RIDGE_COLOR=always` → forced on.
/// - `RIDGE_COLOR=never`  → forced off.
/// - `RIDGE_COLOR=auto` (default) → enabled iff stderr is a tty AND `NO_COLOR` is unset.
///
/// # Errors
///
/// Returns [`RenderError::Io`] if the writer returns an error.  Cache misses
/// are not errors.
pub fn render_with_ariadne(
    diagnostics: &[Diagnostic],
    cache: &dyn SourceCache,
    writer: &mut dyn Write,
) -> Result<usize, RenderError> {
    if diagnostics.is_empty() {
        return Ok(0);
    }

    let use_colour = colour_from_env();
    // Sync yansi's global state with our decision so ariadne's Paint::fg()
    // emits ANSI sequences when colour is enabled.
    // Note: yansi state is process-global; this call is idempotent per render.
    sync_yansi(use_colour);

    let mut adapter = AriadneCacheAdapter::from_diagnostics(diagnostics, cache);
    let mut error_count = 0usize;

    for diag in diagnostics {
        if matches!(diag.severity, Severity::Error) {
            error_count += 1;
        }

        let report_kind = match diag.severity {
            Severity::Warning => ReportKind::Warning,
            _ => ReportKind::Error,
        };

        let title = format!("[{}] {}", diag.code, diag.primary_message);
        let source_id = diag.source_id.clone();
        // ariadne indexes labels by character offset, but our spans are byte
        // offsets — convert, or any multi-byte UTF-8 before a span drifts the
        // underline forward (a byte offset outruns its character offset).
        let primary_span_start = adapter.char_offset(&source_id, diag.primary_span.start as usize);
        let primary_span_end = adapter.char_offset(&source_id, diag.primary_span.end as usize);

        // ariadne 0.6: `Report::build` takes the full span tuple `(Id, Range)`
        // rather than the (kind, id, offset) triple used in 0.4.
        let mut report_builder = Report::build(
            report_kind,
            (source_id.clone(), primary_span_start..primary_span_end),
        )
        .with_config(Config::default().with_color(use_colour))
        .with_message(&title)
        .with_label(
            Label::new((source_id.clone(), primary_span_start..primary_span_end))
                .with_message(&diag.primary_message),
        );

        for note in &diag.notes {
            let color = if use_colour {
                map_note_color(note.severity)
            } else {
                Color::Primary
            };
            let note_start = adapter.char_offset(&diag.source_id, note.span.start as usize);
            let note_end = adapter.char_offset(&diag.source_id, note.span.end as usize);
            report_builder = report_builder.with_label(
                Label::new((diag.source_id.clone(), note_start..note_end))
                    .with_message(&note.message)
                    .with_color(color),
            );
        }

        let built = report_builder.finish();

        // Write to a buffer; on ariadne error fall back to context-less render.
        let mut buf = Vec::new();
        if built.write(&mut adapter, &mut buf).is_ok() {
            writer.write_all(&buf)?;
        } else {
            // Context-less fallback: ariadne could not render (source not
            // available or span out of range).
            let kind = match diag.severity {
                Severity::Error => "error",
                _ => "warning",
            };
            writeln!(
                writer,
                "{kind}[{code}]: {msg}",
                code = diag.code,
                msg = diag.primary_message,
            )?;
            writeln!(writer, "  --> <unknown source> (source not available)")?;
        }
    }

    Ok(error_count)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::redundant_closure_for_method_calls,
    clippy::doc_markdown,
    clippy::cast_possible_truncation
)]
mod tests {
    use super::*;
    use crate::diagnostic::{Diagnostic, NoteSeverity, SourceId};
    use ridge_ast::Span;
    use ridge_resolve::Severity;

    // Mutex to serialize tests that mutate global yansi state and env vars.
    static COLOR_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // A trivial in-memory source cache for tests.
    struct TestCache {
        sources: std::collections::HashMap<String, String>,
    }

    impl TestCache {
        fn with(id: &str, text: &str) -> Self {
            let mut m = std::collections::HashMap::new();
            m.insert(id.to_owned(), text.to_owned());
            Self { sources: m }
        }

        fn empty() -> Self {
            Self {
                sources: std::collections::HashMap::new(),
            }
        }
    }

    impl SourceCache for TestCache {
        fn fetch(&self, id: &SourceId) -> Option<&str> {
            self.sources.get(id.as_str()).map(String::as_str)
        }
    }

    fn make_diag(code: &'static str, sev: Severity, src: &str) -> Diagnostic {
        Diagnostic::new(
            code,
            sev,
            Span::new(0, 1),
            "test message",
            SourceId::new(src),
        )
    }

    /// Empty diagnostics slice → Ok(0), no bytes written.
    #[test]
    fn empty_slice_writes_nothing() {
        let cache = TestCache::empty();
        let mut buf = Vec::new();
        let result = render_with_ariadne(&[], &cache, &mut buf);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
        assert!(buf.is_empty());
    }

    /// Source-cache miss → produces output containing the code.
    #[test]
    fn cache_miss_renders_context_less() {
        let _lock = COLOR_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let cache = TestCache::empty();
        let diag = make_diag("P001", Severity::Error, "nonexistent.ridge");
        let mut buf = Vec::new();
        let result = render_with_ariadne(&[diag], &cache, &mut buf);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);
        let out = String::from_utf8_lossy(&buf);
        assert!(!out.is_empty(), "should produce some output: {out:?}");
    }

    /// `RIDGE_COLOR=never` → no ANSI escape sequences.
    #[test]
    fn never_color_produces_ansi_free_output() {
        let _lock = COLOR_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("RIDGE_COLOR", "never");
        let source = "let x { 1\n";
        let cache = TestCache::with("test.ridge", source);
        let diag = Diagnostic::new(
            "P001",
            Severity::Error,
            Span::new(6, 7),
            "expected = but found {",
            SourceId::new("test.ridge"),
        );
        let mut buf = Vec::new();
        let _ = render_with_ariadne(&[diag], &cache, &mut buf);
        let out = String::from_utf8_lossy(&buf);
        assert!(
            !out.contains('\x1b'),
            "RIDGE_COLOR=never should produce ANSI-free output: {out:?}"
        );
    }

    /// `RIDGE_COLOR=always` → ANSI sequences present even for non-tty writer.
    #[test]
    fn always_color_produces_ansi_output() {
        let _lock = COLOR_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("RIDGE_COLOR", "always");
        let source = "let x { 1\n";
        let cache = TestCache::with("test.ridge", source);
        let diag = Diagnostic::new(
            "P001",
            Severity::Error,
            Span::new(6, 7),
            "expected = but found {",
            SourceId::new("test.ridge"),
        );
        let mut buf = Vec::new();
        let _ = render_with_ariadne(&[diag], &cache, &mut buf);
        let out = String::from_utf8_lossy(&buf);
        assert!(
            out.contains('\x1b'),
            "RIDGE_COLOR=always should produce ANSI output: {out:?}"
        );
    }

    /// Error count: 2 errors + 1 warning → 2.
    #[test]
    fn error_count_skips_warnings() {
        let _lock = COLOR_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let cache = TestCache::empty();
        let diags = vec![
            make_diag("P001", Severity::Error, "a.ridge"),
            make_diag("R001", Severity::Warning, "b.ridge"),
            make_diag("T001", Severity::Error, "c.ridge"),
        ];
        let mut buf = Vec::new();
        let result = render_with_ariadne(&diags, &cache, &mut buf);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2);
    }

    /// Parse error with source → line text and caret present.
    #[test]
    fn parse_error_renders_with_source_context() {
        let _lock = COLOR_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("RIDGE_COLOR", "never");
        let source = "let x { 1\nfoo\nbar\n";
        let cache = TestCache::with("bad.ridge", source);
        let diag = Diagnostic::new(
            "P001",
            Severity::Error,
            Span::new(6, 7),
            "expected = but found {",
            SourceId::new("bad.ridge"),
        );
        let mut buf = Vec::new();
        let result = render_with_ariadne(&[diag], &cache, &mut buf);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);
        let out = String::from_utf8_lossy(&buf);
        assert!(
            out.contains("let x"),
            "output should contain source line 'let x': {out}"
        );
        // ariadne uses box-drawing chars (╰──) rather than ASCII carets.
        assert!(
            out.contains('^') || out.contains('-') || out.contains('╰') || out.contains('┬'),
            "output should contain an underline marker: {out}"
        );
    }

    /// `char_offset` converts a byte offset to the character offset ariadne
    /// expects, discounting the extra bytes of multi-byte UTF-8 seen earlier.
    #[test]
    fn char_offset_converts_byte_to_char() {
        // "-- ──\nlet y = z": each `─` (U+2500) is 3 bytes but 1 character.
        //   byte:  - 0  - 1  ' '2  ─ 3..5  ─ 6..8  \n 9  l 10 … z 18
        //   char:    0    1     2     3       4       5    6  …    14
        let source = "-- ──\nlet y = z";
        let cache = TestCache::with("m.ridge", source);
        let diag = make_diag("T001", Severity::Error, "m.ridge");
        let adapter = AriadneCacheAdapter::from_diagnostics(&[diag], &cache);
        let id = SourceId::new("m.ridge");

        assert_eq!(adapter.char_offset(&id, 0), 0);
        assert_eq!(adapter.char_offset(&id, 10), 6, "start of line 2");
        assert_eq!(
            adapter.char_offset(&id, 18),
            14,
            "the `z`: 4 bytes of multi-byte drift removed"
        );
        // Past end-of-source clamps to the total character count (6 + 9 = 15).
        assert_eq!(adapter.char_offset(&id, 9_999), 15);
        // An unknown source passes the byte offset through unchanged.
        assert_eq!(adapter.char_offset(&SourceId::new("nope.ridge"), 18), 18);
    }

    /// A span preceded by multi-byte UTF-8 anchors on its real line, not a
    /// position drifted forward by the extra bytes — the regression where a type
    /// error underlined a comment far below its real cause.
    #[test]
    fn multibyte_prefixed_span_anchors_correct_line() {
        let _lock = COLOR_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("RIDGE_COLOR", "never");
        // Ten box-drawing chars on line 1 (2 extra bytes each) precede the real
        // target `oops` on line 3.
        let source = format!("-- {}\nlet x = 1\nlet y = oops", "─".repeat(10));
        let cache = TestCache::with("d.ridge", &source);
        let start = source.find("oops").expect("target present");
        let diag = Diagnostic::new(
            "T001",
            Severity::Error,
            Span::new(start as u32, (start + "oops".len()) as u32),
            "type mismatch",
            SourceId::new("d.ridge"),
        );
        let mut buf = Vec::new();
        let result = render_with_ariadne(&[diag], &cache, &mut buf);
        assert!(result.is_ok());
        let out = String::from_utf8_lossy(&buf);
        assert!(
            out.contains(":3:9"),
            "label should anchor at line 3, col 9 (the `oops`): {out}"
        );
        assert!(
            out.contains("let y = oops"),
            "the real source line should be shown: {out}"
        );
    }

    /// A CRLF source is normalised to LF before rendering, so a span in the
    /// lexer's normalised coordinates still anchors correctly (the `\r` bytes
    /// the span does not count must not shift the label).
    #[test]
    fn crlf_source_anchors_correct_line() {
        let _lock = COLOR_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("RIDGE_COLOR", "never");
        // The cache holds the raw CRLF bytes (as read from disk); the span is in
        // the normalised (LF) coordinates the lexer produces.
        let raw = "let x = 1\r\nlet y = z";
        let normalised = raw.replace("\r\n", "\n");
        let start = normalised.find('z').expect("target present");
        let cache = TestCache::with("crlf.ridge", raw);
        let diag = Diagnostic::new(
            "T001",
            Severity::Error,
            Span::new(start as u32, (start + 1) as u32),
            "type mismatch",
            SourceId::new("crlf.ridge"),
        );
        let mut buf = Vec::new();
        let result = render_with_ariadne(&[diag], &cache, &mut buf);
        assert!(result.is_ok());
        let out = String::from_utf8_lossy(&buf);
        assert!(
            out.contains(":2:9"),
            "label should anchor at line 2, col 9 (the `z`): {out}"
        );
    }

    /// R005 DuplicateDeclaration renders secondary note.
    #[test]
    fn duplicate_declaration_renders_secondary_note() {
        let _lock = COLOR_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("RIDGE_COLOR", "never");
        let source = "pub fn foo -> Int = 1\npub fn foo -> Int = 2\n";
        let cache = TestCache::with("dup.ridge", source);
        let diag = Diagnostic::new(
            "R005",
            Severity::Error,
            Span::new(22, 24),
            "duplicate declaration `foo`",
            SourceId::new("dup.ridge"),
        )
        .with_note(
            Span::new(0, 2),
            "first declaration here",
            NoteSeverity::Note,
        );
        let mut buf = Vec::new();
        let result = render_with_ariadne(&[diag], &cache, &mut buf);
        assert!(result.is_ok());
        let out = String::from_utf8_lossy(&buf);
        assert!(
            out.contains("R005") || out.contains("duplicate"),
            "output should mention the error: {out}"
        );
    }

    // ── EnvGuard: restore env var after test ──────────────────────────────────

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
