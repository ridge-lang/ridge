//! §4.20–§4.25 — Lower `IrPat` to `CErlPat`.
//!
//! Pattern lowering is context-free: each `IrPat` variant maps to a `CErlPat`
//! without needing a `LocalScope` or other mutable state.  The [`lower_pat`]
//! function is called from `expr.rs` during `IrExpr::LetIn` (destructuring)
//! and `IrExpr::Match` (arm patterns) lowering.

// T4 helpers are consumed by expr.rs.  Until T8 wires the top-level pipeline
// these items are only exercised from the test suite.
#![allow(dead_code)]
// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
#![allow(clippy::redundant_pub_crate)]

use crate::core_ast::{CErlAtom, CErlExpr, CErlLit, CErlPat, CErlVar};
use crate::error::CodegenError;
use crate::expr::name_to_erl_var;
use crate::lit::lower_lit;
use ridge_ast::Span;
use ridge_ir::{CtorKind, IrPat, SymbolRef};

/// Lower an [`IrPat`] to a [`CErlPat`].
///
/// Covers all 6 `IrPat` variants per §4.20–§4.25.
///
/// The `IrPat::Ctor` arm handles:
/// - `Constructor { ctor_kind: Record }` → `CErlPat::MapPat` with atom keys.
/// - `Constructor { ctor_kind: UnionVariant }` with args → `CErlPat::Tuple`
///   with a leading name atom.
/// - `Constructor { ctor_kind: UnionVariant }` with no args/fields →
///   `CErlPat::Lit(Atom)`.
/// - `Prelude { name: "Some" / "Ok" / "Err" }` → tagged tuple.
/// - `Prelude { name: "None" }` → bare atom `'none'`.
pub(crate) fn lower_pat(pat: &IrPat) -> Result<CErlPat, CodegenError> {
    match pat {
        // §4.20 — Wildcard.
        IrPat::Wild { .. } => Ok(CErlPat::Wild),

        // §4.21 — Literal pattern.
        IrPat::Lit { value, span, .. } => lower_lit(value, *span).map(CErlPat::Lit),

        // §4.22 — Variable binding or as-pattern.
        IrPat::Bind { name, inner, .. } => {
            let var = CErlVar(name_to_erl_var(name));
            match inner {
                // Plain variable binding: `name` → `V_Name`.
                None => Ok(CErlPat::Var(var)),
                // As-pattern: `p as name` → `<lower(p)> = V_Name`.
                Some(inner_pat) => Ok(CErlPat::Alias {
                    var,
                    inner: Box::new(lower_pat(inner_pat)?),
                }),
            }
        }

        // §4.23 — Constructor pattern.
        IrPat::Ctor {
            sym,
            fields,
            args,
            span,
        } => lower_ctor_pat(sym, fields, args, *span),

        // §4.24 — Tuple pattern.
        IrPat::Tuple { elems, .. } => {
            let lowered = elems.iter().map(lower_pat).collect::<Result<Vec<_>, _>>()?;
            Ok(CErlPat::Tuple(lowered))
        }

        // §4.25 — Cons-cell pattern.
        IrPat::Cons { head, tail, .. } => Ok(CErlPat::Cons {
            head: Box::new(lower_pat(head)?),
            tail: Box::new(lower_pat(tail)?),
        }),

        // §4.26 — Empty-list pattern `[]` → nil literal.
        IrPat::Nil { .. } => Ok(CErlPat::Lit(CErlLit::Nil)),

        // IrPat is #[non_exhaustive]; catch future variants defensively.
        _ => Err(CodegenError::IrShapeMalformed {
            variant: "IrPat",
            span: Span::point(0),
            detail: "T4: unrecognised IrPat variant — pending future lowering task".into(),
        }),
    }
}

