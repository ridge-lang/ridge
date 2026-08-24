//! Path spelling shared by every crate that resolves a path on disk.
//!
//! # Why this is not `std::fs::canonicalize`
//!
//! On Windows `std::fs::canonicalize` answers in the *extended-length* form,
//! with a `\\?\` prefix (`\\?\C:\work\demo`). That form exists to lift the
//! 260-character limit, and it is the right thing to hand to a filesystem
//! call. It is the wrong thing to show a person: it is not the path they
//! typed, not the one in their editor title bar, and several shells and
//! editors reject it outright — the prefix disables path normalisation, so
//! `\\?\C:\work\demo\..\demo` does not resolve.
//!
//! Ridge stores one path per module and per manifest and prints that same
//! value in diagnostics, so the spelling chosen when a path is *resolved* is
//! the spelling a user reads. This module makes that spelling the plain one.
//!
//! # Why the whole workspace goes through here
//!
//! Path containment is a security check: `M017` decides whether a relative
//! path dependency escapes the workspace by asking whether one canonical path
//! starts with another. [`Path::starts_with`] compares components, so a plain
//! path and an extended-length path naming the same directory do **not**
//! match. Half a conversion is therefore worse than none — the check stops
//! agreeing with itself and reports an escape that did not happen.
//!
//! `clippy.toml` bans `std::fs::canonicalize` and `Path::canonicalize` across
//! the workspace so that "remember to convert" is not a rule anyone has to
//! remember. There is one door, and it is [`canonicalize`].

use std::io;
use std::path::{Path, PathBuf};

// ── Extended-length prefixes ──────────────────────────────────────────────────

/// The prefix `std::fs::canonicalize` puts on a Windows drive path.
const VERBATIM: &str = r"\\?\";

/// The same prefix for a UNC share, which spells the share differently again.
const VERBATIM_UNC: &str = r"\\?\UNC\";

/// The plain spelling of an extended-length path, when one exists.
///
/// Returns `None` when the input is not extended-length, and — the case worth
/// stating — when it is but has no plain equivalent. `\\?\Volume{GUID}\` names
/// a volume that may carry no drive letter at all; rewriting it would name a
/// different place, or nothing.
///
/// Kept free of `cfg` so its behaviour is tested on every platform. The shapes
/// are Windows shapes, but the rule that decides between them is not something
/// to leave untested on the two targets where it is easiest to run.
fn plain_spelling(path: &str) -> Option<String> {
    if let Some(share) = path.strip_prefix(VERBATIM_UNC) {
        return Some(format!(r"\\{share}"));
    }

    let rest = path.strip_prefix(VERBATIM)?;
    let mut bytes = rest.bytes();
    let drive = bytes.next()?;
    if drive.is_ascii_alphabetic() && bytes.next() == Some(b':') {
        Some(rest.to_owned())
    } else {
        None
    }
}

/// Rewrite an extended-length Windows path into the form a person writes.
///
/// A no-op everywhere else, and deliberately so: on Unix `\\?\x` is a
/// legitimate — if strange — file name, and rewriting it would name a
/// different file. The `cfg!` is a run-time branch rather than a `#[cfg]` so
/// the rule above stays compiled, and tested, on every target.
#[must_use]
pub fn plain(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        if let Some(spelling) = path.to_str().and_then(plain_spelling) {
            return PathBuf::from(spelling);
        }
    }
    path
}

/// Resolve `path` to an absolute path with no symlinks, `.` or `..`, spelled
/// the way a person writes it.
///
/// This is the workspace's only call to `std::fs::canonicalize`; see the
/// module docs for why the others are banned.
///
/// # Errors
///
/// Propagates the `std::fs::canonicalize` error unchanged — most often
/// `NotFound`, for a path that is not on disk.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    #[allow(
        clippy::disallowed_methods,
        reason = "this function is the one door the ban points every caller at"
    )]
    let canonical = std::fs::canonicalize(path)?;
    Ok(plain(canonical))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── The prefix rule, on every platform ────────────────────────────────────

    #[test]
    fn a_drive_path_loses_the_prefix() {
        assert_eq!(
            plain_spelling(r"\\?\C:\work\demo").as_deref(),
            Some(r"C:\work\demo")
        );
    }

    #[test]
    fn a_unc_path_keeps_both_leading_separators() {
        // `\\?\UNC\srv\share` is `\\srv\share`, not `srv\share`: dropping the
        // whole prefix would turn a share into a relative path.
        assert_eq!(
            plain_spelling(r"\\?\UNC\srv\share\a.ridge").as_deref(),
            Some(r"\\srv\share\a.ridge")
        );
    }

    #[test]
    fn a_volume_guid_has_no_plain_spelling() {
        assert_eq!(
            plain_spelling(r"\\?\Volume{b75e2c83-0000-0000-0000-602f00000000}\x"),
            None,
            "a volume with no drive letter must keep the only spelling it has"
        );
    }

    #[test]
    fn an_ordinary_path_is_left_alone() {
        assert_eq!(plain_spelling(r"C:\work\demo"), None);
        assert_eq!(plain_spelling("/usr/lib/ridge"), None);
        assert_eq!(plain_spelling(""), None);
    }

    #[test]
    fn one_backslash_short_is_not_the_prefix() {
        // `\?\` is an ordinary relative name, and treating it as the prefix
        // would strip characters off a path that never had them. This test
        // exists because the first draft of the experiment that measured this
        // bug lost a backslash on the way to the compiler, and the resulting
        // rejection read exactly like a real finding.
        assert_eq!(plain_spelling(r"\?\C:\work\demo"), None);
        assert_eq!(plain_spelling(r"\\?C:\work\demo"), None);
    }

    // ── The round trip, against a real filesystem ─────────────────────────────

    #[test]
    fn what_canonicalize_answers_still_opens() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("ridge.toml"), "x").unwrap();

        let resolved = canonicalize(&dir.path().join("ridge.toml")).unwrap();

        assert!(resolved.is_absolute(), "canonical paths are absolute");
        assert_eq!(
            std::fs::read_to_string(&resolved).unwrap(),
            "x",
            "the spelling this module chose must still name the file: {}",
            resolved.display()
        );
    }

    #[test]
    #[cfg(windows)]
    fn the_answer_a_person_reads_carries_no_prefix() {
        let dir = tempfile::TempDir::new().unwrap();
        let resolved = canonicalize(dir.path()).unwrap();
        assert!(
            !resolved.to_string_lossy().starts_with(VERBATIM),
            "canonicalize must not answer in extended-length form: {}",
            resolved.display()
        );

        // The control: what is being stripped is really there to strip, so a
        // green above means the strip ran — not that Windows stopped producing
        // the prefix and the assertion has nothing left to catch.
        #[allow(
            clippy::disallowed_methods,
            reason = "the control has to see the unconverted answer"
        )]
        let raw = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            raw.to_string_lossy().starts_with(VERBATIM),
            "control failed: std did not answer in extended-length form, so \
             the assertion above proves nothing about the strip"
        );
    }
}
