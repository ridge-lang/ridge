//! `ridge run` — compile and run a Ridge workspace on the BEAM runtime.
//!
//! ## Surface
//!
//! ```text
//! ridge run [--member <name>] [--release] [--watch] [--observer] [--cookie <value>] [-- <args>...]
//! ```
//!
//! ## Modes
//!
//! - **Plain**: delegates to [`ridge_driver::run_workspace`] (60 s timeout).
//! - **`--observer`**: compiles the workspace then spawns `erl` with a
//!   distributed-node name and cookie.  Prints connection info to stderr.
//! - **`--watch`** (requires feature `cli-watch`): watches `**/*.ridge` and
//!   `ridge.toml` via `notify`; debounces 500 ms; SIGTERMs the BEAM child on
//!   file change, waits 2 s, then SIGKILLs if still alive, recompiles, and
//!   relaunches.  Ctrl-C exits cleanly.

use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;
use ridge_driver::{
    compile_workspace, run_workspace, select_entry_beam, CompileOptions, Profile, RunError,
    RunOptions, ToolchainError,
};
use ridge_manifest::{find_workspace_root, parse_project, parse_workspace, ProjectKind};

use crate::error::CliError;
use crate::render::{render_diagnostics, report_diagnostics};

// ── Argument struct ───────────────────────────────────────────────────────────

/// Compile and run a Ridge workspace on the BEAM runtime.
///
/// The bools are independent clap flags (`--release`, `--watch`, `--reload`,
/// `--observer`), not state — a state machine would only obscure the CLI.
#[derive(Debug, Parser)]
#[allow(clippy::struct_excessive_bools)]
pub struct RunArgs {
    /// Only run the named workspace member (must have `kind = "app"` or `kind = "service"`).
    #[arg(long, value_name = "NAME")]
    pub member: Option<String>,

    /// Run in release mode.
    #[arg(long)]
    pub release: bool,

    /// Watch source files for changes and restart automatically.
    ///
    /// Requires feature `cli-watch` (compile with `--features cli-watch`).
    /// Debounces 500 ms between file events.  SIGINT exits cleanly.
    #[arg(long)]
    pub watch: bool,

    /// Watch sources and hot-reload compatible edits into the running dev
    /// node without losing actor state.
    ///
    /// Falls back to a manual restart for edits the checker rejects.
    /// Requires feature `cli-watch` and OTP 27+.
    #[arg(long, conflicts_with_all = ["watch", "observer"])]
    pub reload: bool,

    /// Connect an Erlang observer to the running node.
    ///
    /// Starts BEAM as a named node (`ridge_app@127.0.0.1`) and prints
    /// connection information to stderr.  Reads the Erlang cookie from
    /// `~/.erlang.cookie` (`%USERPROFILE%\.erlang.cookie` on Windows); use
    /// `--cookie` to override.
    #[arg(long)]
    pub observer: bool,

    /// Override the Erlang cookie used by `--observer`.
    ///
    /// When `--observer` is set and this flag is absent, the cookie is read
    /// from `~/.erlang.cookie`.
    #[arg(long, value_name = "VALUE")]
    pub cookie: Option<String>,

