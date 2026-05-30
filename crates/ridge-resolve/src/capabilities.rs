//! Capability-keyword binding + project allow/deny enforcement (plan §4.7).
//!
//! # Overview
//!
//! [`check_capabilities`] walks every capability position in a parsed
//! [`ridge_ast::Module`] and enforces the allow/deny rules from the project and
//! workspace manifests:
//!
//! - **R015 `CapabilityDenied`** — the capability appears in
//!   `workspace.capabilities_deny` **or** `project.capabilities_deny`.  The
//!   `denied_at` string records which manifest issued the denial (`"workspace"`
//!   or the project name).
//!
//! - **R016 `CapabilityNotAllowed`** — the project declares
//!   `capabilities_allow = […]` (whitelist mode) and the capability is not in
//!   that list.
//!
//! - **R019 `UnknownCapabilityKeyword`** — defensive; the `Capability` enum is
//!   closed (9 variants: `io`, `fs`, `net`, `time`, `random`, `env`, `proc`,
//!   `spawn`, `ffi`) and the parser maps every recognised keyword to a variant
//!   before this pass runs.  In 0.1.0 there is no code path in the parser that
//!   would produce an unrecognised capability, so this variant is unreachable
//!   from normal input.  **No test fixture is provided.**
//!
//! - **R020 `CapabilityListOnWrongDecl`** — defensive; same reasoning as R019.
//!   The grammar only permits capability lists on `fn`, `init`, and `on`
//!   declarations; the parser enforces this and will not attach a capability
//!   list to any other AST node.  **No test fixture is provided.**
//!
//! # Span choice
//!
//! `Capability` is a plain `Copy` enum — it carries **no source span**.  The
//! parallel `Vec<Capability>` stored on each declaration likewise has no
//! per-element spans.  Rather than add a `Vec<Span>` to the AST (which would
//! require modifying the parser), this pass uses
//! the enclosing declaration's span as the diagnostic span:
//!
//! - `FnDecl::name.span` for top-level functions.
//! - `InitDecl::span` for `init` blocks.
//! - `OnHandler::name.span` for `on` handlers.
//! - `FnType::span` for capability-annotated function types in type annotations.
//!
//! This points the error at the declaration name rather than the individual
//! capability keyword.  Phase 4 may refine this if per-cap spans are added to
//! the AST.
//!
//! # Integration
//!
//! This pass ships as a standalone function.  It is wired into
//! `resolve_workspace` via the `check_capabilities` call per module.

use ridge_ast::{ActorMember, Capability, FnType, Item, Module, Span, Type};

use crate::{
    error::ResolveError,
    manifest::{Project, WorkspaceManifest},
};

// ── Public entry point ────────────────────────────────────────────────────────

/// Check capability annotations in `module` against the project and workspace
/// allow/deny lists.
///
/// Errors are appended to `errors`.  The caller owns the vector; this function
/// never clears it.
///
/// # Arguments
///
/// - `module` — the parsed module to inspect.
/// - `project` — the owning project's manifest.
/// - `workspace` — the workspace-level manifest (for workspace-wide deny).
///
/// # Errors emitted
///
/// - [`ResolveError::CapabilityDenied`] (`R015`) for each capability that
///   appears in `workspace.capabilities_deny` or `project.capabilities_deny`.
/// - [`ResolveError::CapabilityNotAllowed`] (`R016`) when the project has an
///   explicit `capabilities_allow` list and the capability is not in it.
pub fn check_capabilities(
    module: &Module,
    project: &Project,
    workspace: &WorkspaceManifest,
    errors: &mut Vec<ResolveError>,
) {
    for item in &module.items {
        check_item(item, project, workspace, errors);
    }
}

// ── Item dispatch ─────────────────────────────────────────────────────────────

