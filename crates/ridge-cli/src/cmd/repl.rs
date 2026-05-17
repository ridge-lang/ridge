//! `ridge repl` — interactive read-eval-print loop over stdin.
//!
//! ## Surface
//!
//! ```text
//! ridge repl
//! ```
//!
//! Reads stdin line by line with bracket-counting auto-continuation: if a line
//! ends with unbalanced `(`, `[`, or `{`, the REPL reads more lines until
//! brackets balance.  A trailing `\` forces continuation regardless of bracket
//! state.
//!
//! ## Session model (D162)
//!
//! Each session maintains an accumulating synthetic Ridge module.  User input
//! is classified:
//!
//! - `let <name> = <expr>` → accumulated as a zero-argument top-level function
//!   (`pub fn <name> -> ? = <expr>`).  Subsequent expressions may reference
//!   `<name>` as if it were a constant.
//! - Any other expression `<expr>` → wrapped as
//!   `pub fn _repl_<n> -> ? = <expr>`, compiled and run; result printed.
//! - `:q` → clean exit (code 0).
//! - `:help` → print help text.
//!
//! ## Capabilities (D150, D162)
//!
//! The REPL session declares
//! `allow = ["io", "fs", "net", "time", "random", "env", "proc", "spawn"]`
//! (8 of 9 capabilities; `ffi` excluded) so users can experiment freely.
//! This is a privileged context — code run in the REPL may perform I/O,
//! access the filesystem, spawn processes, etc.
//!
//! ## Bracket-counting scanner (D162)
//!
//! The scanner reads characters sequentially, tracking:
//! - Whether it is inside a double-quoted string literal (`"…"`).
//! - Whether it has seen a line-comment start (`--` outside a string), which
//!   causes the rest of the line to be ignored for bracket counting.
//! - Bracket depth: `(`, `[`, `{` increment; `)`, `]`, `}` decrement.
//!
//! Brackets inside string literals or line comments do NOT affect the depth
//! counter, preventing spurious continuation.
//!
//! ## REPL runner
//!
//! The REPL compiles each `_repl_<n>` expression into a temporary workspace,
//! then invokes an embedded Erlang runner (`ridge_repl_runner.erl`) to call
//! `Module:'_repl_<n>'()` and print the result in a human-readable form.

// REPL-local stylistic allows.  These don't reflect bugs — the code is
// structured for legibility, not for maximum micro-optimisation:
// - `format_push_string`: appended `format!(...)` builds the synthetic Ridge
//   source string clearly; rewriting to `write!` for nano-allocation savings
//   does not pay back in code clarity.
// - `single_char_add_str`: `push_str("\n")` reads parallel to the surrounding
//   `push_str("import ...\n")` calls.
// - `result_large_err`: `CompileError::Diagnostics(Vec<Diagnostic>, …)` is
//   intentionally large; boxing every error path here would not help.
// - `significant_drop_tightening`: REPL is single-threaded; lock-tightening
//   suggestions are noise here.
#![allow(
    clippy::format_push_string,
    clippy::single_char_add_str,
    clippy::result_large_err,
    clippy::significant_drop_tightening
)]

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use clap::Parser;
use ridge_diagnostics::Diagnostic;
use ridge_driver::{compile_workspace, CompileOptions, WorkspaceSourceCache};

use crate::error::CliError;
use crate::render::render_diagnostics;

// ── Embedded REPL runner ──────────────────────────────────────────────────────

/// Erlang-side REPL result printer.
///
/// Written to `<session_tmpdir>/runtime/` and compiled to `.beam` on session
/// start so that `erl -pa <beam_dir> -s ridge_repl_runner run <Mod> <Fn>`
/// can pretty-print the result of each REPL expression.
///
/// Handles:
/// - `ok`          → Unit result from side-effectful fn; no output.
/// - `{ok, _}`     → Result Ok branch; no output.
/// - `{error, B}`  → prints "error: <Msg>" to stderr, exits 1.
/// - `true`/`false`→ prints "true"/"false".
/// - Integer       → decimal.
/// - Float         → `~g` format.
/// - Binary        → UTF-8 text (`~ts`).
/// - Anything else → Erlang `~p` representation.
/// - Exception     → stderr + exit 1.
const REPL_RUNNER_SOURCE: &str = r#"-module(ridge_repl_runner).
-export([run/1]).

