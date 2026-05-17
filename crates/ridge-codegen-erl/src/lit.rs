//! §4.1 — Lower `IrLit` to `CErlLit`.
//!
//! Literals are the simplest lowering form: each Ridge literal maps to exactly
//! one Core Erlang literal without any context requirements.

// T3 helpers are consumed by lower_expr (expr.rs) and wired into the
// module-level entry points in T8.  Until T8 ships they are only exercised
// from within this module's test suite and from expr.rs.
#![allow(dead_code)]
// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it anyway for explicitness per plan §2.2 — suppress the lint here.
#![allow(clippy::redundant_pub_crate)]

use crate::core_ast::{CErlAtom, CErlLit};
use crate::error::CodegenError;
use ridge_ast::Span;
use ridge_ir::IrLit;

/// Lower an [`IrLit`] to a [`CErlLit`].
///
/// Returns `Err(CodegenError::IrShapeMalformed)` for non-finite floats
/// (`NaN`, `Inf`): Phase 4 should have rejected these at compile time, so
/// seeing them here signals an upstream invariant violation.
///
/// # OQ-E001
/// IEEE 754 NaN/Infinity: reject at codegen — deferred to a `ridge_rt`
/// binding (`'$nan'`) in a future release.
pub(crate) fn lower_lit(lit: &IrLit, span: Span) -> Result<CErlLit, CodegenError> {
    match lit {
        IrLit::Int(n) => Ok(CErlLit::Int(*n)),
        IrLit::Float(f) => {
            // OQ-E001: NaN and Infinity are not representable as Core Erlang
            // compile-time literals.  Phase 4 should have rejected them; we
            // treat their appearance here as a Phase-5 invariant violation.
            if f.is_nan() || f.is_infinite() {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrLit::Float",
                    span,
                    detail: format!(
                        "non-finite float value ({f}) is not representable as a Core Erlang \
                         literal; Phase 4 should have rejected it (OQ-E001)"
                    ),
                });
            }
            Ok(CErlLit::Float(*f))
        }
        IrLit::Bool(true) => Ok(CErlLit::Atom(CErlAtom("true".into()))),
        IrLit::Bool(false) => Ok(CErlLit::Atom(CErlAtom("false".into()))),
        // §3.8 Text → BEAM binary (UTF-8 bytes); null bytes are valid in BEAM binaries.
        IrLit::Text(s) => Ok(CErlLit::Binary(s.as_bytes().to_vec())),
        // §3.8 Unit → `'ok'` atom.
        IrLit::Unit => Ok(CErlLit::Atom(CErlAtom("ok".into()))),
        IrLit::EmptyList => Ok(CErlLit::Nil),
        // IrLit is #[non_exhaustive]; future variants land here as malformed until
        // this file is extended.
        _ => Err(CodegenError::IrShapeMalformed {
            variant: "IrLit",
            span,
            detail: "T3: unrecognised IrLit variant — pending future lowering task".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::Span;

    fn sp() -> Span {
        Span::point(0)
    }

    #[test]
    fn lit_int() {
        let result = lower_lit(&IrLit::Int(42), sp());
        assert!(matches!(result, Ok(CErlLit::Int(42))));
    }

    #[test]
    fn lit_float() {
        let result = lower_lit(&IrLit::Float(1.5), sp());
        assert!(matches!(result, Ok(CErlLit::Float(f)) if f.to_bits() == 1.5_f64.to_bits()));
    }

    #[test]
    fn lit_bool_true() {
        let result = lower_lit(&IrLit::Bool(true), sp());
        assert!(matches!(result, Ok(CErlLit::Atom(CErlAtom(ref s))) if s == "true"));
    }

    #[test]
    fn lit_bool_false() {
        let result = lower_lit(&IrLit::Bool(false), sp());
        assert!(matches!(result, Ok(CErlLit::Atom(CErlAtom(ref s))) if s == "false"));
    }

    #[test]
    fn lit_text() {
        let result = lower_lit(&IrLit::Text("hi".into()), sp());
        assert!(matches!(result, Ok(CErlLit::Binary(ref b)) if b == b"hi"));
    }

    #[test]
    fn lit_unit() {
        let result = lower_lit(&IrLit::Unit, sp());
        assert!(matches!(result, Ok(CErlLit::Atom(CErlAtom(ref s))) if s == "ok"));
    }

    #[test]
    fn lit_empty_list() {
        let result = lower_lit(&IrLit::EmptyList, sp());
        assert!(matches!(result, Ok(CErlLit::Nil)));
    }

    #[test]
    fn lit_float_nan_is_error() {
        let result = lower_lit(&IrLit::Float(f64::NAN), sp());
        assert!(matches!(
            result,
            Err(CodegenError::IrShapeMalformed {
                variant: "IrLit::Float",
                ..
            })
        ));
    }

    #[test]
    fn lit_float_infinity_is_error() {
        let result = lower_lit(&IrLit::Float(f64::INFINITY), sp());
        assert!(matches!(
            result,
            Err(CodegenError::IrShapeMalformed {
                variant: "IrLit::Float",
                ..
            })
        ));
    }

    #[test]
    fn lit_float_neg_infinity_is_error() {
        let result = lower_lit(&IrLit::Float(f64::NEG_INFINITY), sp());
        assert!(matches!(
            result,
            Err(CodegenError::IrShapeMalformed {
                variant: "IrLit::Float",
                ..
            })
        ));
    }
}
