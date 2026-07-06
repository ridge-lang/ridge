//! Driver option types for `compile_workspace`, `check_workspace`, and
//! `run_workspace`.

use std::path::PathBuf;

// ── Profile ───────────────────────────────────────────────────────────────────

/// Build-profile selector.
///
/// Controls the output subdirectory (`target/ridge/debug/` vs
/// `target/ridge/release/`) and BEAM optimisation flags passed to `erlc`.
///
/// Controls output subdirectory and `erlc` flags for debug vs release builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Profile {
    /// Debug build — `target/ridge/debug/`, `erlc +debug_info`.
    #[default]
    Debug,
    /// Release build — `target/ridge/release/`, `erlc +bin_opt_info`.
    Release,
}

impl Profile {
    /// Return the profile directory name used inside `target/ridge/<name>/`.
    #[must_use]
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

// ── EmitArtefacts ─────────────────────────────────────────────────────────────

/// Which artefacts to emit during compilation.
///
/// Controls whether `.beam` files, `.core` files, or both are written to the
/// output directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum EmitArtefacts {
    /// Emit `.beam` files only (default).
    #[default]
    Beam,
    /// Emit `.core` (Core Erlang text) files only — no BEAM compilation.
    Core,
    /// Emit both `.core` and `.beam` files.
    Both,
}

impl EmitArtefacts {
    /// Returns `true` if BEAM files should be produced.
    #[must_use]
    pub const fn emit_beam(self) -> bool {
        matches!(self, Self::Beam | Self::Both)
    }

    /// Returns `true` if Core Erlang files should be written.
    #[must_use]
    pub const fn emit_core(self) -> bool {
        matches!(self, Self::Core | Self::Both)
    }
}

// ── CompileOptions ────────────────────────────────────────────────────────────

/// Options for [`crate::compile_workspace`].
///
/// Output directory: `<workspace_root>/target/ridge/<profile>/<member>/`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CompileOptions {
    /// Absolute path to the workspace root directory (the directory containing
    /// the root `ridge.toml` with a `[workspace]` table).
    pub workspace_root: PathBuf,

    /// Optional filter: only compile the named members.  When `None`, every
    /// member in the workspace is compiled.
    pub members: Option<Vec<String>>,

    /// Build profile — controls output subdirectory and `erlc` flags.
    pub profile: Profile,

    /// Which artefacts to emit.
    pub emit: EmitArtefacts,

    /// Compile the workspace as the Ridge standard library itself (permits
    /// `@ffi`, takes reconciled types from source). INTERNAL — see the identical
    /// field on [`CheckOptions`]; set only by the stdlib build paths and
    /// `ridge test --stdlib`, never from user-facing input.
    pub is_stdlib: bool,

    /// Optional cache-root override for `ridge-pkg`.
    ///
    /// When `None`, the driver calls [`ridge_pkg::cache_root`] to resolve the
    /// platform-default location (e.g. `~/.ridge/cache` on Linux).  Tests
    /// supply an explicit path pointing at a [`tempfile::TempDir`] so they do
    /// not pollute the user's cache and can assert the cache contents
    /// deterministically.  (T8 / G5)
    pub cache_root: Option<PathBuf>,
}

impl CompileOptions {
    /// Construct a minimal set of options for the given workspace root.
    ///
    /// Uses `Profile::Debug` and `EmitArtefacts::Beam` as defaults.
    /// `cache_root` defaults to `None` (use platform default).
    #[must_use]
    pub const fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            members: None,
            profile: Profile::Debug,
            emit: EmitArtefacts::Beam,
            cache_root: None,
            is_stdlib: false,
        }
    }

    /// Set the emit mode and return `self` (builder style).
    #[must_use]
    pub const fn with_emit(mut self, emit: EmitArtefacts) -> Self {
        self.emit = emit;
        self
    }

    /// Set the profile and return `self` (builder style).
    #[must_use]
    pub const fn with_profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        self
    }

    /// Override the `ridge-pkg` cache root for this compile invocation.
    ///
    /// Used by tests to redirect cache writes to a temporary directory so they
    /// do not pollute the developer's global Ridge package cache.
    #[must_use]
    pub fn with_cache_root(mut self, cache_root: PathBuf) -> Self {
        self.cache_root = Some(cache_root);
        self
    }
}

// ── CheckOptions ──────────────────────────────────────────────────────────────

/// Options for [`crate::check_workspace`].
///
/// Like [`CompileOptions`] but stops after type-checking — no lowering, no
/// codegen, no BEAM files produced.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CheckOptions {
    /// Absolute path to the workspace root directory.
    pub workspace_root: PathBuf,

    /// Optional filter: only check the named members.  When `None`, every
    /// member is checked.
    pub members: Option<Vec<String>>,

    /// Retain per-module index data (scope trees) for editor queries.
    ///
    /// The batch compiler (`ridge check` / `ridge build`) leaves this `false` so
    /// it does not pay to materialise data it never reads. The language server
    /// sets it `true` to power hover, go-to-definition, and completion.
    pub retain_indices: bool,

    /// Compile the workspace as the Ridge standard library itself.
    ///
    /// When `true`, `@ffi` is permitted (R022 is not raised) and the reconciled
    /// stdlib types / base codec instances are taken from the `.ridge` source
    /// rather than reserved as builtins, so the stdlib can compile itself without
    /// double-declaring them. This is an INTERNAL flag: it is set only by the
    /// compiler's own stdlib build paths and by `ridge test --stdlib` (which
    /// compiles the embedded stdlib, never user code), never from a user-facing
    /// CLI flag — otherwise a user could enable `@ffi` and bypass the capability
    /// system.
    pub is_stdlib: bool,
}

impl CheckOptions {
    /// Construct check options for the given workspace root.
    #[must_use]
    pub const fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            members: None,
            retain_indices: false,
            is_stdlib: false,
        }
    }

    /// Enable or disable retention of per-module index data and return `self`
    /// (builder style).
    #[must_use]
    pub const fn with_retain_indices(mut self, retain: bool) -> Self {
        self.retain_indices = retain;
        self
    }
}

// ── RunOptions ────────────────────────────────────────────────────────────────

/// Options for [`crate::run_workspace`].
///
/// Compiles the workspace then invokes
/// `erl -pa target/ridge/<profile>/<member>/beam -s <main_module> <entry_fn> -s init stop -noshell`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunOptions {
    /// Absolute path to the workspace root directory.
    pub workspace_root: PathBuf,

    /// Build profile.
    pub profile: Profile,

    /// The entry-point member name (must match a workspace member).
    pub main_member: String,

    /// The BEAM module name to invoke (passed to `-s <module> <entry_fn>`).
    ///
    /// Defaults to the mangled name of the first compiled module when `None`.
    pub main_module: Option<String>,

    /// The BEAM function name to invoke on `main_module` (passed to
    /// `-s <module> <entry_fn>`).
    ///
    /// Defaults to `"main"` when `None`, matching the scaffold's `pub fn main`
    /// entry point.
    pub entry_fn: Option<String>,

    /// Extra arguments passed after `-extra` to the BEAM node.
    pub extra_args: Vec<String>,
}

impl RunOptions {
    /// Construct run options with the minimum required fields.
    #[must_use]
    pub const fn new(workspace_root: PathBuf, main_member: String) -> Self {
        Self {
            workspace_root,
            profile: Profile::Debug,
            main_member,
            main_module: None,
            entry_fn: None,
            extra_args: Vec::new(),
        }
    }
}