run([ModAtom, FnAtom]) ->
    try ModAtom:FnAtom() of
        ok ->
            erlang:halt(0);
        {ok, _} ->
            erlang:halt(0);
        {error, Msg} when is_binary(Msg) ->
            io:format(standard_error, "error: ~ts~n", [Msg]),
            erlang:halt(1);
        true ->
            io:put_chars(standard_io, <<"true\n">>),
            erlang:halt(0);
        false ->
            io:put_chars(standard_io, <<"false\n">>),
            erlang:halt(0);
        V when is_integer(V) ->
            io:format("~B~n", [V]),
            erlang:halt(0);
        V when is_float(V) ->
            io:format("~g~n", [V]),
            erlang:halt(0);
        V when is_binary(V) ->
            io:format("~ts~n", [V]),
            erlang:halt(0);
        V ->
            io:format("~p~n", [V]),
            erlang:halt(0)
    catch
        Class:Reason:Stack ->
            io:format(standard_error, "error: ~p:~p~nstack:~p~n",
                      [Class, Reason, Stack]),
            erlang:halt(1)
    end;
run(Other) ->
    io:format(standard_error, "error: bad runner args ~p~n", [Other]),
    erlang:halt(2).
"#;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Start an interactive REPL session.
///
/// Reads expressions from stdin, evaluates each one in a per-session Ridge
/// module, and prints the result.  Type `:q` to quit.
///
/// The REPL session allows the following capabilities:
/// `io`, `fs`, `net`, `time`, `random`, `env`, `proc`, `spawn`
/// (`ffi` is intentionally excluded).
#[derive(Debug, Parser)]
pub struct ReplArgs {
    // No flags for 0.1.0 — stdin-only mode per D150.
}

// ── Session state ─────────────────────────────────────────────────────────────

/// RAII wrapper for a temporary directory.
///
/// Creates a unique directory under the OS temp dir on construction; removes
/// the directory tree on drop.  Used instead of the `tempfile` crate (which
/// is a dev-dependency only) so this production code compiles without
/// requiring a new `[dependencies]` entry.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a new unique temporary directory.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the directory cannot be created.
    fn new() -> Result<Self, io::Error> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let base = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let pid = std::process::id();
        let dir_name = format!("ridge_repl_{pid}_{nanos}");
        let path = base.join(dir_name);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// The path to the temporary directory.
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort removal — ignore errors on drop.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Per-session state accumulated between REPL iterations.
struct ReplSession {
    /// Accumulated `let` bindings from the session, in input order.
    ///
    /// Each entry is `(<name>, <value_expr>)` — e.g. `("x", "5")`.  On each
    /// evaluation, all accumulated bindings are injected as local `let`
    /// bindings inside the wrapper function body, so later expressions can
    /// reference earlier names.
    ///
    /// D162: accumulation strategy — local-binding injection (not top-level
    /// decls) because Ridge's typechecker does not auto-call zero-arg fns,
    /// which makes the top-level-decl approach produce `T001` type errors
    /// when a binding name appears in arithmetic (e.g. `x + 1` where
    /// `x : fn -> Int`).
    accumulated_lets: Vec<(String, String)>,
    /// Counter incremented for each evaluated expression.
    ///
    /// Produces the unique BEAM function name `_repl_<n>`.
    expr_counter: u32,
    /// Temporary directory owning the session workspace.
    ///
    /// Kept alive for the session duration.  Dropped (and deleted) on exit.
    _session_dir: TempDir,
    /// Absolute path to the session workspace root.
    workspace_root: PathBuf,
    /// Absolute path to the per-session beam output directory.
    beam_dir: PathBuf,
    /// Absolute path to the session runtime directory (holds runner `.beam`).
    runtime_dir: PathBuf,
}