    /// Extra arguments passed to the BEAM node after `--`.
    #[arg(last = true)]
    pub extra_args: Vec<String>,
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge run`.
///
/// Dispatches to the appropriate mode based on the flags provided.
///
/// # Errors
///
/// Returns a [`CliError`] for workspace-structure problems.  BEAM process
/// failures are handled internally (exit code is propagated via
/// [`process::exit`]).
pub fn execute(args: &RunArgs, cwd: &Path) -> Result<(), CliError> {
    // ── 1. Validate --watch feature gate ─────────────────────────────────────
    if args.watch {
        #[cfg(not(feature = "cli-watch"))]
        {
            eprintln!(
                "error: --watch requires the `cli-watch` feature.\n\
                 Rebuild with: cargo build -p ridge-cli --features cli-watch"
            );
            process::exit(1);
        }
    }
    if args.reload {
        #[cfg(not(feature = "cli-watch"))]
        {
            eprintln!(
                "error: --reload requires the `cli-watch` feature.\n\
                 Rebuild with: cargo build -p ridge-cli --features cli-watch"
            );
            process::exit(1);
        }
    }

    // ── 2. Locate workspace root ──────────────────────────────────────────────
    let workspace_root =
        find_workspace_root(cwd).ok_or_else(|| CliError::no_workspace_root(cwd))?;

    // ── 3. Resolve executable member ─────────────────────────────────────────
    let member_name = resolve_executable_member(&workspace_root, args)?;

    // ── 4. Dispatch to appropriate mode ──────────────────────────────────────
    let profile = if args.release {
        Profile::Release
    } else {
        Profile::Debug
    };

    if args.watch {
        #[cfg(feature = "cli-watch")]
        {
            execute_watch(args, &workspace_root, &member_name, profile)?;
        }
        #[cfg(not(feature = "cli-watch"))]
        {
            // Already handled above — unreachable here.
        }
    } else if args.reload {
        #[cfg(feature = "cli-watch")]
        {
            execute_reload(args, &workspace_root, &member_name, profile)?;
        }
        #[cfg(not(feature = "cli-watch"))]
        {
            // Already handled above — unreachable here.
        }
    } else if args.observer {
        execute_observer(args, &workspace_root, &member_name, profile)?;
    } else {
        execute_plain(args, workspace_root, member_name, profile);
    }

    Ok(())
}

// ── Member resolution ─────────────────────────────────────────────────────────

/// Resolve which workspace member to run.
///
/// - If `--member` is given: validate it exists and is executable.
/// - Otherwise: find the unique executable member, erroring on 0 or >1.
fn resolve_executable_member(workspace_root: &Path, args: &RunArgs) -> Result<String, CliError> {
    // Collect all project manifests under apps/*.
    let executable_members = collect_executable_members(workspace_root);

    if let Some(ref name) = args.member {
        // Validate the named member exists and is executable.
        let all_members = collect_all_members(workspace_root);
        if !all_members.iter().any(|m| m == name) {
            return Err(CliError::UnknownMember { name: name.clone() });
        }
        if !executable_members.iter().any(|m| m == name) {
            // It exists but is not an app/service.
            return Err(CliError::LibraryNotExecutable { name: name.clone() });
        }
        return Ok(name.clone());
    }

    // No --member specified.
    match executable_members.len() {
        // A member whose manifest does not parse is skipped by the collector,
        // so "no executable member" is also what a typo in `ridge.toml` looks
        // like from here. Say which it is, or `run` sends the reader to the
        // `kind` key on a manifest that already declares it.
        0 => Err(first_member_manifest_error(workspace_root)
            .map_or(CliError::NoExecutableMember, |rendered| {
                CliError::MemberManifestInvalid { rendered }
            })),
        1 => Ok(executable_members.into_iter().next().unwrap_or_default()),
        _ if args.watch => Err(CliError::WatchAmbiguousMember),
        _ => {
            // For plain run (and observer), pick the first one arbitrarily
            // and let the driver handle it.  The spec does not say to error
            // here for plain run.
            Ok(executable_members.into_iter().next().unwrap_or_default())
        }
    }
}

/// The first member manifest that fails to parse, rendered with its `M###`
/// code, searching the same places [`collect_members_with_filter`] does and in
/// the same order.
fn first_member_manifest_error(workspace_root: &Path) -> Option<String> {
    let apps_dir = workspace_root.join("apps");
    if let Ok(entries) = std::fs::read_dir(&apps_dir) {
        // read_dir order is not defined, so sort to keep the reported error
        // stable across runs and platforms.
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let manifest_path = path.join("ridge.toml");
            let Ok(src) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            if let Err(e) = parse_project(&src, &manifest_path) {
                return Some(format!("[{}] {e}", e.code()));
            }
        }
    }

    let root_manifest = workspace_root.join("ridge.toml");
    let src = std::fs::read_to_string(&root_manifest).ok()?;
    let ws = parse_workspace(&src, &root_manifest).ok()?;
    if !ws.members_globs.iter().any(|p| p == ".") {
        return None;
    }
    parse_project(&src, &root_manifest)
        .err()
        .map(|e| format!("[{}] {e}", e.code()))
}

/// Collect the names of all members under `<workspace_root>/apps/`.
fn collect_all_members(workspace_root: &Path) -> Vec<String> {
    collect_members_with_filter(workspace_root, |_| true)
}

/// Collect the names of members with `kind = "app"` or `kind = "service"`.
fn collect_executable_members(workspace_root: &Path) -> Vec<String> {
    collect_members_with_filter(workspace_root, |kind| {
        matches!(kind, ProjectKind::App | ProjectKind::Service)
    })
}

