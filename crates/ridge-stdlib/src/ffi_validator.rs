//! FFI validation: T001–T004 diagnostic checks for `@ffi`-decorated decls.
//!
//! [`validate_ffi_decls`] is the public entry point consumed by the T4 build
//! driver.  It checks every `@ffi` decl in a module against the closed-list
//! audit table and the declared Ridge signature, and returns a vector of
//! [`FfiDiag`] values — one per violation.
//!
//! Error codes:
//! - `T001 FfiArityMismatch`   — declared Ridge param count ≠ `@ffi` arity.
//! - `T002 FfiCapabilityMismatch` — BEAM target needs cap `c`; Ridge decl missing `c`.
//! - `T004 FfiTargetUnknown`   — BEAM `module:name/arity` not in audit table.
//!
//! `T003 FfiOutsideStdlib` is handled by `ridge-resolve` (`R022`) and is
//! therefore absent from this enum.

use ridge_ast::{Body, Capability, FnDecl, Span};

use crate::ffi_caps_audit::{lookup, FfiAuditEntry};

// ── FfiDiag ───────────────────────────────────────────────────────────────────

/// A diagnostic produced by [`validate_ffi_decls`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfiDiag {
    /// T001 — the `@ffi` arity doesn't match the Ridge parameter count.
    FfiArityMismatch {
        /// BEAM module the `@ffi` points at.
        beam_module: String,
        /// BEAM function name.
        beam_fn: String,
        /// Arity declared in the `@ffi` attribute.
        ffi_arity: u32,
        /// Number of parameters on the Ridge `fn` declaration.
        ridge_params: usize,
        /// Span of the Ridge function-name identifier.
        span: Span,
    },

    /// T002 — the Ridge decl is missing a capability that the BEAM target requires.
    FfiCapabilityMismatch {
        /// BEAM module the `@ffi` points at.
        beam_module: String,
        /// BEAM function name.
        beam_fn: String,
        /// The capability that is required but not declared.
        required_cap: Capability,
        /// Span of the Ridge function-name identifier.
        span: Span,
    },

    /// T004 — the BEAM `module:name/arity` triplet is not in the audit table.
    FfiTargetUnknown {
        /// BEAM module from the `@ffi` attribute.
        beam_module: String,
        /// Function name from the `@ffi` attribute.
        beam_fn: String,
        /// Arity from the `@ffi` attribute.
        arity: u32,
        /// Span of the Ridge function-name identifier.
        span: Span,
    },
}

