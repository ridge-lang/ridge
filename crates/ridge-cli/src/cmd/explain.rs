//! `ridge explain` — say what a diagnostic code means.
//!
//! ## Surface
//!
//! ```text
//! ridge explain <CODE>
//! ridge explain --list
//! ```
//!
//! The code a diagnostic prints is the one handle a user has on an error, and
//! until now it led nowhere: the meaning lived in the source of whichever crate
//! declared it. The registry in `ridge-diagnostics` holds a sentence for every
//! code the compiler can emit, and this is the command that reads it.
//!
//! Every code answers, including the ones the compiler stopped emitting. A
//! retired code is answered from the retired table, with the code that arrives
//! in its place — the number in an old log is the only handle its reader has,
//! and that is the moment a dead end costs the most. `rustc --explain` is the
//! shape this follows, minus its one sharp edge: most `rustc` codes have no
//! extended text, so the command that promises an explanation often has none to
//! give. A code with no entry in either table cannot exist here — the registry
//! census fails the build first.

use std::io::{self, Write};
use std::path::Path;

use clap::Parser;
use ridge_diagnostics::{lookup_code, lookup_retired, CodeEntry, RetiredCode, REGISTRY};

use crate::error::CliError;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Say what a diagnostic code means.
#[derive(Debug, Parser)]
pub struct ExplainArgs {
    /// The code to explain, e.g. `T031`.
    ///
    /// Case does not matter, and the brackets a rendered diagnostic puts around
    /// the code are accepted — `[t031]` is what a copy-paste off the terminal
    /// actually produces.
    #[arg(value_name = "CODE", required_unless_present = "list")]
    pub code: Option<String>,

    /// List every code with its summary instead of explaining one.
    #[arg(long, conflicts_with = "code")]
    pub list: bool,
}

// ── Normalisation ─────────────────────────────────────────────────────────────