fn check_item(
    item: &Item,
    project: &Project,
    workspace: &WorkspaceManifest,
    errors: &mut Vec<ResolveError>,
) {
    match item {
        Item::Fn(decl) => {
            // Check caps declared directly on the fn.
            check_caps(&decl.caps, decl.name.span, project, workspace, errors);
            // Walk parameter types and return type for inner fn-type caps.
            for param in &decl.params {
                if let ridge_ast::Param::Annotated { ty, .. } = param {
                    check_type(ty, project, workspace, errors);
                }
            }
            if let Some(ret) = &decl.ret {
                check_type(ret, project, workspace, errors);
            }
        }
        Item::Actor(decl) => {
            for member in &decl.members {
                match member {
                    ActorMember::Init(init) => {
                        check_caps(&init.caps, init.span, project, workspace, errors);
                        // Walk parameter types for inner fn-type caps.
                        for param in &init.params {
                            if let ridge_ast::Param::Annotated { ty, .. } = param {
                                check_type(ty, project, workspace, errors);
                            }
                        }
                    }
                    ActorMember::On(handler) => {
                        check_caps(&handler.caps, handler.name.span, project, workspace, errors);
                        // Walk parameter types and return type for inner fn-type caps.
                        for param in &handler.params {
                            if let ridge_ast::Param::Annotated { ty, .. } = param {
                                check_type(ty, project, workspace, errors);
                            }
                        }
                        if let Some(ret) = &handler.ret {
                            check_type(ret, project, workspace, errors);
                        }
                    }
                    ActorMember::State(state) => {
                        check_type(&state.ty, project, workspace, errors);
                    }
                    ActorMember::Mailbox(_) => {
                        // The mailbox member carries only the bound (an `i64`)
                        // and a policy enum: no capabilities, no types, no
                        // identifiers to resolve.
                    }
                }
            }
        }
        // Import, Const, and Type declarations cannot bear capability lists
        // (grammar invariant); nothing to check at this level.  However, type
        // aliases or field types within a TypeDecl might embed `fn io …` types,
        // so walk them too.
        Item::Type(decl) => check_type_body(&decl.body, project, workspace, errors),
        Item::Const(decl) => check_type(&decl.ty, project, workspace, errors),
        Item::Import(_) => {}
    }
}

// ── Type traversal ────────────────────────────────────────────────────────────

/// Walk a type expression, checking any embedded `Type::Fn` capability lists.
fn check_type(
    ty: &Type,
    project: &Project,
    workspace: &WorkspaceManifest,
    errors: &mut Vec<ResolveError>,
) {
    match ty {
        Type::Fn { fn_ty, .. } => check_fn_type(fn_ty, project, workspace, errors),
        Type::App { args, .. } => {
            for arg in args {
                check_type(arg, project, workspace, errors);
            }
        }
        Type::Tuple { elems, .. } => {
            for elem in elems {
                check_type(elem, project, workspace, errors);
            }
        }
        Type::List { elem, .. } => check_type(elem, project, workspace, errors),
        Type::Paren { inner, .. } => check_type(inner, project, workspace, errors),
        // Primitive, Named, Var — no caps, no children.
        Type::Primitive { .. } | Type::Named { .. } | Type::Var { .. } => {}
        // TODO(0.2.12): recurse into inline record field types for cap checking.
        Type::Record { fields, .. } => {
            for field in fields {
                check_type(&field.ty, project, workspace, errors);
            }
        }
    }
}

/// Check a `fn`-type node: its capability list, then recursively its param /
/// return types.
fn check_fn_type(
    fn_ty: &FnType,
    project: &Project,
    workspace: &WorkspaceManifest,
    errors: &mut Vec<ResolveError>,
) {
    // Use the FnType's own span as the diagnostic span for its caps.
    check_caps(&fn_ty.caps, fn_ty.span, project, workspace, errors);
    for param in &fn_ty.params {
        check_type(param, project, workspace, errors);
    }
    check_type(&fn_ty.ret, project, workspace, errors);
}

/// Walk a type body (record / union / alias).
fn check_type_body(
    body: &ridge_ast::TypeBody,
    project: &Project,
    workspace: &WorkspaceManifest,
    errors: &mut Vec<ResolveError>,
) {
    match body {
        ridge_ast::TypeBody::Alias(ty) => check_type(ty, project, workspace, errors),
        ridge_ast::TypeBody::Record(rb) => {
            for field in &rb.fields {
                check_type(&field.ty, project, workspace, errors);
            }
        }
        ridge_ast::TypeBody::Union(ub) => {
            for alt in &ub.alternatives {
                match alt {
                    ridge_ast::Constructor::Positional { args, .. } => {
                        for arg in args {
                            check_type(arg, project, workspace, errors);
                        }
                    }
                    ridge_ast::Constructor::Record { body, .. } => {
                        for field in &body.fields {
                            check_type(&field.ty, project, workspace, errors);
                        }
                    }
                }
            }
        }
    }
}

// ── Core enforcement ──────────────────────────────────────────────────────────

