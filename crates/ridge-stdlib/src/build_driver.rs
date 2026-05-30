// build_driver.rs — Ridge stdlib build orchestrator (T4).
//
// Compilation approach: pipeline crates live in [dependencies] so that this
// module is reachable as `ridge_stdlib::build_driver` at lib-test time.
// build.rs uses `include!("src/build_driver.rs")` to share the same source,
// accessing the pipeline crates through its own [build-dependencies] entry.
//
// §11.4 preferred [build-dependencies]-only placement is superseded here
// because `pub mod build_driver` in lib.rs requires the crate to compile
// against its [dependencies], not just [build-dependencies].

use std::fmt;
use std::path::{Path, PathBuf};

use ridge_lower::lower_workspace;
use ridge_resolve::{discover_workspace, resolve_workspace, ResolveError, Severity};
use ridge_typecheck::typecheck_workspace;
use tempfile::TempDir;

// ── Tier table (§4.1) ────────────────────────────────────────────────────────

/// One tier of the stdlib dependency graph.
pub struct TierPlan {
    /// Tier number (0–4).  Tier 0 has no Ridge source.
    pub tier: u32,
    /// Dotted module names present in this tier.
    pub modules: &'static [&'static str],
}

/// The five stdlib tiers in dependency order (§4.1).
///
/// Tier 0 — language built-ins — carries no `.ridge` files and is listed for
/// completeness only; `build_all` skips it.
pub const TIERS: &[TierPlan] = &[
    TierPlan {
        tier: 0,
        modules: &[],
    },
    TierPlan {
        tier: 1,
        modules: &[
            "std.int",
            "std.float",
            "std.bool",
            "std.option",
            "std.result",
        ],
    },
    TierPlan {
        tier: 2,
        modules: &["std.text", "std.list", "std.map", "std.set"],
    },
    TierPlan {
        tier: 3,
        modules: &[
            "std.io",
            "std.fs",
            "std.time",
            "std.random",
            "std.env",
            "std.cli",
            "std.proc",
            "std.actor",
        ],
    },
    TierPlan {
        tier: 4,
        modules: &["std.json", "std.net.http"],
    },
];

// ── Error types (T203 / T204) ─────────────────────────────────────────────────

/// A build error produced by the stdlib orchestrator.
///
/// - `T203 StdlibCircularImport` — a within-tier import cycle was detected.
/// - `T204 StdlibTierBuildFailed` — a module in a tier failed to compile.
#[derive(Debug)]
pub enum BuildError {
    /// T203 — a cyclic import was detected within a single tier.
    ///
    /// Currently this surfaces when `ridge-resolve` reports an R003
    /// `CyclicImport` for the tier's module group.
    CircularImport {
        /// The tier in which the cycle was detected.
        tier: u32,
        /// Module names forming the cycle (dotted form).
        cycle: Vec<String>,
    },

    /// T204 — a module in a tier failed to compile.
    TierBuildFailed {
        /// The tier number.
        tier: u32,
        /// The dotted module name that triggered the failure.
        module: String,
        /// The source file path.
        path: String,
        /// Human-readable error description.
        source: String,
    },
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CircularImport { tier, cycle } => {
                write!(
                    f,
                    "T203 StdlibCircularImport tier={tier} cycle=[{}]",
                    cycle.join(" -> ")
                )
            }
            Self::TierBuildFailed {
                tier,
                module,
                path,
                source,
            } => {
                write!(
                    f,
                    "T204 StdlibTierBuildFailed tier={tier} module={module} path={path} error={source}"
                )
            }
        }
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────────

/// A module that was found on disk, ready for compilation.
pub struct DiscoveredModule {
    /// Dotted module name, e.g. `"std.int"`.
    pub name: String,
    /// Tier this module belongs to.
    pub tier: u32,
    /// Absolute path to the `.ridge` source file.
    pub path: PathBuf,
}