/// Walk `<workspace_root>/apps/*/ridge.toml` AND honour `[workspace].members`
/// containing `"."` (the single-project layout produced by `ridge new`), then
/// return member names that pass the `filter` predicate applied to their
/// [`ProjectKind`].
///
/// Two layouts are supported:
/// - **Multi-project**: `<workspace_root>/apps/<name>/ridge.toml` per member.
/// - **Single-project**: `<workspace_root>/ridge.toml` carries BOTH the
///   `[workspace]` and `[project]` tables; `[workspace].members = ["."]`.
fn collect_members_with_filter<F>(workspace_root: &Path, filter: F) -> Vec<String>
where
    F: Fn(ProjectKind) -> bool,
{
    let mut names = Vec::new();

    // Multi-project layout: walk <workspace_root>/apps/*/ridge.toml.
    let apps_dir = workspace_root.join("apps");
    if let Ok(entries) = std::fs::read_dir(&apps_dir) {
        for entry in entries.flatten() {
            let manifest_path = entry.path().join("ridge.toml");
            let Ok(src) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            let Ok(proj) = parse_project(&src, &manifest_path) else {
                continue;
            };
            if filter(proj.kind) {
                names.push(proj.name);
            }
        }
    }

    // Single-project layout: workspace root manifest doubles as a project.
    let root_manifest = workspace_root.join("ridge.toml");
    if let Ok(src) = std::fs::read_to_string(&root_manifest) {
        if let Ok(ws) = parse_workspace(&src, &root_manifest) {
            if ws.members_globs.iter().any(|p| p == ".") {
                if let Ok(proj) = parse_project(&src, &root_manifest) {
                    if filter(proj.kind) && !names.contains(&proj.name) {
                        names.push(proj.name);
                    }
                }
            }
        }
    }

    names
}

// ── Plain run ─────────────────────────────────────────────────────────────────

/// Execute plain `ridge run` — delegate to `run_workspace`.
fn execute_plain(args: &RunArgs, workspace_root: PathBuf, member_name: String, profile: Profile) {
    let mut opts = RunOptions::new(workspace_root, member_name);
    opts.profile = profile;
    opts.extra_args.clone_from(&args.extra_args);

    if let Err(e) = run_workspace(opts) {
        match &e {
            RunError::CompileDiagnostics(payload) => {
                // Render the inner diagnostics the same way `ridge build` and
                // `ridge run --observer` do, then exit non-zero.  Without this
                // pre-existing `.beam` files from an earlier good compile
                // would be executed despite the new errors (capability gate
                // bypass).
                render_diagnostics(&payload.diagnostics, &payload.sources);
            }
            _ => {
                eprintln!("error: {e}");
            }
        }
        process::exit(1);
    }
}

// ── Observer mode ─────────────────────────────────────────────────────────────

/// Execute `ridge run --observer`.
///
/// Compiles the workspace, then spawns `erl` as a named distributed node.
/// Prints connection info to stderr before exec.
fn execute_observer(
    args: &RunArgs,
    workspace_root: &Path,
    member_name: &str,
    profile: Profile,
) -> Result<(), CliError> {
    // ── a. Resolve cookie ─────────────────────────────────────────────────────
    let cookie = resolve_cookie(args)?;

    // ── b. Compile ────────────────────────────────────────────────────────────
    let mut compile_opts = CompileOptions::new(workspace_root.to_owned())
        .with_profile(profile)
        .executing();
    compile_opts.members = Some(vec![member_name.to_owned()]);
    let artefacts = compile_workspace(compile_opts).map_err(|e| {
        eprintln!("error: {e}");
        CliError::AlreadyReported
    })?;
    if report_diagnostics(&artefacts.diagnostics, &artefacts.sources, false).fatal() {
        process::exit(1);
    }
    if artefacts.beam_files.is_empty() {
        eprintln!("error: no .beam files produced");
        process::exit(1);
    }

    // ── c. Resolve beam_dir and module ────────────────────────────────────────
    let beam_dir = beam_dir_from_artefacts(&artefacts.beam_files);
    let module_name = select_entry_beam(&artefacts.entry_modules, member_name)
        .unwrap_or_else(|| module_from_beam(&artefacts.beam_files[0]));

    // ── d. Print connection info to stderr ────────────────────────────────────
    eprintln!(
        "Connect with: erl -name probe@127.0.0.1 -setcookie {cookie} -remsh ridge_app@127.0.0.1"
    );

    // ── e. Spawn BEAM node ────────────────────────────────────────────────────
    let erl_path = which::which("erl").map_err(|_| ToolchainError::erl_not_found())?;

    let mut cmd = process::Command::new(&erl_path);
    cmd.arg("-name")
        .arg("ridge_app@127.0.0.1")
        .arg("-setcookie")
        .arg(&cookie)
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-s")
        .arg(&module_name)
        .arg("main")
        .arg("-s")
        .arg("init")
        .arg("stop")
        .arg("-noshell");

    for arg in &args.extra_args {
        cmd.arg(arg);
    }

    let status = cmd
        .status()
        .map_err(|e| ToolchainError::erl_spawn_failed(&e))?;

    let code = status.code().unwrap_or(-1);
    if code != 0 {
        process::exit(code);
    }
    Ok(())
}

