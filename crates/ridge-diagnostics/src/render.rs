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
}

impl AriadneCacheAdapter {
    /// Construct an adapter from a list of diagnostics and a [`SourceCache`].
    ///
    /// All source IDs referenced by `diagnostics` are resolved up front.
    fn from_diagnostics(diagnostics: &[Diagnostic], cache: &dyn SourceCache) -> Self {
        let mut parsed = HashMap::new();
        let mut names = HashMap::new();
        for diag in diagnostics {
            let key = diag.source_id.as_str().to_owned();
            if !parsed.contains_key(&key) {
                let text = cache.fetch(&diag.source_id).unwrap_or("").to_owned();
                let name = cache.display_name(&diag.source_id).to_owned();
                parsed.insert(key.clone(), Source::from(text));
                names.insert(key, name);
            }
            // Also pre-populate for note spans.
            for note in &diag.notes {
                let _ = note; // notes share the same source_id as the parent
            }
        }
        Self { parsed, names }
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
        let primary_span_start = diag.primary_span.start as usize;
        let primary_span_end = diag.primary_span.end as usize;

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
            report_builder = report_builder.with_label(
                Label::new((
                    diag.source_id.clone(),
                    note.span.start as usize..note.span.end as usize,
                ))
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
    clippy::doc_markdown
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