/// Turn what the user typed into the spelling the registry is keyed by.
///
/// The registry's lookup is deliberately exact, which leaves the shape of the
/// input to whoever collected it. What a terminal hands over is `[T031]`, and
/// what a person types from memory is `t031`; both mean the same code, and
/// neither should be a miss.
fn normalise(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
        .to_uppercase()
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Write one entry as the command's whole output.
fn write_entry(w: &mut dyn Write, entry: &CodeEntry) -> io::Result<()> {
    writeln!(w, "{} — reported by {}", entry.code, entry.owner)?;
    writeln!(w)?;
    writeln!(w, "{}", entry.summary)
}

/// Write one retired code as the command's whole output.
///
/// Opens by saying the compiler no longer reports it, because that is the fact
/// that resolves what the reader is looking at — a number in a log that their
/// current build will never produce. The pointer comes last, and only when
/// there is one: a code whose failure simply stopped being a failure has
/// nothing to send anyone to, and inventing a destination would be worse than
/// the silence.
fn write_retired(w: &mut dyn Write, retired: &RetiredCode) -> io::Result<()> {
    writeln!(w, "{} — retired, no longer reported", retired.code)?;
    writeln!(w)?;
    writeln!(w, "{}", retired.summary)?;
    match retired.see {
        Some(see) => {
            writeln!(w)?;
            writeln!(
                w,
                "You are probably looking for {see}. Run `ridge explain {see}`."
            )
        }
        None => Ok(()),
    }
}

/// Write every code and its summary, one per line, grouped by leading letter.
///
/// The blank line between groups is the only structure here: the letter is not
/// a namespace — `P` covers the parser and the package layer, `T` the type
/// checker and the standard library — so a heading naming a compiler phase per
/// letter would be wrong. The grouping is a reading aid, nothing more.
fn write_list(w: &mut dyn Write) -> io::Result<()> {
    let mut group = None;
    for entry in REGISTRY {
        let letter = entry.code.chars().next();
        if group.is_some() && group != letter {
            writeln!(w)?;
        }
        group = letter;
        writeln!(w, "{}  {}", entry.code, entry.summary)?;
    }
    Ok(())
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Execute `ridge explain`.
///
/// Takes no workspace: a code means the same thing everywhere, and asking what
/// `T031` is should work in a directory that has no project in it.
///
/// # Errors
///
/// Returns [`CliError::ExplainUnknownCode`] when the argument is not a code the
/// compiler can emit.
pub fn execute(args: &ExplainArgs, _cwd: &Path) -> Result<(), CliError> {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if args.list {
        // A closed pipe (`ridge explain --list | head`) is how this command is
        // meant to be used, and it is not a failure of the command.
        let _ = write_list(&mut out);
        return Ok(());
    }

    // `required_unless_present = "list"` means clap has already refused an
    // invocation with neither; treating the gap as an empty code keeps that
    // contract in one place instead of restating it as a second error.
    let code = normalise(args.code.as_deref().unwrap_or_default());

    if let Some(entry) = lookup_code(&code) {
        let _ = write_entry(&mut out, entry);
        return Ok(());
    }

    // A retired code is the case this command exists for. Someone reading a log
    // from an older compiler, or a CI filter nobody has revisited, types a
    // number the compiler no longer emits — and the number is the only handle
    // they have. Answering "unknown code" there would be the dead end the
    // registry was built to remove, at the one moment it costs the most.
    if let Some(retired) = lookup_retired(&code) {
        let _ = write_retired(&mut out, retired);
        return Ok(());
    }

    Err(CliError::ExplainUnknownCode { code })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ridge_diagnostics::RETIRED;

    use super::*;

    #[test]
    fn a_code_off_the_terminal_resolves() {
        // The three spellings of one code: typed, pasted, and shouted.
        for spelling in ["c001", "[C001]", " C001 "] {
            assert_eq!(normalise(spelling), "C001", "from {spelling}");
        }
    }

    #[test]
    fn an_entry_names_the_code_the_crate_and_the_meaning() {
        // A real entry, not a synthetic one: `CodeEntry` is `#[non_exhaustive]`
        // and cannot be built from outside its crate, which is the same reason
        // the rest of the workspace has to go through the registry to read one.
        let found = lookup_code("C001");
        assert!(found.is_some(), "C001 is in the registry");
        let Some(entry) = found else { return };

        let mut out = Vec::new();
        write_entry(&mut out, entry).unwrap_or_default();
        let text = String::from_utf8(out).unwrap_or_default();

        assert!(
            text.starts_with(&format!("C001 — reported by {}\n", entry.owner)),
            "{text}"
        );
        assert!(text.contains(entry.summary), "{text}");
    }

    /// Every code in the registry is reachable through the command.
    ///
    /// The registry census proves the table matches source; this proves the
    /// command reads the whole table rather than some prefix of it.
    #[test]
    fn every_registered_code_can_be_explained() {
        for entry in REGISTRY {
            assert!(
                lookup_code(&normalise(entry.code)).is_some(),
                "{} did not resolve",
                entry.code
            );
        }
    }

    /// A retired code answers, and sends the reader on.
    ///
    /// The number in a two-year-old log is the only handle its reader has. This
    /// is the case the retired table exists for, so it is pinned by output and
    /// not just by the lookup returning something.
    #[test]
    fn a_retired_code_says_so_and_names_its_replacement() {
        let found = lookup_retired("C402");
        assert!(found.is_some(), "C402 is in the retired table");
        let Some(retired) = found else { return };

        let mut out = Vec::new();
        write_retired(&mut out, retired).unwrap_or_default();
        let text = String::from_utf8(out).unwrap_or_default();

        assert!(
            text.starts_with("C402 — retired, no longer reported\n"),
            "{text}"
        );
        assert!(text.contains("ridge explain C004"), "{text}");
    }

    /// Every retired code is reachable through the command, the same as a live
    /// one — the whole table, not some prefix of it.
    #[test]
    fn every_retired_code_can_be_explained() {
        for retired in RETIRED {
            assert!(
                lookup_retired(&normalise(retired.code)).is_some(),
                "{} did not resolve",
                retired.code
            );
        }
    }

    /// A code is live or retired, never both.
    ///
    /// `execute` tries the registry first, so a code in both tables would
    /// answer as live and its retirement would be unreachable — the failure
    /// would be silence, which is the hardest kind to notice.
    #[test]
    fn no_code_is_both_live_and_retired() {
        for retired in RETIRED {
            assert!(
                lookup_code(retired.code).is_none(),
                "{} is in both tables",
                retired.code
            );
        }
    }

    #[test]
    fn the_list_covers_every_code() {
        let mut out = Vec::new();
        write_list(&mut out).unwrap_or_default();
        let text = String::from_utf8(out).unwrap_or_default();

        for entry in REGISTRY {
            assert!(text.contains(entry.code), "{} is missing", entry.code);
        }
    }
}