/// Resolve the Erlang cookie for `--observer`.
///
/// Priority: `--cookie <value>` flag → `~/.erlang.cookie` file.
fn resolve_cookie(args: &RunArgs) -> Result<String, CliError> {
    if let Some(ref c) = args.cookie {
        return Ok(c.clone());
    }

    let cookie_path = erlang_cookie_path().ok_or(CliError::ObserverNoCookie)?;
    std::fs::read_to_string(&cookie_path)
        .map(|s| s.trim().to_owned())
        .map_err(|_| CliError::ObserverNoCookie)
}

/// Return the platform-appropriate path to `~/.erlang.cookie`.
fn erlang_cookie_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".erlang.cookie"))
}

// ── Watch mode ────────────────────────────────────────────────────────────────

#[cfg(feature = "cli-watch")]
/// Execute `ridge run --watch`.
///
/// Algorithm:
/// 1. Initial compile — exit non-zero if it fails (do not enter loop).
/// 2. Spawn BEAM child.
/// 3. Watch `**/*.ridge` and `ridge.toml` via `notify::RecommendedWatcher`.
/// 4. On file event: debounce 500 ms; SIGTERM child; wait 2 s; SIGKILL if
///    still alive; recompile; relaunch.
/// 5. SIGINT exits cleanly (no zombie children left behind — R14).
#[allow(clippy::too_many_lines)]
fn execute_watch(
    args: &RunArgs,
    workspace_root: &Path,
    member_name: &str,
    profile: Profile,
) -> Result<(), CliError> {
    use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let debounce = Duration::from_millis(500);
    let grace = Duration::from_secs(2);

    // ── a. Initial compile ────────────────────────────────────────────────────
    let (beam_files, entry_module) =
        compile_for_watch(workspace_root, member_name, profile, &args.extra_args)?;
    let beam_dir = beam_dir_from_artefacts(&beam_files);
    let module_name = entry_module.unwrap_or_else(|| module_from_beam(&beam_files[0]));

    let erl_path = which::which("erl").map_err(|_| ToolchainError::erl_not_found())?;

    // ── b. Spawn initial child ────────────────────────────────────────────────
    let mut child = spawn_beam_child(
        &erl_path,
        &beam_dir,
        &module_name,
        args.observer,
        args.cookie.as_deref(),
        &args.extra_args,
    )?;

    // ── c. Set up file watcher ────────────────────────────────────────────────
    // Shared state: last event timestamp (for debouncing).
    let last_event: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let last_event_clone = Arc::clone(&last_event);

    // Channel to receive notify events.
    let (tx, rx) = std::sync::mpsc::channel::<()>();

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let Ok(event) = res else { return };
            let relevant = matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            );
            if !relevant {
                return;
            }
            // Update last-event timestamp.
            if let Ok(mut guard) = last_event_clone.lock() {
                *guard = Some(Instant::now());
            }
            // Signal the main loop (ignore send errors if receiver is gone).
            let _ = tx.send(());
        })
        .map_err(|e| CliError::WatcherStartFailed {
            message: e.to_string(),
        })?;

    watcher
        .watch(workspace_root, RecursiveMode::Recursive)
        .map_err(|e| CliError::WatchPathFailed {
            path: workspace_root.to_path_buf(),
            message: e.to_string(),
        })?;

    eprintln!("Watching for changes. Press Ctrl-C to exit.");

    // ── d. Watch loop ─────────────────────────────────────────────────────────
    loop {
        // Wait for a file-change signal.
        if rx.recv().is_err() {
            // Watcher dropped — exit.
            break;
        }

        // Drain additional rapid signals.
        while rx.try_recv().is_ok() {}

        // Debounce: wait until 500 ms after the last event.
        loop {
            let elapsed = {
                let guard = last_event
                    .lock()
                    .map_err(|_| CliError::WatchStateCorrupted)?;
                guard
                    .as_ref()
                    .map_or(debounce + Duration::from_millis(1), Instant::elapsed)
            };
            if elapsed >= debounce {
                break;
            }
            std::thread::sleep(debounce.saturating_sub(elapsed));
        }

        // ── Kill old child ────────────────────────────────────────────────────
        terminate_child(&mut child, grace);

        // ── Recompile ─────────────────────────────────────────────────────────
        let (new_beam_files, new_entry_module) =
            match compile_for_watch(workspace_root, member_name, profile, &args.extra_args) {
                Ok(out) => out,
                Err(e) => {
                    eprintln!("error: recompile failed: {e}");
                    eprintln!("Watching for changes (will retry on next save).");
                    // Spawn a sentinel child that always exits immediately, so
                    // the next iteration has something to kill.
                    child = spawn_noop_child().map_err(|e| {
                        eprintln!("error: cannot spawn sentinel child: {e}");
                        CliError::AlreadyReported
                    })?;
                    continue;
                }
            };

        let new_beam_dir = beam_dir_from_artefacts(&new_beam_files);
        let new_module = new_entry_module.unwrap_or_else(|| module_from_beam(&new_beam_files[0]));

        // ── Relaunch ──────────────────────────────────────────────────────────
        child = match spawn_beam_child(
            &erl_path,
            &new_beam_dir,
            &new_module,
            args.observer,
            args.cookie.as_deref(),
            &args.extra_args,
        ) {
            Ok(c) => c,
            Err(e) => {
                // The relaunch failed; hold a no-op child so the loop keeps
                // watching and the next save gets another try. If even that
                // will not start, the OS is refusing new processes and there
                // is nothing left to watch with.
                eprintln!("error: failed to relaunch after the change: {e}");
                spawn_noop_child().map_err(|err| CliError::WatchRestartFailed {
                    message: err.to_string(),
                })?
            }
        };

        eprintln!("Restarted.");
    }

    // ── e. Cleanup: kill child on exit ────────────────────────────────────────
    terminate_child(&mut child, grace);
    Ok(())
}

