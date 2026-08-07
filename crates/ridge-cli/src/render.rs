//! Diagnostic rendering helpers for the CLI.
//!
//! Calls [`ridge_diagnostics::render_with_ariadne`] to emit structured,
//! colourised, source-span-aware diagnostics to stderr.

use std::io::Write;

use ridge_diagnostics::{lookup_code, render_with_ariadne, Diagnostic, Severity, SourceCache};

/// Render a slice of structured diagnostics to stderr.
///
/// Returns the number of error-severity diagnostics rendered.  Errors from the
/// renderer itself are silently swallowed — in the worst case the user sees
/// nothing, which is preferable to a renderer crash masking the original error.
pub fn render_diagnostics(diagnostics: &[Diagnostic], cache: &dyn SourceCache) -> usize {
    let mut stderr = std::io::stderr();
    let count = render_with_ariadne(diagnostics, cache, &mut stderr).unwrap_or(0);
    write_explain_hint(diagnostics, &mut stderr);
    count
}

/// Point at `ridge explain`, once per batch, naming a code that resolves.
///
/// Once per batch rather than once per diagnostic: the invitation is worth one
/// line at the end of a wall of errors and nothing at all repeated under each
/// one. The code it names is the first in the batch the registry actually
/// knows, so the command it suggests is one that answers — a hint that sends
/// someone to a command that fails is worse than no hint.
fn write_explain_hint(diagnostics: &[Diagnostic], w: &mut dyn Write) {
    let Some(code) = diagnostics
        .iter()
        .map(|d| d.code)
        .find(|c| lookup_code(c).is_some())
    else {
        return;
    };
    // Same reasoning as the render above: a failed write here must not turn
    // into a second error on top of the ones being reported.
    let _ = writeln!(w, "\nFor more about a code, run `ridge explain {code}`.");
}

/// Whether warnings should stop the command.
///
/// Flattened into the three commands that act as build gates, so they share one
/// spelling and one help line.  `run`, `reload`, `migrate` and `repl` print
/// warnings and carry on regardless, and so do not take the flag: refusing to
/// start a program, or to evaluate a REPL line, over an advisory diagnostic is
/// not a gate anyone asked for.
#[derive(Debug, clap::Args)]
pub struct WarningPolicy {
    /// Fail on warnings as well as errors.
    #[arg(long = "deny-warnings")]
    pub deny_warnings: bool,
}

/// What one batch of diagnostics means for the command that produced it.
#[derive(Debug, Clone, Copy)]
pub struct Report {
    /// Error-severity diagnostics. These always stop the command.
    pub errors: usize,
    /// Warning-severity diagnostics. Advisory unless the caller denied them.
    pub warnings: usize,
    /// Whether the caller asked for warnings to be fatal.
    deny_warnings: bool,
}

impl Report {
    /// Whether the command must stop here.
    #[must_use]
    pub const fn fatal(&self) -> bool {
        self.errors > 0 || (self.deny_warnings && self.warnings > 0)
    }

    /// A clause to append to a success line: empty, or ` with 2 warnings`.
    #[must_use]
    pub fn warning_suffix(&self) -> String {
        match self.warnings {
            0 => String::new(),
            1 => " with 1 warning".to_owned(),
            n => format!(" with {n} warnings"),
        }
    }
}

/// Render diagnostics to stderr, then say what they mean for the caller.
///
/// The severity counts are taken from the diagnostics themselves rather than
/// from [`render_diagnostics`]'s return value, which reports zero errors when
/// the terminal write fails. An exit code must not depend on whether the
/// message reached the screen.
///
/// Only `Error` counts as fatal and only `Warning` counts as a warning:
/// `Severity` is `#[non_exhaustive]`, and the informational levels it
/// anticipates should gate nothing.
pub fn report_diagnostics(
    diagnostics: &[Diagnostic],
    cache: &dyn SourceCache,
    deny_warnings: bool,
) -> Report {
    render_diagnostics(diagnostics, cache);
    Report {
        errors: count_severity(diagnostics, Severity::Error),
        warnings: count_severity(diagnostics, Severity::Warning),
        deny_warnings,
    }
}

/// Render and gate on the error-severity diagnostics only, ignoring warnings.
///
/// For the second pass of a command that walks the pipeline twice — `test`
/// type-checks, then compiles. Both passes carry the same warnings, so
/// rendering the whole batch again would print each one twice. Codegen adds
/// errors, never warnings, so nothing is lost by dropping the second copy.
pub fn report_errors_only(diagnostics: &[Diagnostic], cache: &dyn SourceCache) -> Report {
    let errors: Vec<Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .cloned()
        .collect();
    report_diagnostics(&errors, cache, false)
}

/// Count the diagnostics carrying exactly `wanted`.
fn count_severity(diagnostics: &[Diagnostic], wanted: Severity) -> usize {
    diagnostics.iter().filter(|d| d.severity == wanted).count()
}