/// Discover which stdlib modules are present on disk, in tier order.
///
/// For each tier (1–4) and each module name in `TIERS`, checks whether the
/// corresponding `.ridge` file exists under `stdlib_dir`.  Missing files are
/// silently skipped — T5+ adds them progressively.
///
/// Module `std.net.http` lives at `<stdlib_dir>/net/http.ridge` (§11.4 / T9).
/// All other modules live at `<stdlib_dir>/<last-component>.ridge`.
#[must_use]
pub fn discover(stdlib_dir: &Path) -> Vec<DiscoveredModule> {
    let mut found = Vec::new();
    for tier in TIERS {
        for &dotted in tier.modules {
            let rel_path = module_path(dotted);
            let full = stdlib_dir.join(&rel_path);
            if full.exists() {
                found.push(DiscoveredModule {
                    name: dotted.to_owned(),
                    tier: tier.tier,
                    path: full,
                });
            }
        }
    }
    found
}

/// Map a dotted module name to its relative `.ridge` path under `stdlib/`.
///
/// `std.net.http` → `net/http.ridge`
/// `std.int`      → `int.ridge`
fn module_path(dotted: &str) -> PathBuf {
    // Strip the leading "std." prefix.
    let rest = dotted.strip_prefix("std.").unwrap_or(dotted);
    // Replace remaining dots with path separators.
    let with_slashes = rest.replace('.', "/");
    PathBuf::from(format!("{with_slashes}.ridge"))
}

// ── Build summary ─────────────────────────────────────────────────────────────

/// Summary of a successful `build_all` run.
#[derive(Debug)]
pub struct BuildSummary {
    /// Number of tiers that had at least one module compiled.
    pub tiers_built: u32,
    /// Dotted module names compiled, in tier order.
    pub modules_built: Vec<String>,
}

// ── Tier compilation ──────────────────────────────────────────────────────────

/// Compile all present stdlib modules, walking tiers 1–4 in order.
///
/// For each tier, collects the present modules, creates a temporary workspace,
/// and runs lex → parse → resolve → typecheck → lower over them as a group.
/// Returns immediately with `Err(BuildError)` if any tier fails.
///
/// Tier 0 is skipped (no Ridge source).  If `stdlib_dir` does not exist or
/// contains no `.ridge` files, returns `Ok(BuildSummary { tiers_built: 0, .. })`.
///
/// # Errors
///
/// Returns `BuildError::CircularImport` (T203) if a within-tier import cycle
/// is detected, or `BuildError::TierBuildFailed` (T204) if any module fails
/// to lex, parse, resolve, typecheck, or lower.
pub fn build_all(stdlib_dir: &Path) -> Result<BuildSummary, BuildError> {
    let discovered = discover(stdlib_dir);

    let mut tiers_built: u32 = 0;
    let mut modules_built: Vec<String> = Vec::new();

    // Walk tiers 1..=4; tier 0 has no Ridge code.
    for tier_plan in TIERS.iter().filter(|t| t.tier >= 1) {
        let tier_modules: Vec<&DiscoveredModule> = discovered
            .iter()
            .filter(|m| m.tier == tier_plan.tier)
            .collect();

        if tier_modules.is_empty() {
            // Nothing present in this tier — skip silently.
            continue;
        }

        compile_tier(tier_plan.tier, &tier_modules, stdlib_dir)?;

        tiers_built += 1;
        for m in &tier_modules {
            modules_built.push(m.name.clone());
        }
    }

    Ok(BuildSummary {
        tiers_built,
        modules_built,
    })
}

// ── Internal: per-tier compilation ───────────────────────────────────────────