#[cfg(feature = "cli-watch")]
/// Compile the workspace for watch mode.  Returns the beam file paths plus the
/// entry module's BEAM atom (the module carrying `fn main`), or a `CliError`
/// whose cause was already printed to stderr.
fn compile_for_watch(
    workspace_root: &Path,
    member_name: &str,
    profile: Profile,
    _extra_args: &[String],
) -> Result<(Vec<PathBuf>, Option<String>), CliError> {
    let mut opts = CompileOptions::new(workspace_root.to_owned()).with_profile(profile);
    opts.members = Some(vec![member_name.to_owned()]);
    let artefacts = compile_workspace(opts).map_err(|e| {
        eprintln!("error: {e}");
        CliError::AlreadyReported
    })?;
    if report_diagnostics(&artefacts.diagnostics, &artefacts.sources, false).fatal() {
        return Err(CliError::AlreadyReported);
    }
    if artefacts.beam_files.is_empty() {
        eprintln!("error: no .beam files produced");
        return Err(CliError::AlreadyReported);
    }
    let entry = select_entry_beam(&artefacts.entry_modules, member_name);
    Ok((artefacts.beam_files, entry))
}

#[cfg(feature = "cli-watch")]
/// Spawn a BEAM child process.
fn spawn_beam_child(
    erl_path: &Path,
    beam_dir: &Path,
    module_name: &str,
    observer: bool,
    cookie: Option<&str>,
    extra_args: &[String],
) -> Result<process::Child, CliError> {
    let mut cmd = process::Command::new(erl_path);
    cmd.arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-s")
        .arg(module_name)
        .arg("main")
        .arg("-s")
        .arg("init")
        .arg("stop");

    if observer {
        if let Some(c) = cookie {
            cmd.arg("-name")
                .arg("ridge_app@127.0.0.1")
                .arg("-setcookie")
                .arg(c);
        }
    }

    for arg in extra_args {
        cmd.arg(arg);
    }

    cmd.spawn()
        .map_err(|e| ToolchainError::erl_spawn_failed(&e).into())
}