impl FfiDiag {
    /// Stable error code string.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::FfiArityMismatch { .. } => "T001",
            Self::FfiCapabilityMismatch { .. } => "T002",
            Self::FfiTargetUnknown { .. } => "T004",
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Validate all `@ffi`-decorated `FnDecl`s in `decls`.
///
/// Returns one [`FfiDiag`] per violation; order matches `decls` order.
/// Non-`@ffi` decls are silently skipped.
#[must_use]
pub fn validate_ffi_decls(decls: &[&FnDecl]) -> Vec<FfiDiag> {
    let mut out = Vec::new();
    for decl in decls {
        if let Body::Ffi {
            module,
            name,
            arity,
        } = &decl.body
        {
            validate_one(decl, module, name, *arity, &mut out);
        }
    }
    out
}

// ── Internal ──────────────────────────────────────────────────────────────────

fn validate_one(
    decl: &FnDecl,
    beam_module: &str,
    beam_fn: &str,
    ffi_arity: u32,
    out: &mut Vec<FfiDiag>,
) {
    let span = decl.name.span;
    let ridge_params = decl.params.len();

    // T004 — unknown target.
    let Some(entry) = lookup(beam_module, beam_fn, ffi_arity) else {
        out.push(FfiDiag::FfiTargetUnknown {
            beam_module: beam_module.to_owned(),
            beam_fn: beam_fn.to_owned(),
            arity: ffi_arity,
            span,
        });
        // Cannot check T001/T002 without a valid entry; early-return.
        return;
    };

    // T001 — arity mismatch.
    #[allow(clippy::cast_possible_truncation)]
    if ridge_params as u32 != ffi_arity {
        out.push(FfiDiag::FfiArityMismatch {
            beam_module: beam_module.to_owned(),
            beam_fn: beam_fn.to_owned(),
            ffi_arity,
            ridge_params,
            span,
        });
    }

    // T002 — capability mismatch.
    check_cap_requirements(decl, entry, span, out);
}

fn check_cap_requirements(
    decl: &FnDecl,
    entry: &FfiAuditEntry,
    span: Span,
    out: &mut Vec<FfiDiag>,
) {
    for &required in entry.requires_caps {
        if !decl.caps.contains(&required) {
            out.push(FfiDiag::FfiCapabilityMismatch {
                beam_module: entry.beam_module.to_owned(),
                beam_fn: entry.fn_name.to_owned(),
                required_cap: required,
                span,
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ridge_ast::{Body, Capability, FnDecl, Ident, Param, Span, Visibility};

    use super::*;

    fn sp() -> Span {
        Span::point(0)
    }

    fn make_ident(name: &str) -> Ident {
        Ident {
            text: name.to_owned(),
            span: sp(),
        }
    }

    fn bare_param(name: &str) -> Param {
        Param::Bare(make_ident(name))
    }

    fn ffi_decl(
        caps: Vec<Capability>,
        params: Vec<Param>,
        module: &str,
        fn_name: &str,
        arity: u32,
    ) -> FnDecl {
        FnDecl {
            attrs: vec![],
            vis: Visibility::Pub,
            caps,
            name: make_ident("testFn"),
            params,
            ret: None,
            constraints: vec![],
            body: Body::Ffi {
                module: module.to_owned(),
                name: fn_name.to_owned(),
                arity,
            },
            span: sp(),
            doc: None,
        }
    }

    // ── T001 FfiArityMismatch ─────────────────────────────────────────────────

    #[test]
    fn t001_arity_mismatch_fires_when_param_count_differs() {
        // erlang:+/2 expects 2 params, but we declare only 1.
        let decl = ffi_decl(vec![], vec![bare_param("a")], "erlang", "+", 2);
        let diags = validate_ffi_decls(&[&decl]);
        assert_eq!(diags.len(), 1, "expected exactly one diagnostic");
        assert_eq!(diags[0].code(), "T001");
        assert!(matches!(
            diags[0],
            FfiDiag::FfiArityMismatch {
                ffi_arity: 2,
                ridge_params: 1,
                ..
            }
        ));
    }

    #[test]
    fn t001_no_diagnostic_when_arity_matches() {
        let decl = ffi_decl(
            vec![],
            vec![bare_param("a"), bare_param("b")],
            "erlang",
            "+",
            2,
        );
        let diags = validate_ffi_decls(&[&decl]);
        assert!(
            diags.iter().all(|d| d.code() != "T001"),
            "must not fire T001 when arity matches"
        );
    }

    // ── T002 FfiCapabilityMismatch ────────────────────────────────────────────

    #[test]
    fn t002_fires_when_required_cap_not_declared() {
        // ridge_rt:println/1 requires io, but we declare no caps.
        let decl = ffi_decl(vec![], vec![bare_param("s")], "ridge_rt", "println", 1);
        let diags = validate_ffi_decls(&[&decl]);
        let t002: Vec<_> = diags.iter().filter(|d| d.code() == "T002").collect();
        assert_eq!(t002.len(), 1, "expected one T002");
        assert!(matches!(
            t002[0],
            FfiDiag::FfiCapabilityMismatch {
                required_cap: Capability::Io,
                ..
            }
        ));
    }

    #[test]
    fn t002_no_diagnostic_when_cap_is_declared() {
        // ridge_rt:println/1 requires io, and we declare io.
        let decl = ffi_decl(
            vec![Capability::Io],
            vec![bare_param("s")],
            "ridge_rt",
            "println",
            1,
        );
        let diags = validate_ffi_decls(&[&decl]);
        assert!(
            diags.iter().all(|d| d.code() != "T002"),
            "must not fire T002 when required cap is declared"
        );
    }

    #[test]
    fn t002_no_diagnostic_for_pure_target() {
        // erlang:+/2 requires no capabilities.
        let decl = ffi_decl(
            vec![],
            vec![bare_param("a"), bare_param("b")],
            "erlang",
            "+",
            2,
        );
        let diags = validate_ffi_decls(&[&decl]);
        assert!(
            diags.iter().all(|d| d.code() != "T002"),
            "pure target must not fire T002"
        );
    }

    // ── T004 FfiTargetUnknown ─────────────────────────────────────────────────

    #[test]
    fn t004_fires_for_unknown_beam_target() {
        let decl = ffi_decl(
            vec![],
            vec![bare_param("x")],
            "some_user_lib",
            "dangerous_fn",
            1,
        );
        let diags = validate_ffi_decls(&[&decl]);
        assert_eq!(diags.len(), 1, "expected exactly one diagnostic");
        assert_eq!(diags[0].code(), "T004");
        assert!(matches!(
            &diags[0],
            FfiDiag::FfiTargetUnknown { beam_module, beam_fn, arity: 1, .. }
            if beam_module == "some_user_lib" && beam_fn == "dangerous_fn"
        ));
    }

    #[test]
    fn t004_no_diagnostic_for_known_target() {
        // lists:map/2 is in the audit table.
        let decl = ffi_decl(
            vec![],
            vec![bare_param("f"), bare_param("xs")],
            "lists",
            "map",
            2,
        );
        let diags = validate_ffi_decls(&[&decl]);
        assert!(
            diags.iter().all(|d| d.code() != "T004"),
            "known target must not fire T004"
        );
    }

    #[test]
    fn t004_suppresses_t001_and_t002() {
        // When the target is unknown, T001/T002 must NOT fire (no audit entry
        // means no reference for arity or capability checks).
        let decl = ffi_decl(vec![], vec![], "ghost_lib", "ghost_fn", 99);
        let diags = validate_ffi_decls(&[&decl]);
        // Only T004 should appear; T001/T002 must not.
        assert!(diags.iter().all(|d| d.code() == "T004"));
        assert_eq!(diags.len(), 1);
    }

    // ── Non-@ffi decls are skipped ────────────────────────────────────────────

    #[test]
    fn non_ffi_decls_produce_no_diagnostics() {
        let decl = FnDecl {
            attrs: vec![],
            vis: Visibility::Pub,
            caps: vec![],
            name: make_ident("pureFunc"),
            params: vec![bare_param("x")],
            ret: None,
            constraints: vec![],
            body: Body::Expr(ridge_ast::Expr::Unit(sp())),
            span: sp(),
            doc: None,
        };
        let diags = validate_ffi_decls(&[&decl]);
        assert!(diags.is_empty());
    }

    // ── Code stability ────────────────────────────────────────────────────────

    #[test]
    fn t001_code_is_stable() {
        let d = FfiDiag::FfiArityMismatch {
            beam_module: "erlang".into(),
            beam_fn: "+".into(),
            ffi_arity: 2,
            ridge_params: 1,
            span: sp(),
        };
        assert_eq!(d.code(), "T001");
    }

    #[test]
    fn t002_code_is_stable() {
        let d = FfiDiag::FfiCapabilityMismatch {
            beam_module: "ridge_rt".into(),
            beam_fn: "println".into(),
            required_cap: Capability::Io,
            span: sp(),
        };
        assert_eq!(d.code(), "T002");
    }

    #[test]
    fn t004_code_is_stable() {
        let d = FfiDiag::FfiTargetUnknown {
            beam_module: "ghost".into(),
            beam_fn: "fn".into(),
            arity: 0,
            span: sp(),
        };
        assert_eq!(d.code(), "T004");
    }
}