/// Compile all modules in one tier by constructing a temporary workspace
/// and running the full pipeline over it.
///
/// The temporary directory is bound to a [`TempDir`] held for the lifetime
/// of this function — Drop removes the directory on every exit path
/// (success, early return, or panic), so a failure in resolve, typecheck,
/// or lower no longer leaks `/tmp/ridge_stdlib_tier*` orphans.
fn compile_tier(
    tier: u32,
    modules: &[&DiscoveredModule],
    stdlib_dir: &Path,
) -> Result<(), BuildError> {
    // Build a temporary workspace under the OS temp dir. The TempDir guard
    // stays bound until the end of this function; the underlying directory is
    // removed on Drop regardless of how we exit.
    let tmp_dir = build_temp_workspace(tier, modules, stdlib_dir).map_err(|e| {
        BuildError::TierBuildFailed {
            tier,
            module: "<setup>".to_owned(),
            path: stdlib_dir.display().to_string(),
            source: e,
        }
    })?;
    let tmp_root = tmp_dir.path();

    // Run discover → resolve → typecheck → lower.
    let disc = discover_workspace(tmp_root);

    // Surface any workspace-discovery errors as T204.
    if !disc.resolve_errors.is_empty() {
        let first = &disc.resolve_errors[0];
        return Err(error_from_resolve(tier, modules, first));
    }

    let Some(ws_graph) = disc.graph else {
        return Err(BuildError::TierBuildFailed {
            tier,
            module: "<discovery>".to_owned(),
            path: stdlib_dir.display().to_string(),
            source: "workspace graph not produced by discovery".to_owned(),
        });
    };

    // Validate the stdlib's own `@ffi` declarations against the closed-list
    // audit table (T001 arity, T002 capability, T004 unknown target) before
    // the sources are compiled. The audit table is the single source of truth
    // for which BEAM targets the standard library is permitted to reach, so a
    // declaration that drifts out of the table must fail the build rather than
    // ship a stub that crashes — or escapes the capability model — at runtime.
    validate_tier_ffi(tier, modules, &ws_graph)?;

    let resolved = resolve_workspace(ws_graph);

    // Check for R003 (cycle) or R004 (self-import) — surface as T203.
    // All other errors surface as T204.
    for (_, err) in &resolved.errors {
        if err.severity() == Severity::Error {
            if matches!(
                err,
                ResolveError::CyclicImport { .. } | ResolveError::SelfImport { .. }
            ) {
                // TODO(T203): split out cycle errors once resolve exposes a
                // typed cycle-path with dotted module names.
                let cycle_names: Vec<String> = modules.iter().map(|m| m.name.clone()).collect();
                return Err(BuildError::CircularImport {
                    tier,
                    cycle: cycle_names,
                });
            }
            return Err(error_from_resolve(tier, modules, err));
        }
    }

    // Typecheck.
    let typecheck_result = typecheck_workspace(&resolved);

    if !typecheck_result.errors.is_empty() {
        let (_, first_err) = &typecheck_result.errors[0];
        let (mod_name, mod_path) = first_module_label(modules);
        return Err(BuildError::TierBuildFailed {
            tier,
            module: mod_name,
            path: mod_path,
            source: first_err.to_string(),
        });
    }

    // Lower.
    let _lowered = lower_workspace(&typecheck_result.typed, &resolved);

    // `tmp_dir` Drop runs at scope exit and removes the workspace directory.
    Ok(())
}

