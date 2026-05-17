//! Codegen diagnostic errors — `E###` namespace.
//!
//! `E001`–`E099` are codegen errors, `E101`–`E199` are `erlc` toolchain errors.
//! Severity and recovery guidance is documented per-variant.
//!
//! ## Type-erasure allow-list (§3.10)
//!
//! Six `Type::Error` sites are permitted in the lowered IR without triggering
//! `E007 TypeErasureUnsupportedErrorSite`.  These are Phase-7-stub sites where
//! the type resolver cannot yet produce a concrete type (e.g. `Response`/`Request`
//! from `std.net.http`).  See [`AllowedErrorSite`] and [`audit_type_error_at`].
//!

use ridge_ast::Span;
use ridge_ir::IrNodeId;
use ridge_types::Type;
use std::path::PathBuf;

use ridge_resolve::ModuleId;

// ── Type-erasure allow-list (§3.10) ──────────────────────────────────────────

/// Structural classifier for the six known Phase-7-stub sites where
/// `Type::Error` is permitted to appear without triggering `E007`.
///
/// These sites correspond to six `Type::Error` values:
///
/// - **4×** `IrFn.ret_ty == Type::Error` — fns returning `Response`/`Request`
///   from `std.net.http` (Phase 7 stdlib stub).  All in `url_shortener` examples.
/// - **2×** `IrParam.ty == Type::Error` — params typed `Response`/`Request`
///   (Phase 7 stdlib stubs).  Same examples.
/// - **1×** `IrPat::Wild` carrying `Type::Error` — wildcards bind nothing so
///   their carried type is meaningless at every wildcard site.
///
/// Codegen consults [`audit_type_error_at`] whenever it observes `Type::Error`
/// in the IR.  Sites matching this allow-list are tolerated (defensive lowering —
/// emit `'?<unknown>'` tag, let BEAM detect any runtime mismatch); any other
/// site triggers `E007`.
///
/// # Design note
///
/// We use a structural classifier rather than `IrNodeId`-specific patterns
/// because `NodeId`s are not stable across compilation runs.  The three
/// categories here are stable across runs and across all four example snapshots.
/// The `#[cfg(test)]` variant `SyntheticTestSite` is deliberately excluded from
/// [`ALLOWED_ERROR_SITES`] so tests can exercise the rejection branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedErrorSite {
    /// `IrFn.ret_ty` — function return type unresolved (Phase 7 stdlib stub).
    FnReturnType,
    /// `IrParam.ty` — parameter type unresolved (Phase 7 stdlib stub).
    ParamType,
    /// `IrPat::Wild` — wildcards bind nothing; their carried type is irrelevant.
    WildcardPattern,
    /// Test-only sentinel — never in `ALLOWED_ERROR_SITES`.
    /// Used to exercise the E007 rejection branch in unit tests.
    #[cfg(test)]
    SyntheticTestSite,
}

/// The set of structural IR sites where `Type::Error` is explicitly tolerated.
///
/// Any site **not** in this list that produces a `Type::Error` must trigger
/// `E007 TypeErasureUnsupportedErrorSite` via [`audit_type_error_at`].
pub const ALLOWED_ERROR_SITES: &[AllowedErrorSite] = &[
    AllowedErrorSite::FnReturnType,
    AllowedErrorSite::ParamType,
    AllowedErrorSite::WildcardPattern,
];

/// Audit a `Type::Error` observed during codegen at a specific IR site.
///
/// Returns `None` when the site is in [`ALLOWED_ERROR_SITES`] (tolerated; the
/// caller should apply defensive lowering — e.g. emit `'?<unknown>'` tag).
///
/// Returns `Some(E007)` when the site is **not** in the allow-list, meaning a
/// `Type::Error` appeared somewhere that Phase 4.5 / Phase 7 stubs do not
/// explain.  The caller should push this error into the module error vector and
/// NOT panic.
///
/// When `ty` is not `Type::Error`, this function always returns `None`
/// (fast-path: only `Type::Error` is audited, all other types are fine).
///
/// # Note on current coverage
///
/// As of T11, codegen does not yet observe `Type::Error` via `node_types[id]`
/// lookups — most codegen paths erase types structurally (§3.10, type-erasure
/// decision).  The audit call-sites are wired at `IrPat::Wild` (pat.rs) because
/// that is the only codegen path that explicitly encounters a typed IR node
/// whose type could legitimately be `Type::Error`.  Other paths use field
/// access (`fn_.ret_ty`, `param.ty`) but swallow the value silently because
/// Core Erlang is dynamically typed.  The allow-list and this function document
/// the contract so T13 can assert each site is within-list when the four
/// example snapshots are shipped.
#[must_use]
pub fn audit_type_error_at(
    ty: &Type,
    site: AllowedErrorSite,
    node: IrNodeId,
    span: Span,
    ir_variant: &'static str,
) -> Option<CodegenError> {
    if !ty.is_error() {
        return None;
    }
    if ALLOWED_ERROR_SITES.contains(&site) {
        return None;
    }
    Some(CodegenError::TypeErasureUnsupportedErrorSite {
        span,
        node,
        ir_variant,
    })
}