#[cfg(feature = "cli-watch")]
/// Terminate a child process gracefully: SIGTERM → 2 s grace → SIGKILL (R14).
///
/// On Windows: uses [`std::process::Child::kill`] which calls `TerminateProcess`.
/// On Unix: sends `SIGTERM` via the `libc` crate, then falls back to SIGKILL
/// after the grace period.
fn terminate_child(child: &mut process::Child, grace: std::time::Duration) {
    // Try graceful termination first.
    #[cfg(unix)]
    {
        // std does not expose SIGTERM; `libc::kill` is the canonical Unix call.
        // PID always fits in `i32` because Linux PID_MAX_LIMIT is 2^22.
        #[allow(
            unsafe_code,
            reason = "libc::kill is the only stable way to send SIGTERM; std::process::Child::kill maps to SIGKILL"
        )]
        #[allow(
            clippy::cast_possible_wrap,
            reason = "u32 PID always fits in i32 on Unix (PID_MAX_LIMIT = 2^22)"
        )]
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
    }
    #[cfg(windows)]
    {
        // On Windows `kill()` calls `TerminateProcess` (immediate, not graceful).
        let _ = child.kill();
    }

    // Wait up to `grace` for the child to exit.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return, // reaped cleanly
            Ok(None) => {
                if start.elapsed() >= grace {
                    // Grace period expired — force-kill.
                    let _ = child.kill();
                    let _ = child.wait();
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

#[cfg(feature = "cli-watch")]
/// Spawn a no-op child (exits immediately with code 0).
///
/// Used as a placeholder when the initial or recompile child cannot be
/// launched, so that subsequent iterations have a valid `Child` to reap.
/// Returns `Err` only if the host shell is so broken that not even the
/// platform's trivial exit-0 command can be spawned — in which case the
/// caller should bail out of the watch loop.
fn spawn_noop_child() -> std::io::Result<process::Child> {
    #[cfg(unix)]
    {
        process::Command::new("true")
            .spawn()
            .or_else(|_| process::Command::new("sh").arg("-c").arg("exit 0").spawn())
    }
    #[cfg(windows)]
    {
        process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Extract the parent directory from the first beam file in the list.
fn beam_dir_from_artefacts(beam_files: &[PathBuf]) -> PathBuf {
    beam_files
        .first()
        .and_then(|f| f.parent())
        .map_or_else(|| PathBuf::from("."), Path::to_owned)
}

/// Extract the BEAM module name (file stem) from a `.beam` path.
fn module_from_beam(beam_path: &Path) -> String {
    beam_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ridge_module_0")
        .to_owned()
}

// ── ridge run --reload (cli-watch) ────────────────────────────────────────────

#[cfg(feature = "cli-watch")]
use ridge_driver::{
    manifest_path_for, plan_reload, snapshot_path_for, snapshot_vsn, CheckOptions,
    WorkspaceSnapshot,
};

#[cfg(feature = "cli-watch")]
/// Execute `ridge run --reload`: boot a persistent dev node, then per save
/// compile → plan → hot-apply into the node. Rejected edits keep the node on
/// its current code and offer a cold restart.
fn execute_reload(
    args: &RunArgs,
    workspace_root: &Path,
    member_name: &str,
    profile: Profile,
) -> Result<(), CliError> {
    use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let debounce = Duration::from_millis(500);
    let grace = Duration::from_secs(2);
    let profile_name = profile.dir_name();

    let erl_path = which::which("erl").map_err(|_| ToolchainError::erl_not_found())?;

    // Cookie for the probe<->node handshake: user-provided or generated.
    let cookie = args.cookie.clone().unwrap_or_else(|| {
        use std::hash::Hasher;
        let mut h = rustc_hash::FxHasher::default();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.write_usize(std::process::id() as usize);
        format!("ridge{:016x}", h.finish())
    });
    let node_name = format!("ridge_dev_{}@127.0.0.1", std::process::id());

    // ── Initial compile + boot ────────────────────────────────────────────
    let (beam_files, entry_module) =
        compile_for_watch(workspace_root, member_name, profile, &args.extra_args)?;
    let beam_dir = beam_dir_from_artefacts(&beam_files);
    let module_name = entry_module.unwrap_or_else(|| module_from_beam(&beam_files[0]));

    let snap_path = snapshot_path_for(workspace_root, profile_name);
    let manifest_path = manifest_path_for(workspace_root, profile_name);
    let mut node_snapshot: WorkspaceSnapshot = read_snapshot(&snap_path)?;
    let mut node_vsn = snapshot_vsn(&node_snapshot);

    let mut child = spawn_reload_node(
        &erl_path,
        &beam_dir,
        &module_name,
        &node_name,
        &cookie,
        &node_vsn,
        &args.extra_args,
    )?;
    eprintln!(
        "dev node {node_name} up (vsn {node_vsn}); watching for changes. \
         Press r + Enter to restart, Ctrl-C to exit."
    );

    // ── stdin restart channel ─────────────────────────────────────────────
    let (stdin_tx, stdin_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            if line.map(|l| stdin_tx.send(l)).is_err() {
                return;
            }
        }
    });

    // ── File watcher (same shape as --watch) ──────────────────────────────
    let last_event: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let last_event_clone = Arc::clone(&last_event);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let Ok(event) = res else { return };
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                return;
            }
            if let Ok(mut guard) = last_event_clone.lock() {
                *guard = Some(Instant::now());
            }
            let _ = tx.send(());
        })
        .map_err(|e| CliError::WatcherStartFailed {
            message: e.to_string(),
        })?;
    watcher
        .watch(workspace_root, RecursiveMode::Recursive)
        .map_err(|e| CliError::WatchPathFailed {
            path: workspace_root.to_path_buf(),
            message: e.to_string(),
        })?;

    let mut probe_seq: u64 = 0;

    loop {
        // Restart requests are honored between file events.
        if rx.recv().is_err() {
            break;
        }
        while rx.try_recv().is_ok() {}
        loop {
            let elapsed = {
                let guard = last_event
                    .lock()
                    .map_err(|_| CliError::WatchStateCorrupted)?;
                guard
                    .as_ref()
                    .map_or(debounce + Duration::from_millis(1), Instant::elapsed)
            };
            if elapsed >= debounce {
                break;
            }
            std::thread::sleep(debounce.saturating_sub(elapsed));
        }

        // Drain any queued restart request first.
        if drain_restart(&stdin_rx) {
            restart_node(
                &erl_path,
                &mut child,
                grace,
                workspace_root,
                member_name,
                profile,
                &args.extra_args,
                &node_name,
                &cookie,
                &snap_path,
                &mut node_snapshot,
                &mut node_vsn,
            )?;
            continue;
        }

        // ── Recompile ─────────────────────────────────────────────────────
        if let Err(e) = compile_for_watch(workspace_root, member_name, profile, &args.extra_args) {
            let _ = e; // diagnostics already rendered by compile_for_watch
            eprintln!("compile failed; node keeps running the previous code.");
            continue;
        }

        // ── Plan ──────────────────────────────────────────────────────────
        let plan = match plan_reload(
            &node_snapshot,
            CheckOptions::new(workspace_root.to_owned()),
            &manifest_path,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("reload planning failed: {e}; node keeps running the previous code.");
                continue;
            }
        };

        let Some(manifest) = plan.manifest else {
            crate::cmd::reload::print_reload_rejection(&plan.report);
            eprintln!(
                "reload rejected — press r + Enter to restart with the new code, or keep editing."
            );
            continue;
        };

        if manifest.modules.is_empty() {
            // Identical rebuild (e.g. our own build outputs retriggered the
            // watcher): nothing to load, nothing to report.
            continue;
        }

        // ── Apply via probe ───────────────────────────────────────────────
        probe_seq += 1;
        match probe_apply(
            &erl_path,
            &node_name,
            &cookie,
            &manifest_path,
            &manifest.base_vsn,
            probe_seq,
        ) {
            Ok(line) => {
                node_snapshot = plan.new_snapshot;
                node_vsn = manifest.new_vsn.clone();
                eprintln!("{line}");
            }
            Err(msg) => {
                eprintln!("reload failed at the node: {msg}");
                eprintln!("node is unchanged — press r + Enter to restart, or keep editing.");
            }
        }
    }

    terminate_child(&mut child, grace);
    Ok(())
}