impl ReplSession {
    /// Initialise a new REPL session in a fresh temporary directory.
    ///
    /// Returns `Err` if the temp directory cannot be created or the REPL runner
    /// Erlang source cannot be compiled.
    fn new(erl_path: &Path, erlc_path: &Path) -> Result<Self, String> {
        let td =
            TempDir::new().map_err(|e| format!("failed to create REPL session directory: {e}"))?;
        let ws = td.path().to_owned();

        // Create workspace layout.
        let member_dir = ws.join("apps").join("repl_session");
        let src_dir = member_dir.join("src");
        let beam_dir = ws.join("target").join("ridge").join("debug").join("beam");
        let runtime_dir = ws
            .join("target")
            .join("ridge")
            .join("debug")
            .join("runtime");

        for dir in [&src_dir, &beam_dir, &runtime_dir] {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("failed to create directory {}: {e}", dir.display()))?;
        }

        // Write workspace manifest.
        std::fs::write(
            ws.join("ridge.toml"),
            "[workspace]\nname = \"repl-session\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
        )
        .map_err(|e| format!("failed to write workspace manifest: {e}"))?;

        // Write project manifest with full capability allowlist (D150, D162).
        // The [capabilities] table is a top-level section in the project TOML,
        // not nested under [project] — see ridge-manifest src/project.rs.
        std::fs::write(
            member_dir.join("ridge.toml"),
            "[project]\n\
             name = \"repl_session\"\n\
             version = \"0.1.0\"\n\
             kind = \"library\"\n\
             \n\
             [capabilities]\n\
             allow = [\"io\", \"fs\", \"net\", \"time\", \"random\", \"env\", \"proc\", \"spawn\"]\n",
        )
        .map_err(|e| format!("failed to write project manifest: {e}"))?;

        // Write the REPL runner Erlang source and compile it to a .beam.
        let runner_erl = runtime_dir.join("ridge_repl_runner.erl");
        std::fs::write(&runner_erl, REPL_RUNNER_SOURCE)
            .map_err(|e| format!("failed to write REPL runner source: {e}"))?;

        let erlc_out = process::Command::new(erlc_path)
            .arg("-o")
            .arg(&beam_dir)
            .arg(&runner_erl)
            .output()
            .map_err(|e| format!("failed to spawn erlc: {e}"))?;

        if !erlc_out.status.success() {
            let stderr = String::from_utf8_lossy(&erlc_out.stderr);
            return Err(format!("erlc failed to compile REPL runner: {stderr}"));
        }

        // erl_path is not used during session init (only erlc_path is needed).
        // Suppress the lint — the parameter is part of the public API for
        // potential future use (e.g. warmup check).
        let _ = erl_path;