/// Create a temporary on-disk workspace containing all the modules for one
/// tier. Returns a [`TempDir`] guard whose `Drop` removes the directory on
/// every exit path (Ok, Err, panic). The `prefix` keeps the directory name
/// recognisable while `tempfile::Builder` appends random characters so
/// concurrent builds and partially-interrupted prior runs cannot collide.
///
/// Layout (relative to `dir.path()`):
/// ```text
/// ridge.toml            (workspace)
/// stdlib/ridge.toml     (project, kind = "library")
/// stdlib/src/<rel>.ridge   (source files)
/// ```
fn build_temp_workspace(
    tier: u32,
    modules: &[&DiscoveredModule],
    _stdlib_dir: &Path,
) -> Result<TempDir, String> {
    let tmp_dir = tempfile::Builder::new()
        .prefix(&format!("ridge_stdlib_tier{tier}_"))
        .tempdir()
        .map_err(|e| format!("could not create temp dir for tier {tier}: {e}"))?;
    let tmp_root = tmp_dir.path();

    // Name the project directory `ridge-stdlib` so the source paths handed to
    // the resolver carry the marker that the `@ffi` crate-path gate (R022)
    // looks for. The stdlib genuinely lives in `crates/ridge-stdlib`; these
    // copied tier sources are part of that same build, and must be treated as
    // stdlib rather than user code.
    let proj_dir = tmp_root.join("ridge-stdlib");
    std::fs::create_dir_all(proj_dir.join("src"))
        .map_err(|e| format!("could not create src dir: {e}"))?;

    // Workspace manifest.
    write_str(
        &tmp_root.join("ridge.toml"),
        "[workspace]\nname = \"ridge-stdlib-tier\"\nversion = \"0.1.0\"\nmembers = [\"ridge-stdlib\"]\n",
    )?;

    // Project manifest.
    write_str(
        &proj_dir.join("ridge.toml"),
        "[project]\nname = \"std\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"std.**\"]\n",
    )?;

    // Copy each module's source file into the temp workspace.
    for module in modules {
        // Derive the source path relative to src/ from the module name.
        // e.g. "std.int" → "int.ridge", "std.net.http" → "net/http.ridge"
        let rel = module_path(&module.name);
        let dest = proj_dir.join("src").join(&rel);

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create dirs for {}: {e}", dest.display()))?;
        }

        std::fs::copy(&module.path, &dest).map_err(|e| {
            format!(
                "could not copy {} → {}: {e}",
                module.path.display(),
                dest.display()
            )
        })?;
    }

    Ok(tmp_dir)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_str(path: &Path, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Validate every `@ffi` declaration in a tier against the closed-list audit
/// table, returning a T204 build error on the first diagnostic.
///
/// Parses the tier's modules through the resolver's module-graph pass to reach
/// the `FnDecl` nodes, collects the `@ffi`-decorated ones, and runs
/// [`crate::ffi_validator::validate_ffi_decls`] over them. A non-empty result
/// means a stdlib `@ffi` drifted out of the audit table (unknown target,
/// wrong arity, or a missing capability) and the build must stop.
fn validate_tier_ffi(
    tier: u32,
    modules: &[&DiscoveredModule],
    ws_graph: &ridge_resolve::WorkspaceGraph,
) -> Result<(), BuildError> {
    let graph = ridge_resolve::build_module_graph(ws_graph);

    let mut ffi_decls: Vec<&ridge_ast::FnDecl> = Vec::new();
    for parsed in &graph.modules {
        for item in &parsed.ast.items {
            if let ridge_ast::Item::Fn(decl) = item {
                if matches!(decl.body, ridge_ast::Body::Ffi { .. }) {
                    ffi_decls.push(decl);
                }
            }
        }
    }

    let diags = crate::ffi_validator::validate_ffi_decls(&ffi_decls);
    if let Some(first) = diags.first() {
        let (mod_name, mod_path) = first_module_label(modules);
        return Err(BuildError::TierBuildFailed {
            tier,
            module: mod_name,
            path: mod_path,
            source: format!("{} invalid stdlib @ffi declaration", first.code()),
        });
    }

    Ok(())
}

/// Build a T204 `BuildError` from a `ResolveError`.
fn error_from_resolve(tier: u32, modules: &[&DiscoveredModule], err: &ResolveError) -> BuildError {
    let (mod_name, mod_path) = first_module_label(modules);
    BuildError::TierBuildFailed {
        tier,
        module: mod_name,
        path: mod_path,
        source: err.to_string(),
    }
}

/// Return the (name, path) of the first module in the list for error labelling.
fn first_module_label(modules: &[&DiscoveredModule]) -> (String, String) {
    modules.first().map_or_else(
        || ("<unknown>".to_owned(), "<unknown>".to_owned()),
        |m| (m.name.clone(), m.path.display().to_string()),
    )
}