/// Codegen-emitted diagnostic.  Severity is encoded in the variant.
///
/// All variants are covered by `#[non_exhaustive]`; match arms must include `_`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CodegenError {
    /// `E001` — IR shape malformed (defensive; Phase 5 invariant violated).
    IrShapeMalformed {
        /// The IR variant name that was malformed.
        variant: &'static str,
        /// Source span of the offending node.
        span: Span,
        /// Human-readable detail.
        detail: String,
    },
    /// `E002` — Stdlib bridge missing for symbol `X`.
    StdlibBridgeMissing {
        /// The Ridge stdlib module name.
        module: String,
        /// The symbol name that has no bridge entry.
        name: String,
        /// Source span.
        span: Span,
    },
    /// `E003` — `erlc` not found on PATH.
    ErlcNotFound {
        /// All paths searched when probing for `erlc`.
        searched_paths: Vec<PathBuf>,
    },
    /// `E004` — `erlc` rejected the emitted `.core` (with stderr surfaced).
    ErlcRejectedInput {
        /// The `.core` file that was passed to `erlc`.
        core_path: PathBuf,
        /// `erlc` stderr output verbatim.
        stderr: String,
        /// `erlc` exit code.
        exit_code: i32,
    },
    /// `E005` — Output directory not writable.
    OutputDirNotWritable {
        /// The directory path that could not be written.
        path: PathBuf,
        /// The underlying OS error message.
        io_err: String,
    },
    /// `E006` — Module name collision (two Ridge modules mangle to the same BEAM module).
    BeamModuleNameCollision {
        /// First module that claims the mangled name.
        left: ModuleId,
        /// Second module that claims the same mangled name.
        right: ModuleId,
        /// The conflicting mangled atom.
        mangled: String,
    },
    /// `E007` — Type-erasure surfaced an unsupported `Type::Error` site outside of
    /// Phase-7-stub regions (defensive — see §3.10).
    TypeErasureUnsupportedErrorSite {
        /// Source span.
        span: Span,
        /// The IR node that triggered this.
        node: IrNodeId,
        /// The IR expression variant name.
        ir_variant: &'static str,
    },
    /// `E008` — Capability erasure audit found a `Capability` token in emitted Core Erlang.
    CapabilityLeakIntoCoreErl {
        /// Source span.
        span: Span,
        /// The leaked capability token text.
        leaked_token: String,
    },
    /// `E101` — `erlc` toolchain version below OTP 26 minimum.
    ErlcVersionTooOld {
        /// The version string `erlc --version` reported.
        found: String,
        /// The required minimum version string.
        minimum: String,
    },
    /// `E102` — `erlc` produced unexpected output (parse error in our `.core`).
    ErlcUnexpectedOutput {
        /// The `.core` file that was passed to `erlc`.
        core_path: PathBuf,
        /// Captured stdout.
        stdout: String,
        /// Captured stderr.
        stderr: String,
    },
}