        Ok(Self {
            accumulated_lets: Vec::new(),
            expr_counter: 0,
            _session_dir: td,
            workspace_root: ws,
            beam_dir,
            runtime_dir,
        })
    }

    /// Generate the current synthetic Ridge module source.
    ///
    /// The module structure:
    /// 1. Pre-imported stdlib aliases for the 8 allowed capabilities.
    /// 2. A single evaluation function `pub fn <fn_name> = …` whose body
    ///    starts with all accumulated `let` bindings (in session order) and
    ///    ends with the current expression.
    ///
    /// D162 accumulation strategy: accumulated `let x = <v>` bindings are
    /// injected as local `let` bindings inside the wrapper function body.
    /// This avoids the T001 type mismatch that occurs when a zero-arg
    /// top-level function (`pub fn x -> Int = 5`) is referenced by name
    /// in an arithmetic context (`x + 1`), because the typechecker treats
    /// the reference as `fn -> Int`, not `Int`.
    ///
    /// Pre-imports let users write `Io.println "hi"` without an explicit
    /// `import` statement, matching the "privileged REPL context" documented
    /// in §3.8 / D150.
    fn build_module_source(&self, current_expr: &str, fn_name: &str) -> String {
        let mut src = String::new();

        // Standard library pre-imports for the 8 allowed capabilities.
        // These match the `allow = [...]` list in the project manifest (D150).
        src.push_str("import std.io as Io\n");
        src.push_str("import std.fs as Fs\n");
        src.push_str("import std.env as Env\n");
        src.push_str("import std.time as Time\n");
        src.push_str("import std.random as Random\n");
        src.push_str("import std.list as List\n");
        src.push_str("import std.map as Map\n");
        src.push_str("import std.option as Option\n");
        src.push_str("import std.text as Text\n");
        src.push_str("import std.int as Int\n");
        src.push_str("\n");

        // Build the wrapper function.
        // If there are no accumulated bindings, emit a simple one-liner.
        // If there are accumulated bindings, emit a multi-line body with
        // local `let` bindings preceding the final expression.
        if self.accumulated_lets.is_empty() {
            src.push_str(&format!("pub fn {fn_name} = {current_expr}\n"));
        } else {
            src.push_str(&format!("pub fn {fn_name} =\n"));
            for (name, value) in &self.accumulated_lets {
                src.push_str(&format!("    let {name} = {value}\n"));
            }
            src.push_str(&format!("    {current_expr}\n"));
        }

        src
    }

    /// Write the synthetic Ridge source to the session workspace and compile it.
    ///
    /// Returns the BEAM module name (file stem) of the compiled function module.
    fn compile_expr(&self, src: &str) -> Result<CompileResult, CompileError> {
        // Write the source file.
        let src_path = self
            .workspace_root
            .join("apps")
            .join("repl_session")
            .join("src")
            .join("ReplSession.rg");

        std::fs::write(&src_path, src)
            .map_err(|e| CompileError::Io(format!("write source: {e}")))?;

        // Compile via driver.
        let opts = CompileOptions::new(self.workspace_root.clone());
        match compile_workspace(opts) {
            Ok(artefacts) => {
                if !artefacts.diagnostics.is_empty() {
                    return Err(CompileError::Diagnostics(
                        artefacts.diagnostics,
                        artefacts.sources,
                    ));
                }
                // Derive BEAM module name from the first .beam file produced.
                let beam_module = artefacts
                    .beam_files
                    .first()
                    .and_then(|p| p.file_stem())
                    .and_then(|s| s.to_str())
                    .unwrap_or("ridge_module_0")
                    .to_owned();
                Ok(CompileResult { beam_module })
            }
            Err(e) => Err(CompileError::Driver(e.to_string())),
        }
    }
}

// ── Compile helpers ───────────────────────────────────────────────────────────

/// Result of a successful compilation step.
struct CompileResult {
    /// BEAM module name (file stem, e.g. `ridge_module_0`).
    beam_module: String,
}

