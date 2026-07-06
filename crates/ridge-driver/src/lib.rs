//! Compilation orchestration for the Ridge compiler.
//!
//! `ridge-driver` is the single entry point that CLI, LSP, and the Phase-6
//! BEAM-runtime test harness all consume.  It wires
//! `ridge-resolve → ridge-typecheck → ridge-lower → ridge-codegen-erl` into
//! three public functions:
//!
//! - [`compile_workspace`] — full pipeline, produces `.beam` / `.core`.
//! - [`check_workspace`] — stops after typecheck, no codegen.
//! - [`run_workspace`] — `compile_workspace` + `erl -s <module> start`.
//!
//! # Hard constraints (§1.3)
//!
//! - No `panic!` / `unwrap` / `expect` on user-input paths (§1.3 #4).
//!   Every user-reachable error path returns a structured error.
//! - Cross-platform paths via [`std::path::PathBuf::join`] only (§1.3 #5).
//! - Output dir: `<workspace_root>/target/ridge/<profile>/`.

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod check;
pub mod compile;
pub mod diag_adapters;
pub mod error;
pub mod incremental;
pub mod options;
pub mod run;
pub mod sources;

// ── Flat re-exports ───────────────────────────────────────────────────────────

pub use check::{
    check_standalone_incremental, check_workspace, check_workspace_incremental,
    check_workspace_typed, collect_diagnostics, CheckArtefacts, CheckTypedArtefacts,
};
pub use compile::{compile_workspace, write_stdlib_test_workspace, CompileArtefacts, SourceMap};
pub use error::{CheckError, CompileDiagnostics, CompileError, ProcessExitCode, RunError};
pub use incremental::IncrementalState;
pub use options::{CheckOptions, CompileOptions, EmitArtefacts, Profile, RunOptions};
pub use run::run_workspace;
pub use sources::WorkspaceSourceCache;

// Re-export typed workspace types so `ridge-cli` doesn't need a direct dep on
// `ridge-typecheck` (T9 — test runner needs TypedWorkspace + TypedModule).
pub use ridge_typecheck::{TypedModule, TypedWorkspace};

// Re-export workspace graph metadata so `ridge-cli` can map ModuleId → file
// path without a direct `ridge-resolve` dep (T9 — test beam module naming).
pub use ridge_resolve::{ModuleId, ModuleMetadata, WorkspaceGraph};

// Re-export AST types used by `ridge test` for test-function introspection,
// so `ridge-cli` avoids a direct dep on `ridge-ast` (T9).
pub use ridge_ast::{
    Attribute as AstAttribute, Capability as AstCapability, Item as AstItem, PrimitiveType,
    Type as AstType, Visibility,
};