#[cfg(feature = "cli-watch")]
/// Read and parse the workspace snapshot written by the last clean compile.
fn read_snapshot(path: &Path) -> Result<WorkspaceSnapshot, CliError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        eprintln!("error: cannot read reload snapshot {}: {e}", path.display());
        CliError::AlreadyReported
    })?;
    serde_json::from_str(&text).map_err(|e| {
        eprintln!(
            "error: cannot parse reload snapshot {}: {e}",
            path.display()
        );
        CliError::AlreadyReported
    })
}

#[cfg(feature = "cli-watch")]
/// Boot the persistent dev node: named, cookied, stays alive after main.
fn spawn_reload_node(
    erl_path: &Path,
    beam_dir: &Path,
    module_name: &str,
    node_name: &str,
    cookie: &str,
    vsn: &str,
    extra_args: &[String],
) -> Result<process::Child, CliError> {
    let mut cmd = process::Command::new(erl_path);
    cmd.arg("-name")
        .arg(node_name)
        .arg("-setcookie")
        .arg(cookie)
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(format!(
            "persistent_term:put(ridge_loader_vsn, <<\"{vsn}\">>)."
        ))
        .arg("-s")
        .arg("ridge_main_runner")
        .arg("run_detached")
        .arg(module_name)
        .arg("main");
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.spawn()
        .map_err(|e| ToolchainError::erl_spawn_failed(&e).into())
}

