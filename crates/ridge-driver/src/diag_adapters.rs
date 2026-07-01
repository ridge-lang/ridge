//! `From<&XError> for Diagnostic` adapters for `ridge-typecheck` and
//! `ridge-codegen-erl` error types.
//!
//! These adapters live in `ridge-driver` (not `ridge-diagnostics`) because
//! both `ridge-typecheck` and `ridge-codegen-erl` depend on `ridge-diagnostics`,
//! making `ridge-diagnostics → ridge-typecheck/ridge-codegen-erl` a dep cycle.
//! `ridge-driver` depends on all four crates and is the natural home.

use ridge_codegen_erl::CodegenError;
use ridge_diagnostics::{Diagnostic, DiagnosticNote, NoteSeverity, SourceId};
use ridge_ir::Span;
use ridge_resolve::Severity;
use ridge_typecheck::TypeError;

// ── TypeError → Diagnostic ────────────────────────────────────────────────────

/// Build a [`Diagnostic`] from a [`TypeError`].
///
/// Suggestions on `T005 UnknownField` and `T008 UnknownConstructor` are
/// surfaced as `Help`-level notes.
#[must_use]
pub fn diag_from_typecheck(e: &TypeError, source_id: SourceId) -> Diagnostic {
    use ridge_diagnostics::HasErrorCode;

    let code = e.code();
    let severity = e.severity();
    let primary_span = e.span();
    let message = e.to_string();

    let mut diag = Diagnostic::new(code, severity, primary_span, message, source_id);

    // Surface per-variant secondary notes.
    match e {
        TypeError::UnknownField { suggestions, .. }
        | TypeError::UnknownConstructor { suggestions, .. }
        | TypeError::UnknownActorHandler { suggestions, .. } => {
            for sug in suggestions {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message: format!("did you mean `{sug}`?"),
                    severity: NoteSeverity::Help,
                });
            }
        }
        TypeError::NonExhaustiveMatch {
            witnesses,
            total_missing,
            ..
        } => {
            for w in witnesses {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message: format!("missing pattern: {w}"),
                    severity: NoteSeverity::Help,
                });
            }
            if *total_missing > witnesses.len() {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message: format!(
                        "... and {} more missing pattern(s)",
                        total_missing - witnesses.len()
                    ),
                    severity: NoteSeverity::Note,
                });
            }
        }
        TypeError::InsertShapeFullEntity {
            companion, omitted, ..
        } if !omitted.is_empty() => {
            let cols = omitted
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let (plural, them) = if omitted.len() == 1 {
                ("", "it")
            } else {
                ("s", "them")
            };
            diag.notes.push(DiagnosticNote {
                span: primary_span,
                message: format!(
                    "`{companion}` drops the database-generated column{plural} {cols}; build a `{companion}` and leave {them} to the database"
                ),
                severity: NoteSeverity::Help,
            });
        }
        _ => {}
    }

    diag
}

// ── CodegenError → Diagnostic ─────────────────────────────────────────────────

/// Build a [`Diagnostic`] from a [`CodegenError`].
///
/// Toolchain-oriented variants (`E003`–`E006`, `E101`, `E102`) carry no source
/// span; they use a sentinel span and render context-lessly.  Span-bearing
/// variants (`E001`, `E002`, `E007`, `E008`) anchor to their source location.
#[must_use]
pub fn diag_from_codegen(e: &CodegenError, source_id: SourceId) -> Diagnostic {
    let code = e.code();
    let primary_span = e.span().unwrap_or_else(|| Span::point(0));
    let severity = Severity::Error;

    let message = describe_codegen_error(e);
    let mut diag = Diagnostic::new(code, severity, primary_span, message, source_id);

    // For E004/E102, surface erlc stderr as a note.
    match e {
        CodegenError::ErlcRejectedInput { stderr, .. } => {
            if !stderr.is_empty() {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message: format!("erlc output:\n{stderr}"),
                    severity: NoteSeverity::Note,
                });
            }
        }
        CodegenError::ErlcUnexpectedOutput { stderr, .. } => {
            if !stderr.is_empty() {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message: format!("erlc stderr:\n{stderr}"),
                    severity: NoteSeverity::Note,
                });
            }
        }
        _ => {}
    }

    diag
}

/// Produce a human-readable one-line message for a `CodegenError`.
fn describe_codegen_error(e: &CodegenError) -> String {
    match e {
        CodegenError::IrShapeMalformed { detail, .. } => {
            format!("E001: malformed IR: {detail}")
        }
        CodegenError::StdlibBridgeMissing { module, name, .. } => {
            format!("E002: no stdlib bridge for `{module}.{name}`")
        }
        CodegenError::ErlcNotFound { .. } => {
            "E003: erlc not found on PATH (install OTP 26+)".to_owned()
        }
        CodegenError::ErlcRejectedInput {
            core_path,
            exit_code,
            ..
        } => {
            format!(
                "E004: erlc rejected `{}` (exit {})",
                core_path.display(),
                exit_code
            )
        }
        CodegenError::OutputDirNotWritable { path, io_err } => {
            format!(
                "E005: output directory `{}` not writable: {io_err}",
                path.display()
            )
        }
        CodegenError::BeamModuleNameCollision { mangled, .. } => {
            format!("E006: two Ridge modules mangle to the same BEAM name `{mangled}`")
        }
        CodegenError::TypeErasureUnsupportedErrorSite { ir_variant, .. } => {
            format!("E007: unexpected Type::Error at IR site `{ir_variant}`")
        }
        CodegenError::CapabilityLeakIntoCoreErl { leaked_token, .. } => {
            format!("E008: capability token `{leaked_token}` leaked into Core Erlang")
        }
        CodegenError::ErlcVersionTooOld { found, minimum } => {
            format!("E101: erlc version `{found}` is below minimum `{minimum}`")
        }
        CodegenError::ErlcUnexpectedOutput { core_path, .. } => {
            format!(
                "E102: erlc produced unexpected output for `{}`",
                core_path.display()
            )
        }
        _ => format!("{}: unknown codegen error", e.code()),
    }
}