/// Check each capability in `caps` against the project and workspace manifests.
///
/// `decl_span` is the enclosing declaration span used for diagnostics (see
/// module-level rustdoc for the span-choice rationale).
fn check_caps(
    caps: &[Capability],
    decl_span: Span,
    project: &Project,
    workspace: &WorkspaceManifest,
    errors: &mut Vec<ResolveError>,
) {
    for &cap in caps {
        // R015 — workspace deny takes priority.
        if workspace.capabilities_deny.contains(&cap) {
            errors.push(ResolveError::CapabilityDenied {
                cap,
                denied_at: "workspace".to_owned(),
                span: decl_span,
            });
            // Still check project deny and allow below — a cap can appear in
            // both deny lists, emitting two diagnostics is valid (each list is
            // independent).
        }

        // R015 — project-level deny.
        if project.capabilities_deny.contains(&cap) {
            errors.push(ResolveError::CapabilityDenied {
                cap,
                denied_at: project.name.clone(),
                span: decl_span,
            });
        }

        // R016 — project whitelist (only when capabilities_allow is Some).
        if let Some(allow_list) = &project.capabilities_allow {
            if !allow_list.contains(&cap) {
                errors.push(ResolveError::CapabilityNotAllowed {
                    cap,
                    project: project.name.clone(),
                    span: decl_span,
                });
            }
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ridge_ast::Capability;
    use ridge_parser::parse_source;

    use crate::{
        capabilities::check_capabilities,
        error::ResolveError,
        manifest::{Project, ProjectKind, WorkspaceManifest},
        ProjectId,
    };
    use std::path::PathBuf;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Build a minimal `WorkspaceManifest` with the given workspace-level deny
    /// list.
    fn workspace(deny: Vec<Capability>) -> WorkspaceManifest {
        WorkspaceManifest {
            name: "test-ws".to_owned(),
            version: "0.1.0".to_owned(),
            members_globs: vec![],
            dependencies: vec![],
            forbid_rules: vec![],
            capabilities_deny: deny,
            source_path: PathBuf::from("ridge.toml"),
        }
    }

    /// Build a minimal `Project` with optional allow/deny lists.
    fn project(name: &str, allow: Option<Vec<Capability>>, deny: Vec<Capability>) -> Project {
        Project {
            id: ProjectId(0),
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            kind: ProjectKind::Library,
            manifest_path: PathBuf::from("ridge.toml"),
            src_root: PathBuf::from("src"),
            exports_public: vec![],
            exports_internal: vec![],
            dependencies: vec![],
            capabilities_allow: allow,
            capabilities_deny: deny,
        }
    }

    /// Parse `src` and run the capability pass; return all errors.
    fn check(src: &str, proj: &Project, ws: &WorkspaceManifest) -> Vec<ResolveError> {
        let result = parse_source(src);
        assert!(
            result.errors.is_empty(),
            "parse errors: {:?}",
            result.errors
        );
        let mut errors = Vec::new();
        check_capabilities(&result.module, proj, ws, &mut errors);
        errors
    }

    // ── Tests 1–8 ─────────────────────────────────────────────────────────────

    /// Test 1 — no allow/deny configured → no errors.
    #[test]
    fn t1_no_deny_no_allow_is_ok() {
        let proj = project("demo", None, vec![]);
        let ws = workspace(vec![]);
        let errs = check("fn io fs main () = ()", &proj, &ws);
        assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
    }

    /// Test 2 — capability denied at project level → exactly one R015.
    #[test]
    fn t2_project_deny_emits_r015() {
        let proj = project("demo", None, vec![Capability::Ffi]);
        let ws = workspace(vec![]);
        let errs = check("fn ffi foo () = ()", &proj, &ws);
        assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
        match &errs[0] {
            ResolveError::CapabilityDenied { cap, denied_at, .. } => {
                assert_eq!(*cap, Capability::Ffi);
                assert_eq!(denied_at, "demo");
            }
            other => panic!("expected CapabilityDenied, got: {other:?}"),
        }
    }

    /// Test 3 — capability denied at workspace level → exactly one R015.
    #[test]
    fn t3_workspace_deny_emits_r015() {
        let proj = project("demo", None, vec![]);
        let ws = workspace(vec![Capability::Net]);
        let errs = check("fn net foo () = ()", &proj, &ws);
        assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
        match &errs[0] {
            ResolveError::CapabilityDenied { cap, denied_at, .. } => {
                assert_eq!(*cap, Capability::Net);
                assert_eq!(denied_at, "workspace");
            }
            other => panic!("expected CapabilityDenied, got: {other:?}"),
        }
    }

    /// Test 4 — project has allow list and cap is not in it → exactly one R016.
    #[test]
    fn t4_not_in_allow_list_emits_r016() {
        let proj = project("demo", Some(vec![Capability::Io, Capability::Fs]), vec![]);
        let ws = workspace(vec![]);
        let errs = check("fn net foo () = ()", &proj, &ws);
        assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
        match &errs[0] {
            ResolveError::CapabilityNotAllowed { cap, project, .. } => {
                assert_eq!(*cap, Capability::Net);
                assert_eq!(project, "demo");
            }
            other => panic!("expected CapabilityNotAllowed, got: {other:?}"),
        }
    }

    /// Test 5 — cap is in the allow list → no errors.
    #[test]
    fn t5_in_allow_list_is_ok() {
        let proj = project("demo", Some(vec![Capability::Io]), vec![]);
        let ws = workspace(vec![]);
        let errs = check("fn io foo () = ()", &proj, &ws);
        assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
    }

    /// Test 6 — empty allow list rejects every capability.
    #[test]
    fn t6_empty_allow_list_rejects_all() {
        let proj = project("demo", Some(vec![]), vec![]);
        let ws = workspace(vec![]);
        // fn with two caps → 2× R016
        let errs = check("fn io fs main () = ()", &proj, &ws);
        assert_eq!(errs.len(), 2, "expected 2 errors, got: {errs:?}");
        for e in &errs {
            assert!(
                matches!(e, ResolveError::CapabilityNotAllowed { .. }),
                "expected CapabilityNotAllowed, got: {e:?}"
            );
        }
    }

    /// Test 7 — actor with init + on handler, both denied → 2× R015.
    #[test]
    fn t7_actor_init_and_handler_both_denied() {
        let src = r"
actor X =
    state count: Int = 0
    init time () =
        count <- 0
    on time inc () =
        count <- count + 1
";
        let proj = project("demo", None, vec![Capability::Time]);
        let ws = workspace(vec![]);
        let errs = check(src, &proj, &ws);
        assert_eq!(
            errs.len(),
            2,
            "expected 2 errors (init + handler), got: {errs:?}"
        );
        for e in &errs {
            match e {
                ResolveError::CapabilityDenied { cap, denied_at, .. } => {
                    assert_eq!(*cap, Capability::Time);
                    assert_eq!(denied_at, "demo");
                }
                other => panic!("expected CapabilityDenied, got: {other:?}"),
            }
        }
    }

    /// Test 8 — capability on inner `Type::Fn` in a parameter annotation:
    /// `fn foo (cb: fn ffi Int -> Int) = …` with denied `ffi` → 1× R015.
    #[test]
    fn t8_cap_on_inner_fn_type_is_checked() {
        let src = "fn foo (cb: fn ffi Int -> Int) = cb 0";
        let proj = project("demo", None, vec![Capability::Ffi]);
        let ws = workspace(vec![]);
        let errs = check(src, &proj, &ws);
        assert_eq!(
            errs.len(),
            1,
            "expected 1 error on inner fn-type, got: {errs:?}"
        );
        match &errs[0] {
            ResolveError::CapabilityDenied { cap, denied_at, .. } => {
                assert_eq!(*cap, Capability::Ffi);
                assert_eq!(denied_at, "demo");
            }
            other => panic!("expected CapabilityDenied, got: {other:?}"),
        }
    }

    /// Bonus — R015 `denied_at` distinguishes workspace vs project.
    #[test]
    fn t9_denied_at_label_workspace_vs_project() {
        // Both workspace and project deny `ffi` → 2 R015 errors with different
        // `denied_at` strings.
        let proj = project("myproj", None, vec![Capability::Ffi]);
        let ws = workspace(vec![Capability::Ffi]);
        let errs = check("fn ffi bar () = ()", &proj, &ws);
        assert_eq!(errs.len(), 2, "expected 2 errors, got: {errs:?}");
        let denied_ats: Vec<&str> = errs
            .iter()
            .map(|e| match e {
                ResolveError::CapabilityDenied { denied_at, .. } => denied_at.as_str(),
                other => panic!("expected CapabilityDenied, got: {other:?}"),
            })
            .collect();
        assert!(
            denied_ats.contains(&"workspace"),
            "missing workspace denial"
        );
        assert!(denied_ats.contains(&"myproj"), "missing project denial");
    }

    /// Bonus — `fn io fs main` with no allow/deny → no errors (canonical example
    /// entry points are clean).
    #[test]
    fn t10_io_fs_main_no_deny_is_ok() {
        let proj = project("game", None, vec![]);
        let ws = workspace(vec![]);
        let errs = check("fn io fs main () = ()", &proj, &ws);
        assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
    }
}