#[cfg(feature = "cli-watch")]
/// Apply the manifest in the dev node via a short-lived probe node.
/// Returns the one-line report on success, or the node's error term.
fn probe_apply(
    erl_path: &Path,
    node_name: &str,
    cookie: &str,
    manifest_path: &Path,
    base_vsn: &str,
    seq: u64,
) -> Result<String, String> {
    // Erlang string literals treat backslashes as escapes — use forward slashes.
    let manifest_fwd = manifest_path.to_string_lossy().replace('\\', "/");
    let eval = format!(
        "case rpc:call('{node_name}', ridge_loader, apply, [\"{manifest_fwd}\", <<\"{base_vsn}\">>]) of \
            {{ok, Rep}} -> io:format(\"RIDGE_RELOAD_OK ~w ~w ~w ~w~n\", [maps:get(modules_loaded, Rep), maps:get(actors_migrated, Rep), maps:get(messages_migrated, Rep), maps:get(duration_ms, Rep)]); \
            Err -> io:format(\"RIDGE_RELOAD_ERR ~p~n\", [Err]) \
        end."
    );
    let output = process::Command::new(erl_path)
        .arg("-name")
        .arg(format!(
            "ridge_probe_{}_{}@127.0.0.1",
            std::process::id(),
            seq
        ))
        .arg("-setcookie")
        .arg(cookie)
        .arg("-noshell")
        .arg("-eval")
        .arg(eval)
        .arg("-s")
        .arg("init")
        .arg("stop")
        .output()
        .map_err(|e| format!("failed to spawn probe: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("RIDGE_RELOAD_OK ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if let [modules, actors, messages, ms] = parts.as_slice() {
                if messages == &"0" {
                    // Zero-suppressed: keep the common case on the terse line.
                    return Ok(format!(
                        "reloaded {modules} modules, migrated {actors} actors in {ms}ms"
                    ));
                }
                return Ok(format!(
                    "reloaded {modules} modules, migrated {actors} actors, {messages} in-flight messages in {ms}ms"
                ));
            }
        }
        if let Some(rest) = line.strip_prefix("RIDGE_RELOAD_ERR ") {
            return Err(rest.to_string());
        }
    }
    Err(format!("probe produced no verdict (stdout: {stdout})"))
}

#[cfg(feature = "cli-watch")]
/// True when the user typed `r` since the last drain.
fn drain_restart(stdin_rx: &std::sync::mpsc::Receiver<String>) -> bool {
    let mut restart = false;
    while let Ok(line) = stdin_rx.try_recv() {
        if line.trim() == "r" {
            restart = true;
        }
    }
    restart
}

#[cfg(feature = "cli-watch")]
/// Cold-restart the dev node on the current on-disk build, rebasing the
/// running snapshot/version. Used for the interactive fallback.
#[allow(clippy::too_many_arguments)]
fn restart_node(
    erl_path: &Path,
    child: &mut process::Child,
    grace: std::time::Duration,
    workspace_root: &Path,
    member_name: &str,
    profile: Profile,
    extra_args: &[String],
    node_name: &str,
    cookie: &str,
    snap_path: &Path,
    node_snapshot: &mut WorkspaceSnapshot,
    node_vsn: &mut String,
) -> Result<(), CliError> {
    terminate_child(child, grace);
    let (beam_files, entry_module) =
        compile_for_watch(workspace_root, member_name, profile, extra_args)?;
    let beam_dir = beam_dir_from_artefacts(&beam_files);
    let module_name = entry_module.unwrap_or_else(|| module_from_beam(&beam_files[0]));
    *node_snapshot = read_snapshot(snap_path)?;
    *node_vsn = snapshot_vsn(node_snapshot);
    *child = spawn_reload_node(
        erl_path,
        &beam_dir,
        &module_name,
        node_name,
        cookie,
        node_vsn,
        extra_args,
    )?;
    eprintln!("restarted on current code (vsn {node_vsn}).");
    Ok(())
}
