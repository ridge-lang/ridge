//! Corpus generators and a temp-workspace harness for the Ridge compile
//! pipeline benchmarks.
//!
//! The benchmarks measure how the pipeline `lex -> parse -> resolve ->
//! typecheck -> lower -> emit Core` scales with input size, so the generators
//! here emit *valid* Ridge of a controlled shape and size:
//!
//! - [`many_functions`] — a chain of nullary functions, each calling the
//!   previous one. Stresses symbol resolution and per-function typechecking.
//! - [`deep_let_chain`] — one function with a long run of sequential `let`
//!   bindings. Stresses scope resolution and the let-lowering.
//! - [`wide_record`] — a record type with many fields plus a constructor.
//!   Stresses row unification in the type checker.
//!
//! [`BenchWorkspace`] writes a single-member workspace to a temp directory so
//! the driver entry points ([`run_check`], [`run_emit_core`]) can run the real
//! pipeline against it. The shapes are deliberately additive in one dimension
//! so a regression that turns an `O(n)` pass into `O(n^2)` shows up as a bend
//! in the size/time curve rather than a flat shift.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ridge_driver::{
    check_workspace, compile_workspace, CheckOptions, CompileOptions, EmitArtefacts,
};

pub mod tracking;

// ── Corpus generators ─────────────────────────────────────────────────────────

/// Generate a module with `n` exported nullary functions, each calling the
/// previous one (`f{i} = f{i-1} () + 1`).
///
/// Every function is `pub` so the resolver does not flag them as unused; the
/// call chain forces real cross-function name resolution and typechecking.
#[must_use]
pub fn many_functions(n: usize) -> String {
    let n = n.max(1);
    let mut s = String::new();
    let _ = writeln!(s, "pub fn f0 () -> Int = 0");
    for i in 1..n {
        let _ = writeln!(s, "pub fn f{i} () -> Int = f{prev} () + 1", prev = i - 1);
    }
    s
}

/// Generate one function whose body is a run of `n` sequential `let` bindings,
/// each referring to the previous binding, returning the last.
///
/// Ridge `let` is indentation-based with no `in` keyword, so the bindings and
/// the trailing result expression all sit at one indentation level.
#[must_use]
pub fn deep_let_chain(n: usize) -> String {
    let mut s = String::from("pub fn deep () -> Int =\n");
    let _ = writeln!(s, "    let x0 = 0");
    for i in 1..=n {
        let _ = writeln!(s, "    let x{i} = x{prev}", prev = i - 1);
    }
    let _ = writeln!(s, "    x{n}");
    s
}

/// Generate a record type with `n` `Int` fields plus a `pub` constructor that
/// fills every field, exercising row unification in the type checker.
#[must_use]
pub fn wide_record(n: usize) -> String {
    let n = n.max(1);
    let mut s = String::from("type Wide = {\n");
    for i in 0..n {
        let sep = if i + 1 < n { "," } else { "" };
        let _ = writeln!(s, "    f{i}: Int{sep}");
    }
    // The record *type* parses across lines, but a record *literal* must sit on
    // one line, so the constructor is emitted inline.
    s.push_str("}\n\npub fn mk () -> Wide = Wide { ");
    for i in 0..n {
        if i > 0 {
            s.push_str(", ");
        }
        let _ = write!(s, "f{i} = 0");
    }
    s.push_str(" }\n");
    s
}

// ── Temp-workspace harness ────────────────────────────────────────────────────

/// A single-member Ridge workspace written to a temp directory, removed on drop.
///
/// The entry module (`app/src/Main.ridge`) holds the generated source. The
/// manifest uses the same schema the driver integration tests rely on (a named,
/// versioned workspace with one app member).
pub struct BenchWorkspace {
    dir: tempfile::TempDir,
}

impl BenchWorkspace {
    /// Write a workspace whose entry module contains `source`.
    ///
    /// # Errors
    ///
    /// Returns any filesystem error encountered while creating the directories
    /// or writing the manifests and source file.
    pub fn new(source: &str) -> std::io::Result<Self> {
        let dir = tempfile::Builder::new().prefix("ridge-bench-").tempdir()?;
        let root = dir.path();
        let app_src = root.join("app").join("src");
        std::fs::create_dir_all(&app_src)?;
        std::fs::write(
            root.join("ridge.toml"),
            "[workspace]\nname = \"bench-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
        )?;
        std::fs::write(
            root.join("app").join("ridge.toml"),
            "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
        )?;
        std::fs::write(app_src.join("Main.ridge"), source)?;
        Ok(Self { dir })
    }

    /// Absolute path to the workspace root.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }
}

// ── Pipeline entry points (measured by the benches) ───────────────────────────

/// Run the front-and-middle pipeline (`discover -> resolve -> typecheck`).
///
/// Returns the number of diagnostics produced — `0` on a clean check, and
/// [`usize::MAX`] if the driver hit a fatal error (e.g. a missing manifest).
/// The benches assert `0` before timing so a generator regression cannot turn
/// the benchmark into a measurement of the error path.
#[must_use]
pub fn run_check(root: &Path) -> usize {
    check_workspace(CheckOptions::new(root.to_path_buf()))
        .map(|a| a.diagnostics.len())
        .unwrap_or(usize::MAX)
}

/// Run the full pipeline through Core Erlang emission (no `erlc`, no BEAM).
///
/// `lower` + `codegen` run, but the `.core` text is written without compiling
/// to BEAM. `cache_root` redirects the package cache to a temp dir so the bench
/// never touches the developer's global Ridge cache. Returns the number of
/// `.core` files written, or [`usize::MAX`] on a fatal driver error.
#[must_use]
pub fn run_emit_core(root: &Path, cache_root: &Path) -> usize {
    compile_workspace(
        CompileOptions::new(root.to_path_buf())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache_root.to_path_buf()),
    )
    .map(|a| a.core_files.len())
    .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Every generator must emit source that checks cleanly: a benchmark of an
    /// error path measures nothing useful.
    #[test]
    fn generators_check_cleanly() {
        for (name, src) in [
            ("many_functions", many_functions(8)),
            ("deep_let_chain", deep_let_chain(8)),
            ("wide_record", wide_record(8)),
        ] {
            let ws = BenchWorkspace::new(&src).expect("write workspace");
            assert_eq!(
                run_check(&ws.root()),
                0,
                "{name} must produce a workspace that checks with no diagnostics"
            );
        }
    }
}