/// Lower an `IrPat::Ctor` (§4.23).
fn lower_ctor_pat(
    sym: &SymbolRef,
    fields: &[(String, IrPat)],
    args: &[IrPat],
    span: Span,
) -> Result<CErlPat, CodegenError> {
    match sym {
        SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            ..
        } => {
            // Record pattern → MapPat with atom-keyed entries.
            // Each field (name, pat) maps to (atom "name", lower_pat(pat)).
            let entries = fields
                .iter()
                .map(|(field_name, field_pat)| {
                    let key = CErlExpr::Lit(CErlLit::Atom(CErlAtom(field_name.clone())));
                    let val = lower_pat(field_pat)?;
                    Ok((key, val))
                })
                .collect::<Result<Vec<_>, CodegenError>>()?;
            Ok(CErlPat::MapPat(entries))
        }

        SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            name,
            ..
        } => {
            if args.is_empty() && fields.is_empty() {
                // Zero-payload union variant → bare atom `'<name>'`.
                Ok(CErlPat::Lit(CErlLit::Atom(CErlAtom(name.clone()))))
            } else {
                // Union variant with positional args → tagged tuple `{'<name>', p1, …}`.
                let mut elems = Vec::with_capacity(1 + args.len() + fields.len());
                elems.push(CErlPat::Lit(CErlLit::Atom(CErlAtom(name.clone()))));
                // Positional args first (union-variant style).
                for arg in args {
                    elems.push(lower_pat(arg)?);
                }
                // Named fields (rare for union variants but possible).
                for (_, field_pat) in fields {
                    elems.push(lower_pat(field_pat)?);
                }
                Ok(CErlPat::Tuple(elems))
            }
        }

        SymbolRef::Prelude { name } => lower_prelude_pat(name, args, fields, span),

        // Handler / ActorType / Local / Stdlib / External as patterns are
        // Phase-5 invariant violations.
        _ => Err(CodegenError::IrShapeMalformed {
            variant: "IrPat::Ctor",
            span,
            detail: "T4: unexpected SymbolRef variant in constructor pattern".into(),
        }),
    }
}