impl CodegenError {
    /// Return the stable `E###` error code for this variant.
    ///
    /// Codes are **stable across releases** — never renumber an assigned code.
    /// `E001`–`E099` are codegen errors; `E101`–`E199` are `erlc` toolchain errors.
    ///
    /// Approved as a frozen-crate additive exception per FROZEN-02 (2026-05-01).
    /// One `*_code_is_stable` test per variant is required per the FROZEN-02 `DoD`.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::IrShapeMalformed { .. } => "E001",
            Self::StdlibBridgeMissing { .. } => "E002",
            Self::ErlcNotFound { .. } => "E003",
            Self::ErlcRejectedInput { .. } => "E004",
            Self::OutputDirNotWritable { .. } => "E005",
            Self::BeamModuleNameCollision { .. } => "E006",
            Self::TypeErasureUnsupportedErrorSite { .. } => "E007",
            Self::CapabilityLeakIntoCoreErl { .. } => "E008",
            Self::ErlcVersionTooOld { .. } => "E101",
            Self::ErlcUnexpectedOutput { .. } => "E102",
        }
    }

    /// Return the primary [`Span`] associated with this error, if any.
    ///
    /// Toolchain-oriented errors (`E003`–`E006`, `E101`, `E102`) have no
    /// source span and return `None`.  Span-bearing variants return `Some`.
    #[must_use]
    pub const fn span(&self) -> Option<Span> {
        match self {
            Self::IrShapeMalformed { span, .. }
            | Self::StdlibBridgeMissing { span, .. }
            | Self::TypeErasureUnsupportedErrorSite { span, .. }
            | Self::CapabilityLeakIntoCoreErl { span, .. } => Some(*span),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ir::IrNodeId;
    use std::path::PathBuf;

    fn dummy_span() -> Span {
        Span::point(0)
    }

    // ── code() stability tests (FROZEN-02, one per variant) ──────────────────

    #[test]
    fn ir_shape_malformed_code_is_stable() {
        let e = CodegenError::IrShapeMalformed {
            variant: "IrExpr::Lit",
            span: dummy_span(),
            detail: "test".into(),
        };
        assert_eq!(e.code(), "E001");
    }

    #[test]
    fn stdlib_bridge_missing_code_is_stable() {
        let e = CodegenError::StdlibBridgeMissing {
            module: "std.io".into(),
            name: "println".into(),
            span: dummy_span(),
        };
        assert_eq!(e.code(), "E002");
    }

    #[test]
    fn erlc_not_found_code_is_stable() {
        let e = CodegenError::ErlcNotFound {
            searched_paths: vec![PathBuf::from("/usr/bin/erlc")],
        };
        assert_eq!(e.code(), "E003");
    }

    #[test]
    fn erlc_rejected_input_code_is_stable() {
        let e = CodegenError::ErlcRejectedInput {
            core_path: PathBuf::from("out.core"),
            stderr: "parse error".into(),
            exit_code: 1,
        };
        assert_eq!(e.code(), "E004");
    }

    #[test]
    fn output_dir_not_writable_code_is_stable() {
        let e = CodegenError::OutputDirNotWritable {
            path: PathBuf::from("/tmp/out"),
            io_err: "permission denied".into(),
        };
        assert_eq!(e.code(), "E005");
    }

    #[test]
    fn beam_module_name_collision_code_is_stable() {
        let e = CodegenError::BeamModuleNameCollision {
            left: ModuleId(0),
            right: ModuleId(1),
            mangled: "ridge_main".into(),
        };
        assert_eq!(e.code(), "E006");
    }

    #[test]
    fn type_erasure_unsupported_error_site_code_is_stable() {
        let e = CodegenError::TypeErasureUnsupportedErrorSite {
            span: dummy_span(),
            node: IrNodeId(0),
            ir_variant: "IrExpr::Call",
        };
        assert_eq!(e.code(), "E007");
    }

    #[test]
    fn capability_leak_into_core_erl_code_is_stable() {
        let e = CodegenError::CapabilityLeakIntoCoreErl {
            span: dummy_span(),
            leaked_token: "io".into(),
        };
        assert_eq!(e.code(), "E008");
    }

    #[test]
    fn erlc_version_too_old_code_is_stable() {
        let e = CodegenError::ErlcVersionTooOld {
            found: "OTP 24".into(),
            minimum: "OTP 26".into(),
        };
        assert_eq!(e.code(), "E101");
    }

    #[test]
    fn erlc_unexpected_output_code_is_stable() {
        let e = CodegenError::ErlcUnexpectedOutput {
            core_path: PathBuf::from("out.core"),
            stdout: String::new(),
            stderr: "unexpected".into(),
        };
        assert_eq!(e.code(), "E102");
    }

    // ── existing constructibility + audit tests ───────────────────────────────

    #[test]
    fn all_variants_constructible() {
        let _e001 = CodegenError::IrShapeMalformed {
            variant: "IrExpr::Lit",
            span: dummy_span(),
            detail: "test".into(),
        };
        let _e002 = CodegenError::StdlibBridgeMissing {
            module: "std.io".into(),
            name: "println".into(),
            span: dummy_span(),
        };
        let _e003 = CodegenError::ErlcNotFound {
            searched_paths: vec![PathBuf::from("/usr/bin/erlc")],
        };
        let _e004 = CodegenError::ErlcRejectedInput {
            core_path: PathBuf::from("out.core"),
            stderr: "parse error".into(),
            exit_code: 1,
        };
        let _e005 = CodegenError::OutputDirNotWritable {
            path: PathBuf::from("/tmp/out"),
            io_err: "permission denied".into(),
        };
        let _e006 = CodegenError::BeamModuleNameCollision {
            left: ModuleId(0),
            right: ModuleId(1),
            mangled: "ridge_main".into(),
        };
        let _e007 = CodegenError::TypeErasureUnsupportedErrorSite {
            span: dummy_span(),
            node: IrNodeId(0),
            ir_variant: "IrExpr::Call",
        };
        let _e008 = CodegenError::CapabilityLeakIntoCoreErl {
            span: dummy_span(),
            leaked_token: "io".into(),
        };
        let _e101 = CodegenError::ErlcVersionTooOld {
            found: "OTP 24".into(),
            minimum: "OTP 26".into(),
        };
        let _e102 = CodegenError::ErlcUnexpectedOutput {
            core_path: PathBuf::from("out.core"),
            stdout: String::new(),
            stderr: "unexpected".into(),
        };
    }

    // ── T11: audit_type_error_at — allow-list tests ───────────────────────────

    /// Test 3 (DoD): allowed sites with `Type::Error` must return `None` (no E007).
    #[test]
    fn audit_allows_known_sites() {
        use ridge_types::Type;
        let sp = dummy_span();
        let node = IrNodeId(0);

        // FnReturnType is explicitly tolerated.
        let result = audit_type_error_at(
            &Type::Error,
            AllowedErrorSite::FnReturnType,
            node,
            sp,
            "IrFn",
        );
        assert!(result.is_none(), "FnReturnType must be allowed");

        // ParamType is explicitly tolerated.
        let result = audit_type_error_at(
            &Type::Error,
            AllowedErrorSite::ParamType,
            node,
            sp,
            "IrParam",
        );
        assert!(result.is_none(), "ParamType must be allowed");

        // WildcardPattern is explicitly tolerated.
        let result = audit_type_error_at(
            &Type::Error,
            AllowedErrorSite::WildcardPattern,
            node,
            sp,
            "IrPat::Wild",
        );
        assert!(result.is_none(), "WildcardPattern must be allowed");
    }

    /// Test 4 (DoD): `Type::Error` at a non-allowed site fires `E007`.
    ///
    /// Uses `AllowedErrorSite::SyntheticTestSite` — a `#[cfg(test)]`-only
    /// variant that is deliberately absent from `ALLOWED_ERROR_SITES`.  This
    /// exercises the full rejection path of [`audit_type_error_at`].
    #[test]
    fn audit_rejects_synthetic_unknown_site() {
        use ridge_types::Type;
        let sp = dummy_span();
        let node = IrNodeId(42);

        // Sanity: non-Error type → always None regardless of site.
        // Use Type::Tuple([]) as a simple, concrete non-Error type.
        let result = audit_type_error_at(
            &Type::Tuple(vec![]),
            AllowedErrorSite::SyntheticTestSite,
            node,
            sp,
            "IrExpr::SyntheticSite",
        );
        assert!(result.is_none(), "non-Error type must never trigger E007");

        // SyntheticTestSite is NOT in ALLOWED_ERROR_SITES, so Type::Error here
        // must produce Some(E007).
        let result = audit_type_error_at(
            &Type::Error,
            AllowedErrorSite::SyntheticTestSite,
            node,
            sp,
            "IrExpr::SyntheticSite",
        );
        assert!(result.is_some(), "SyntheticTestSite must trigger E007");
        match result.unwrap() {
            CodegenError::TypeErasureUnsupportedErrorSite {
                node: n,
                ir_variant,
                ..
            } => {
                assert_eq!(n, IrNodeId(42));
                assert_eq!(ir_variant, "IrExpr::SyntheticSite");
            }
            _ => panic!("expected TypeErasureUnsupportedErrorSite"),
        }

        // Sanity: ALLOWED_ERROR_SITES must not contain the test sentinel.
        assert!(!ALLOWED_ERROR_SITES.contains(&AllowedErrorSite::SyntheticTestSite));
        // And the production sites must all be present.
        assert!(ALLOWED_ERROR_SITES.contains(&AllowedErrorSite::FnReturnType));
        assert!(ALLOWED_ERROR_SITES.contains(&AllowedErrorSite::ParamType));
        assert!(ALLOWED_ERROR_SITES.contains(&AllowedErrorSite::WildcardPattern));
    }
}