/// Error during REPL compilation.
enum CompileError {
    /// Driver-level fatal error (workspace root not found, etc.).
    Driver(String),
    /// I/O error writing source or reading artefacts.
    Io(String),
    /// Compile diagnostics (type errors, parse errors, etc.).
    Diagnostics(Vec<Diagnostic>, WorkspaceSourceCache),
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge repl`.
///
/// Reads input from stdin, evaluates each expression in a per-session
/// synthetic Ridge module, and prints the result.  Continues until `:q` is
/// received or stdin closes.
///
/// # Errors
///
/// Returns a [`CliError`] if `erl` / `erlc` is not found, or if the session
/// directory cannot be initialised.
pub fn execute(_args: &ReplArgs, _cwd: &Path) -> Result<(), CliError> {
    // ── Locate erl and erlc ───────────────────────────────────────────────────
    let erl_path = which::which("erl").map_err(|_| {
        eprintln!("error: C004 ErlangNotFound: erl not found on PATH (install OTP 26+)");
        CliError::NoWorkspaceRoot
    })?;

    let erlc_path = which::which("erlc").map_err(|_| {
        eprintln!("error: C004 ErlangNotFound: erlc not found on PATH (install OTP 26+)");
        CliError::NoWorkspaceRoot
    })?;

    // ── Initialise session ────────────────────────────────────────────────────
    let mut session = ReplSession::new(&erl_path, &erlc_path).map_err(|e| {
        eprintln!("error: failed to initialise REPL session: {e}");
        CliError::NoWorkspaceRoot
    })?;

    // ── Print welcome header ──────────────────────────────────────────────────
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "Ridge REPL  (:q to quit, :help for help)");
    let _ = out.flush();

    let stdin = io::stdin();

    // ── Main REPL loop ────────────────────────────────────────────────────────
    loop {
        // Read one complete input (may span multiple lines via bracket-counting).
        let input = match read_input(&stdin) {
            Ok(Some(s)) => s,
            Ok(None) => break, // stdin closed
            Err(e) => {
                eprintln!("error: I/O error reading input: {e}");
                process::exit(1);
            }
        };

        let trimmed = input.trim();

        if trimmed.is_empty() {
            continue;
        }

        // ── Built-in commands ─────────────────────────────────────────────────
        if trimmed == ":q" {
            break;
        }

        if trimmed == ":help" {
            print_help();
            continue;
        }

        // ── Let-binding accumulation (D162) ───────────────────────────────────
        if let Some(binding) = try_parse_let_binding(trimmed) {
            // Accumulate as a local binding for injection into subsequent
            // expression functions (D162 local-binding injection strategy).
            session.accumulated_lets.push((binding.name, binding.value));
            // Let bindings are not evaluated independently — they become
            // available to subsequent expressions in the session.
            continue;
        }

        // ── Expression evaluation ─────────────────────────────────────────────
        session.expr_counter += 1;
        let fn_name = format!("_repl_{}", session.expr_counter);

        let module_src = session.build_module_source(trimmed, &fn_name);

        match session.compile_expr(&module_src) {
            Ok(result) => {
                // Run the compiled expression via the REPL runner.
                run_repl_expr(
                    &erl_path,
                    &session.beam_dir,
                    &session.runtime_dir,
                    &result.beam_module,
                    &fn_name,
                );
            }
            Err(CompileError::Diagnostics(diags, sources)) => {
                // Type / parse errors — render inline and continue.
                render_diagnostics(&diags, &sources);
            }
            Err(CompileError::Driver(msg) | CompileError::Io(msg)) => {
                eprintln!("error: {msg}");
            }
        }
    }

    Ok(())
}

// ── Input reader ──────────────────────────────────────────────────────────────

/// Read one complete REPL input from stdin.
///
/// Returns `Ok(Some(input))` when a complete input is available, `Ok(None)` on
/// EOF, and `Err` on I/O error.
///
/// A complete input is one or more lines where:
/// - The bracket depth is zero after the final line, AND
/// - The final line does not end with `\` (explicit continuation).
fn read_input(stdin: &io::Stdin) -> io::Result<Option<String>> {
    let mut lines = Vec::new();
    let mut reader = stdin.lock();

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;

        if n == 0 {
            // EOF.
            if lines.is_empty() {
                return Ok(None);
            }
            // Return whatever was accumulated before EOF.
            return Ok(Some(lines.join("\n")));
        }

        // Strip the trailing newline for bracket analysis.
        let bare = line.trim_end_matches(['\n', '\r']);

        // Check for explicit `\` continuation (strip the backslash).
        let explicit_continuation = bare.ends_with('\\');
        let effective_line = if explicit_continuation {
            bare.trim_end_matches('\\').to_owned()
        } else {
            bare.to_owned()
        };

        lines.push(effective_line.clone());

        if explicit_continuation {
            // Always read another line.
            continue;
        }

        // Compute cumulative bracket depth across all accumulated lines.
        let all_text = lines.join("\n");
        let depth = bracket_depth(&all_text);

        if depth <= 0 {
            return Ok(Some(all_text));
        }
        // depth > 0 — read another line.
    }
}

// ── Bracket-counting scanner (D162) ──────────────────────────────────────────

/// Compute the net bracket depth of `src` after scanning all characters.
///
/// Accounts for string literals (`"…"`) and line comments (`-- …`) so brackets
/// inside them do not affect the depth counter.
///
/// Rules:
/// - Outside a string / comment: `(`, `[`, `{` → +1; `)`, `]`, `}` → -1.
/// - Inside a `"…"` string: `\"` is an escape sequence; closing `"` ends the
///   string.  Brackets inside the string are ignored.
/// - `--` (outside a string) starts a line comment; the rest of the line is
///   ignored for bracket counting.
/// - Newlines reset the comment state.
fn bracket_depth(src: &str) -> i32 {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_comment = false;
    let chars: Vec<char> = src.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];

        if c == '\n' {
            // Newline resets line-comment state; strings span lines (Ridge
            // allows multi-line string literals inside `"…"`).
            in_comment = false;
            i += 1;
            continue;
        }

        if in_comment {
            i += 1;
            continue;
        }

        if in_string {
            if c == '\\' && i + 1 < len {
                // Escape sequence inside string — skip the next character.
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        // Outside string and comment.
        if c == '"' {
            in_string = true;
            i += 1;
            continue;
        }

        // Check for `--` line-comment start.
        if c == '-' && i + 1 < len && chars[i + 1] == '-' {
            in_comment = true;
            i += 2;
            continue;
        }

        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }

        i += 1;
    }

    depth
}

// ── Let-binding parser ────────────────────────────────────────────────────────

/// A parsed `let <name> = <value>` REPL command.
struct LetBinding {
    /// Binding name (identifier).
    name: String,
    /// Binding value expression (everything after `= `).
    value: String,
}

/// Try to parse a REPL line as a `let <name> = <expr>` binding.
///
/// Returns `None` if the line does not match the `let` prefix or is
/// malformed.  Whitespace around `=` is allowed; the name must be a simple
/// Ridge identifier (letters, digits, underscores; starts with a letter or
/// underscore).
fn try_parse_let_binding(input: &str) -> Option<LetBinding> {
    let rest = input.strip_prefix("let ")?;
    let rest = rest.trim_start();

    // Find the `=` separator.
    let eq_pos = rest.find('=')?;
    let name_part = rest[..eq_pos].trim();
    let value_part = rest[eq_pos + 1..].trim();

    if name_part.is_empty() || value_part.is_empty() {
        return None;
    }

    // Validate the name is a simple identifier.
    if !is_valid_identifier(name_part) {
        return None;
    }

    Some(LetBinding {
        name: name_part.to_owned(),
        value: value_part.to_owned(),
    })
}

/// Returns `true` if `s` is a valid Ridge identifier.
///
/// An identifier starts with a letter or underscore and consists of letters,
/// digits, and underscores only.
fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

// ── BEAM execution ────────────────────────────────────────────────────────────

/// Run a compiled REPL expression via the `ridge_repl_runner` Erlang bridge.
///
/// Invokes:
/// ```text
/// erl -pa <beam_dir> -pa <runtime_dir>
///     -s ridge_repl_runner run <beam_module> <fn_name>
///     -s init stop -noshell
/// ```
///
/// Output (stdout) is inherited so the REPL prints the result to the
/// terminal.  Stderr is also inherited so error messages are visible.
/// Waits up to 60 s for the child; kills it if it times out.
fn run_repl_expr(
    erl_path: &Path,
    beam_dir: &Path,
    runtime_dir: &Path,
    beam_module: &str,
    fn_name: &str,
) {
    let mut cmd = process::Command::new(erl_path);
    cmd.arg("-pa")
        .arg(beam_dir)
        .arg("-pa")
        .arg(runtime_dir)
        .arg("-s")
        .arg("ridge_repl_runner")
        .arg("run")
        .arg(beam_module)
        .arg(fn_name)
        .arg("-s")
        .arg("init")
        .arg("stop")
        .arg("-noshell");

    // Inherit stdout and stderr so output goes directly to the terminal.
    cmd.stdout(process::Stdio::inherit());
    cmd.stderr(process::Stdio::inherit());

    let child = cmd.spawn();
    let Ok(mut child) = child else {
        eprintln!("error: failed to spawn erl for REPL expression");
        return;
    };

    let timeout = std::time::Duration::from_secs(60);
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!("error: REPL expression timed out after 60s");
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                return;
            }
        }
    }
}

// ── Help text ─────────────────────────────────────────────────────────────────

/// Print the REPL help text.
fn print_help() {
    println!(
        "\
Ridge REPL help
  <expr>           evaluate an expression and print the result
  let x = <expr>   bind a name for use in subsequent expressions
  :q               quit the REPL
  :help            show this message

Continuation:
  Lines ending with unbalanced (, [, or {{ are continued automatically.
  A trailing \\ forces continuation regardless of bracket state.

Capabilities allowed: io, fs, net, time, random, env, proc, spawn
  Example: io.println \"hello\""
    );
}