/// Lower a `Prelude`-keyed constructor pattern (§4.23 prelude table).
fn lower_prelude_pat(
    name: &str,
    args: &[IrPat],
    _fields: &[(String, IrPat)],
    span: Span,
) -> Result<CErlPat, CodegenError> {
    match name {
        "Some" => {
            // `Some p` → `{'some', lower(p)}`.
            let inner = args.first().ok_or_else(|| CodegenError::IrShapeMalformed {
                variant: "IrPat::Ctor(Prelude::Some)",
                span,
                detail: "T4: Some pattern expects exactly one argument".into(),
            })?;
            Ok(CErlPat::Tuple(vec![
                CErlPat::Lit(CErlLit::Atom(CErlAtom("some".into()))),
                lower_pat(inner)?,
            ]))
        }
        "None" => {
            // `None` → `'none'`.
            Ok(CErlPat::Lit(CErlLit::Atom(CErlAtom("none".into()))))
        }
        "Ok" => {
            // `Ok p` → `{'ok', lower(p)}`.
            let inner = args.first().ok_or_else(|| CodegenError::IrShapeMalformed {
                variant: "IrPat::Ctor(Prelude::Ok)",
                span,
                detail: "T4: Ok pattern expects exactly one argument".into(),
            })?;
            Ok(CErlPat::Tuple(vec![
                CErlPat::Lit(CErlLit::Atom(CErlAtom("ok".into()))),
                lower_pat(inner)?,
            ]))
        }
        "Err" => {
            // `Err p` → `{'error', lower(p)}`.
            let inner = args.first().ok_or_else(|| CodegenError::IrShapeMalformed {
                variant: "IrPat::Ctor(Prelude::Err)",
                span,
                detail: "T4: Err pattern expects exactly one argument".into(),
            })?;
            Ok(CErlPat::Tuple(vec![
                CErlPat::Lit(CErlLit::Atom(CErlAtom("error".into()))),
                lower_pat(inner)?,
            ]))
        }
        other => Err(CodegenError::IrShapeMalformed {
            variant: "IrPat::Ctor(Prelude)",
            span,
            detail: format!("T4: unknown Prelude constructor pattern '{other}'"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAtom, CErlLit, CErlPat, CErlVar};
    use ridge_ast::Span;
    use ridge_ir::{CtorKind, IrLit, IrPat, SymbolRef};
    use ridge_resolve::ModuleId;
    use ridge_types::TyConId;

    fn sp() -> Span {
        Span::point(0)
    }

    // ── §4.20 Wild ───────────────────────────────────────────────────────────

    #[test]
    fn pat_wild() {
        let pat = IrPat::Wild { span: sp() };
        let result = lower_pat(&pat).unwrap();
        assert!(matches!(result, CErlPat::Wild));
    }

    // ── §4.21 Lit ────────────────────────────────────────────────────────────

    #[test]
    fn pat_lit_int() {
        let pat = IrPat::Lit {
            value: IrLit::Int(7),
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        assert!(matches!(result, CErlPat::Lit(CErlLit::Int(7))));
    }

    // ── §4.22 Bind ───────────────────────────────────────────────────────────

    #[test]
    fn pat_bind_no_inner() {
        let pat = IrPat::Bind {
            name: "x".into(),
            inner: None,
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        assert!(matches!(result, CErlPat::Var(CErlVar(ref s)) if s == "V_X"));
    }

    #[test]
    fn pat_bind_with_inner() {
        // `Wild as x` → Alias { var: V_X, inner: Wild }.
        let pat = IrPat::Bind {
            name: "x".into(),
            inner: Some(Box::new(IrPat::Wild { span: sp() })),
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        match result {
            CErlPat::Alias {
                var: CErlVar(ref v),
                inner,
            } => {
                assert_eq!(v, "V_X");
                assert!(matches!(*inner, CErlPat::Wild));
            }
            other => panic!("expected Alias, got {other:?}"),
        }
    }

    // ── §4.24 Tuple ──────────────────────────────────────────────────────────

    #[test]
    fn pat_tuple() {
        let pat = IrPat::Tuple {
            elems: vec![IrPat::Wild { span: sp() }, IrPat::Wild { span: sp() }],
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        match result {
            CErlPat::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(elems[0], CErlPat::Wild));
                assert!(matches!(elems[1], CErlPat::Wild));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    // ── §4.25 Cons ───────────────────────────────────────────────────────────

    #[test]
    fn pat_cons() {
        let pat = IrPat::Cons {
            head: Box::new(IrPat::Wild { span: sp() }),
            tail: Box::new(IrPat::Wild { span: sp() }),
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        match result {
            CErlPat::Cons { head, tail } => {
                assert!(matches!(*head, CErlPat::Wild));
                assert!(matches!(*tail, CErlPat::Wild));
            }
            other => panic!("expected Cons, got {other:?}"),
        }
    }

    // ── §4.23 Ctor — Record ──────────────────────────────────────────────────

    #[test]
    fn pat_ctor_record() {
        // Record pattern with one field `name → Bind "n"`.
        let pat = IrPat::Ctor {
            sym: SymbolRef::Constructor {
                ctor_kind: CtorKind::Record,
                owner_type: TyConId(0),
                name: "User".into(),
                variant: 0,
            },
            fields: vec![(
                "name".into(),
                IrPat::Bind {
                    name: "n".into(),
                    inner: None,
                    span: sp(),
                },
            )],
            args: vec![],
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        match result {
            CErlPat::MapPat(entries) => {
                assert_eq!(entries.len(), 1);
                let (key, val) = &entries[0];
                assert!(
                    matches!(key, CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s))) if s == "name")
                );
                assert!(matches!(val, CErlPat::Var(CErlVar(ref v)) if v == "V_N"));
            }
            other => panic!("expected MapPat, got {other:?}"),
        }
    }

    // ── §4.23 Ctor — UnionVariant zero payload ───────────────────────────────

    #[test]
    fn pat_ctor_union_zero_payload() {
        let pat = IrPat::Ctor {
            sym: SymbolRef::Constructor {
                ctor_kind: CtorKind::UnionVariant,
                owner_type: TyConId(1),
                name: "Info".into(),
                variant: 0,
            },
            fields: vec![],
            args: vec![],
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        assert!(matches!(result, CErlPat::Lit(CErlLit::Atom(CErlAtom(ref s))) if s == "Info"));
    }

    // ── §4.23 Ctor — UnionVariant with args ─────────────────────────────────

    #[test]
    fn pat_ctor_union_with_args() {
        // `Pair(Wild, Wild)` → `{'Pair', Wild, Wild}`.
        let pat = IrPat::Ctor {
            sym: SymbolRef::Constructor {
                ctor_kind: CtorKind::UnionVariant,
                owner_type: TyConId(2),
                name: "Pair".into(),
                variant: 0,
            },
            fields: vec![],
            args: vec![IrPat::Wild { span: sp() }, IrPat::Wild { span: sp() }],
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        match result {
            CErlPat::Tuple(elems) => {
                assert_eq!(elems.len(), 3);
                assert!(
                    matches!(&elems[0], CErlPat::Lit(CErlLit::Atom(CErlAtom(s))) if s == "Pair")
                );
                assert!(matches!(elems[1], CErlPat::Wild));
                assert!(matches!(elems[2], CErlPat::Wild));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    // ── §4.23 Prelude patterns ───────────────────────────────────────────────

    #[test]
    fn pat_prelude_some() {
        // `Some Wild` → `{'some', Wild}`.
        let pat = IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Some".into(),
            },
            fields: vec![],
            args: vec![IrPat::Wild { span: sp() }],
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        match result {
            CErlPat::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(
                    matches!(&elems[0], CErlPat::Lit(CErlLit::Atom(CErlAtom(s))) if s == "some")
                );
                assert!(matches!(elems[1], CErlPat::Wild));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    #[test]
    fn pat_prelude_none() {
        // `None` → `'none'`.
        let pat = IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "None".into(),
            },
            fields: vec![],
            args: vec![],
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        assert!(matches!(result, CErlPat::Lit(CErlLit::Atom(CErlAtom(ref s))) if s == "none"));
    }

    #[test]
    fn pat_prelude_ok() {
        // `Ok Wild` → `{'ok', Wild}`.
        let pat = IrPat::Ctor {
            sym: SymbolRef::Prelude { name: "Ok".into() },
            fields: vec![],
            args: vec![IrPat::Wild { span: sp() }],
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        match result {
            CErlPat::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(&elems[0], CErlPat::Lit(CErlLit::Atom(CErlAtom(s))) if s == "ok"));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    #[test]
    fn pat_prelude_err() {
        // `Err Wild` → `{'error', Wild}`.
        let pat = IrPat::Ctor {
            sym: SymbolRef::Prelude { name: "Err".into() },
            fields: vec![],
            args: vec![IrPat::Wild { span: sp() }],
            span: sp(),
        };
        let result = lower_pat(&pat).unwrap();
        match result {
            CErlPat::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(
                    matches!(&elems[0], CErlPat::Lit(CErlLit::Atom(CErlAtom(s))) if s == "error")
                );
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    // ── Prelude unused name is an error ──────────────────────────────────────

    #[test]
    fn pat_prelude_unknown_is_error() {
        let pat = IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Unknown".into(),
            },
            fields: vec![],
            args: vec![],
            span: sp(),
        };
        let result = lower_pat(&pat);
        assert!(matches!(result, Err(CodegenError::IrShapeMalformed { .. })));
    }

    // ── Unused import guards ─────────────────────────────────────────────────

    fn _use_module_id() -> ModuleId {
        ModuleId(0)
    }
}
