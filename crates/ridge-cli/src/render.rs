//! Diagnostic rendering helpers for the CLI.
//!
//! Calls [`ridge_diagnostics::render_with_ariadne`] to emit structured,
//! colourised, source-span-aware diagnostics to stderr.

use ridge_diagnostics::{render_with_ariadne, Diagnostic, SourceCache};

/// Render a slice of structured diagnostics to stderr.
///
/// Returns the number of error-severity diagnostics rendered.  Errors from the
/// renderer itself are silently swallowed — in the worst case the user sees
/// nothing, which is preferable to a renderer crash masking the original error.
pub fn render_diagnostics(diagnostics: &[Diagnostic], cache: &dyn SourceCache) -> usize {
    let mut stderr = std::io::stderr();
    render_with_ariadne(diagnostics, cache, &mut stderr).unwrap_or(0)
}
