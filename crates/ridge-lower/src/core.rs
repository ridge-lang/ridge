//! Core expression and pattern dispatcher (`lower_expr` / `lower_pattern`).
//!
//! This module is the central dispatch table for Phase 5 lowering.  Every
//! `Expr::*` and `Pattern::*` variant has exactly one arm; atom variants
//! produce correct IR immediately while non-atomic variants are handled by
//! their dedicated rule modules.
//!
//! # Atom variants lowered here
//!
//! | AST variant | IR result |
//! |---|---|
//! | `Expr::Literal(Int*)` | `IrExpr::Lit { IrLit::Int(i64) }` |
//! | `Expr::Literal(Float)` | `IrExpr::Lit { IrLit::Float(f64) }` |
//! | `Expr::Literal(Bool)` | `IrExpr::Lit { IrLit::Bool(bool) }` |
//! | `Expr::Literal(Text)` | `IrExpr::Lit { IrLit::Text(String) }` |
//! | `Expr::Unit` | `IrExpr::Lit { IrLit::Unit }` |
//! | `Expr::Ident` | `IrExpr::Local` or `IrExpr::Symbol` via `BindingMap` |
//! | `Expr::Qualified` | `IrExpr::Symbol` via `BindingMap` |
//! | `Expr::Interp` (text-only, 1 segment) | `IrExpr::Lit { IrLit::Text }` |
//!
//! # Direct AST→IR mappings
//!
//! | AST variant | IR result |
//! |---|---|
//! | `Expr::Tuple` | `IrExpr::Tuple` |
//! | `Expr::List` | `IrExpr::ListLit` |
//! | `Expr::Return` | `IrExpr::Return` |
//! | `Expr::Lambda` | `IrExpr::Lambda` (caps via `lookup_inferred_caps`) |
//! | `Expr::Record` | `IrExpr::Construct` (tycon via `lookup_tycon_by_name`) |
//! | `Expr::FieldAccess` | `IrExpr::Field` |
//! | `Expr::Call` | `IrExpr::Call` |
//! | `Expr::Spawn` | `IrExpr::Spawn` (actor module via `resolve_actor_module`; OQ-PHASE45-006) |
//! | `Expr::Send` | `IrExpr::Send` (actor name resolved via binding-map Group B 3.1 path) |
//! | `Expr::Ask` | `IrExpr::Ask` (actor name resolved via binding-map Group B 3.1 path) |
// PHASE45-Group-B: Send/Ask handler-name resolution now uses BindingMap-first precedence.
//!
//! # `BindingMap` wiring
//!
//! `lower_expr` resolves `Ident` and `QualifiedName` nodes via the
//! `BindingMap` attached to [`LowerCtx`] during [`crate::lower_module`].
//! If the binding map is absent (e.g. test scaffolding with no `ResolvedModule`)
//! a defensive `L999` error is emitted and a `Unit` literal is returned.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::doc_markdown
    )
)]

use ridge_ast::{
    expr::{InterpPart, LambdaParam, QualifiedName, RecordCtor},
    Expr, Ident, Literal, Pattern, Span,
};
use ridge_ir::{IrExpr, IrLit, IrParam, IrPat, SymbolRef};
use ridge_resolve::{imports::Binding, ModuleId, NodeKind, StdlibModuleId, BUILTINS};
use ridge_types::{CapRow, TyConId, Type};

use crate::ast_type::lower_ast_type;
use crate::block::{lower_assign, lower_block};
use crate::ctx::LowerCtx;
use crate::error::LowerError;
use crate::field_accessor::lower_field_accessor;
use crate::guard::lower_guard_bare;
use crate::if_lower::lower_if;
use crate::inner_fn::lower_inner_fn_bare;
use crate::interp::lower_interp_full;
use crate::match_lower::{lower_match, lower_pattern_full};
use crate::operators::{lower_binary, lower_unary};
use crate::pipe::lower_pipe;
use crate::propagate::lower_propagate;
use crate::try_block::lower_try;
use crate::with_update::lower_with;

// ── Public dispatcher — expressions ──────────────────────────────────────────

/// Lower a single [`Expr`] node to its [`IrExpr`] equivalent.
///
/// Atoms (`Literal`, `Unit`, `Ident`, `Qualified`, text-only `Interp`) produce
/// their correct IR immediately.  Every other variant returns a stub
/// `IrExpr::Lit { IrLit::Unit }` with the expression's span preserved; the
/// stub is replaced once the corresponding rule module lands.
///
/// On any defensive error the error is pushed to [`LowerCtx::errors`] and a
/// `Unit` literal is returned (never panics on any input).
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive dispatch over all Expr variants — each arm is a single delegating call or a short constructor; splitting would obscure the flat dispatch table"
)]
pub fn lower_expr(ctx: &mut LowerCtx<'_>, expr: &Expr) -> IrExpr {
    match expr {
        // ── Atoms ─────────────────────────────────────────────────────────────
        Expr::Literal(lit) => lower_literal(ctx, lit),

        Expr::Ident(ident) => lower_ident(ctx, ident),

        Expr::Qualified(qname) => lower_qualified(ctx, qname),

        // Text-only interpolation (single `Text` part, no holes): lower to a
        // text literal.  Any other shape (multiple parts, or a hole part) uses
        // full interpolation lowering.
        Expr::Interp { parts, span } => lower_interp(ctx, parts, *span),

        // ── Pipe, operators, field accessor, paren erasure ────────────────────

        // `lhs |> rhs` — desugars to a flat Call (§4.1).
        Expr::Pipe { lhs, rhs, span } => lower_pipe(ctx, lhs, rhs, *span),

        // `lhs op rhs` — desugars to stdlib Call or IrExpr::Cons (§4.11).
        Expr::Binary { op, lhs, rhs, span } => lower_binary(ctx, *op, lhs, rhs, *span),

        // `-expr` — desugars to stdlib Call (§4.11).
        Expr::Unary { op, expr, span } => lower_unary(ctx, *op, expr, *span),

        // `(.field)` — desugars to a Lambda (§4.10).
        Expr::FieldAccessorFn { field, span } => lower_field_accessor(ctx, field, *span),

        // `(expr)` — paren erasure: lower the inner expression directly (§1.3, §4.1).
        Expr::Paren { inner, .. } => lower_expr(ctx, inner),

        // ── Conditional and pattern matching ─────────────────────────────────

        // `if cond then then_branch [else else_branch]` — desugars to Match (§4.7).
        Expr::If {
            cond,
            then_branch,
            else_branch,
            span,
        } => lower_if(ctx, cond, then_branch, else_branch.as_deref(), *span),

        // `match scrutinee { arms }` — the only "preserved" mapping (§4.8).
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => lower_match(ctx, scrutinee, arms, *span),

        // ── Block, Let, Var, Assign ───────────────────────────────────────────

        // `{ stmts }` — lower via right-fold continuation (§4.9).
        Expr::Block(block) => lower_block(ctx, block),

        // `target <- value` — lower to IrExpr::Assign (§4.14).
        Expr::Assign {
            target,
            value,
            span,
        } => lower_assign(ctx, target, value, *span),

        // Bare `let` / `var` outside of a block context is a Phase 4 invariant
        // violation — emit a defensive L999 error and return a Unit stub.
        Expr::Let { span, .. } => {
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span: *span,
                message: "`let` binding encountered outside of block context; \
                          Phase 4 should have rejected this"
                    .into(),
            });
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span: *span,
            }
        }

        Expr::Var { span, .. } => {
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span: *span,
                message: "`var` binding encountered outside of block context; \
                          Phase 4 should have rejected this"
                    .into(),
            });
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span: *span,
            }
        }

        // ── Propagate (`?`) and Try ───────────────────────────────────────────

        // `inner?` — desugar to Match over Result/Option (§4.2).
        Expr::Propagate { inner, span } => lower_propagate(ctx, inner, *span),

        // `try { ... }` — push propagation scope, lower block, pop scope (§4.3).
        Expr::Try { block, span } => lower_try(ctx, block, *span),

        // ── Guard and InnerFn (bare, outside block context) ───────────────────

        // `guard cond else { ... }` appearing as a bare expression (not inside
        // a block fold) is a Phase 4 invariant violation.  Emit L006.
        Expr::Guard { span, .. } => lower_guard_bare(ctx, *span),

        // `fn name params = body` appearing as a bare expression (not inside a
        // block fold) is a Phase 4 invariant violation.  Emit L999.
        Expr::InnerFn { decl, span } => lower_inner_fn_bare(ctx, decl, *span),

        // ── `with` update desugaring ──────────────────────────────────────────

        // `base with { f1 = v1, ... }` — lowers to LetIn + Construct (§4.5).
        Expr::With { base, fields, span } => lower_with(ctx, base, fields, *span),

        // ── Unit literal ──────────────────────────────────────────────────────
        Expr::Unit(span) => {
            let id = ctx.fresh_id(None);
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span: *span,
            }
        }

        // ── Tuple, List, Return, Lambda, Record, Field, Call, Spawn, Send, Ask ─
        //
        // `(e1, e2, …)` lowers to `IrExpr::Tuple` with each element lowered
        // in source order.  No desugaring needed.
        Expr::Tuple { elems, span } => {
            let id = ctx.fresh_id(None);
            let elems = elems.iter().map(|e| lower_expr(ctx, e)).collect();
            IrExpr::Tuple {
                id,
                elems,
                span: *span,
            }
        }

        // ── List literal ──────────────────────────────────────────────────────
        //
        // `[e1, e2, …]` lowers to `IrExpr::ListLit`.
        // `IrExpr::ListLit` has no element-type field — element type rides on
        // the side-table; no IR slot.
        Expr::List { elems, span } => {
            let id = ctx.fresh_id(None);
            let elems = elems.iter().map(|e| lower_expr(ctx, e)).collect();
            IrExpr::ListLit {
                id,
                elems,
                span: *span,
            }
        }

        // ── Return ────────────────────────────────────────────────────────────
        //
        // `return e` lowers to `IrExpr::Return { value: lower(e) }`.
        // Per OQ-L011 (resolved §12): DO NOT wrap in Ok/Some; preserve verbatim.
        Expr::Return { value, span } => {
            let id = ctx.fresh_id(None);
            let value = Box::new(lower_expr(ctx, value));
            IrExpr::Return {
                id,
                value,
                span: *span,
            }
        }

        // ── Lambda ────────────────────────────────────────────────────────────
        //
        // `fn Param+ -> Body` lowers to `IrExpr::Lambda`.
        // Anonymous lambdas have no entry in the `inferred_caps` side-table
        // (Phase 4 only tracks top-level `fn` decls); caps default to PURE.
        // Bare param types are looked up from node_types via the parent
        // lambda's Type::Fn (see lambda_param_to_ir_param).
        //
        // B-2 fix (Phase 5 followup): tuple-pattern lambda params (e.g.
        // `fn (dr, dc) -> body`) get a synthetic `__tuple_param_N` name and
        // the body is wrapped in an inner `Match` that destructures the tuple.
        // This is mechanical and target-neutral; it uses `IrExpr::Match` and
        // `IrPat::Tuple`/`IrPat::Bind`, which Phase 6 already handles.
        Expr::Lambda { params, body, span } => {
            let id = ctx.fresh_id(None);
            // Lower all params; detect any non-Var tuple patterns.
            let mut ir_params: Vec<IrParam> = Vec::with_capacity(params.len());
            // Collect (param_idx, synthetic_name, tuple_elems, ty_for_param)
            // for params that need a destructuring Match wrapper.
            let mut destructure_entries: Vec<(String, Vec<IrPat>, Span)> = Vec::new();

            for (idx, p) in params.iter().enumerate() {
                match p {
                    LambdaParam::Pattern(Pattern::Tuple { elems, span: tspan }) => {
                        // Synthesise a fresh __tuple_param name.
                        let synth_name = ctx.fresh_local("__tuple_param");
                        // Look up the type of this param from the enclosing lambda's
                        // Type::Fn (same logic as lambda_param_to_ir_param).
                        let ty = ctx
                            .node_id_map
                            .as_ref()
                            .and_then(|m| m.get(*span, NodeKind::Expr))
                            .and_then(|nid| ctx.node_type(nid).cloned())
                            .and_then(|fn_ty| {
                                if let Type::Fn {
                                    params: fn_params, ..
                                } = fn_ty
                                {
                                    fn_params.into_iter().nth(idx)
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(Type::Error);
                        ir_params.push(IrParam {
                            name: synth_name.clone(),
                            ty,
                            span: *tspan,
                        });
                        // Collect IrPat elements for each tuple element:
                        // - Pattern::Var → IrPat::Bind
                        // - Pattern::Wildcard → IrPat::Wild (preserves arity)
                        // Other patterns are not yet supported; wildcard is the
                        // common case (fn (x, _) -> ...).
                        let elem_pats: Vec<IrPat> = elems
                            .iter()
                            .map(|e| match e {
                                Pattern::Var {
                                    name,
                                    span: eid_span,
                                } => IrPat::Bind {
                                    name: name.text.clone(),
                                    inner: None,
                                    span: *eid_span,
                                },
                                Pattern::Wildcard { span: ws } => IrPat::Wild { span: *ws },
                                // Defensive: other patterns degrade to Wild to preserve arity.
                                other => IrPat::Wild { span: other.span() },
                            })
                            .collect();
                        destructure_entries.push((synth_name, elem_pats, *tspan));
                    }
                    other => {
                        ir_params.push(lambda_param_to_ir_param(ctx, *span, idx, other));
                    }
                }
            }

            let lowered_body = lower_expr(ctx, body);

            // Wrap the body in nested Match nodes for each tuple-pattern param,
            // innermost-last (since we're building outside-in, we reverse).
            let wrapped_body = destructure_entries.into_iter().rev().fold(
                lowered_body,
                |inner_body, (synth_name, elem_pats, tspan)| {
                    // IrPat::Tuple { elems: [Bind(a), Wild, Bind(b), ...] }
                    let arm_pat = IrPat::Tuple {
                        elems: elem_pats,
                        span: tspan,
                    };
                    let arm = ridge_ir::IrArm {
                        pat: arm_pat,
                        when: None,
                        body: inner_body,
                        span: tspan,
                    };
                    let match_id = ctx.fresh_id(None);
                    let scrutinee_id = ctx.fresh_id(None);
                    IrExpr::Match {
                        id: match_id,
                        scrutinee: Box::new(IrExpr::Local {
                            id: scrutinee_id,
                            name: synth_name,
                            span: tspan,
                        }),
                        arms: vec![arm],
                        span: tspan,
                    }
                },
            );

            // Anonymous lambdas have no inferred_caps entry; fall back to PURE.
            let caps = ctx.lookup_inferred_caps(*span);
            IrExpr::Lambda {
                id,
                params: ir_params,
                body: Box::new(wrapped_body),
                caps,
                span: *span,
            }
        }

        // ── Record and union-variant construction ─────────────────────────────
        //
        // `Ctor { field = val, … }` or bare `Ctor` lowers to `IrExpr::Construct`.
        //
        // B-1 fix (Phase 5 followup): dispatch on the constructor's resolved
        // binding before emitting the IR shape:
        //
        // | Binding                          | Emitted ctor                        |
        // |----------------------------------|-------------------------------------|
        // | StdlibSymbol { "Ok"/"Err"/…  }  | SymbolRef::Prelude { name }         |
        // | Constructor { variant: 0, .. }  | SymbolRef::Constructor { Record }   |
        // | Constructor { variant > 0, .. } | SymbolRef::Constructor { UnionVar } |
        // | None / other                    | SymbolRef::Constructor { Record }   | (defensive)
        //
        // The prelude set is the closed set {Ok, Err, Some, None} plus the seven
        // JsonValue variants {JNull, JBool, JInt, JFloat, JText, JList, JObject}
        // (hard-coded in ridge-lower, not ridge-resolve).
        Expr::Record {
            constructor,
            fields,
            span,
        } => {
            let id = ctx.fresh_id(None);
            let ctor_name = record_ctor_name(constructor);
            let ctor_span = record_ctor_span(constructor);

            // Resolve the constructor binding from the BindingMap.
            let binding = ctx
                .node_id_map
                .as_ref()
                .and_then(|m| m.get(ctor_span, NodeKind::Ident))
                .and_then(|nid| {
                    ctx.binding_map
                        .and_then(|bm| bm.get(nid.0 as usize).and_then(Option::as_ref))
                })
                .cloned();

            let ir_fields: Vec<(String, IrExpr)> = fields
                .iter()
                .map(|fi| {
                    // Record shorthand: `{ age }` means `{ age = age }`.
                    let val_expr = fi
                        .value
                        .as_ref()
                        .map_or_else(|| Expr::Ident(fi.name.clone()), Clone::clone);
                    (fi.name.text.clone(), lower_expr(ctx, &val_expr))
                })
                .collect();

            // B-1: route to Prelude for the four stdlib prelude constructors.
            // OQ-PF001 resolved: hard-code the 4-name set inside ridge-lower.
            //
            // Two IR shapes are emitted depending on field presence:
            // - No fields (function-style, e.g. `Ok x` parsed as Record + Call):
            //   → `IrExpr::Symbol { Prelude("Ok") }` so that the outer `Call` node
            //   routes through Phase 6's `lower_prelude_call(args=[x])`.
            // - Fields present (record-style, e.g. `Some { value = x }`):
            //   → `IrExpr::Construct { Prelude("Some"), fields=[…] }` handled by
            //   Phase 6's `lower_construct_expr` (expects exactly 1 field).
            if matches!(
                &binding,
                Some(Binding::StdlibSymbol { name, .. })
                    if matches!(
                        name.as_str(),
                        "Ok" | "Err" | "Some" | "None"
                            | "JNull" | "JBool" | "JInt" | "JFloat" | "JText" | "JList" | "JObject"
                    )
            ) {
                if ir_fields.is_empty() {
                    // Function-style usage: emit as Symbol so Call routing works.
                    IrExpr::Symbol {
                        id,
                        sym: SymbolRef::Prelude { name: ctor_name },
                        span: *span,
                    }
                } else {
                    // Record-style usage: preserve fields in Construct.
                    IrExpr::Construct {
                        id,
                        ctor: SymbolRef::Prelude { name: ctor_name },
                        fields: ir_fields,
                        span: *span,
                    }
                }
            } else if let Some(Binding::Constructor {
                owner_type: sym_id,
                variant,
                is_record,
            }) = &binding
            {
                // User constructor (record auto-ctor or union variant).
                // Use the `is_record` flag carried by `Binding::Constructor`
                // which the resolver sets accurately based on the type body.
                let owner_type = ctx
                    .lookup_constructor_tycon(*sym_id)
                    .or_else(|| ctx.lookup_tycon_by_name(&ctor_name))
                    .unwrap_or(TyConId(0));
                let ctor_kind = if *is_record {
                    ridge_ir::CtorKind::Record
                } else {
                    ridge_ir::CtorKind::UnionVariant
                };
                IrExpr::Construct {
                    id,
                    ctor: SymbolRef::Constructor {
                        ctor_kind,
                        owner_type,
                        name: ctor_name,
                        variant: *variant,
                    },
                    fields: ir_fields,
                    span: *span,
                }
            } else {
                // Defensive fallback: no binding map or unrecognised binding.
                // OQ-PHASE45-007: fall back to lookup_tycon_by_name then TyConId(0).
                let owner_type = ctx.lookup_tycon_by_name(&ctor_name).unwrap_or(TyConId(0));
                IrExpr::Construct {
                    id,
                    ctor: SymbolRef::Constructor {
                        ctor_kind: ridge_ir::CtorKind::Record,
                        owner_type,
                        name: ctor_name,
                        variant: 0,
                    },
                    fields: ir_fields,
                    span: *span,
                }
            }
        }

        // ── Field access ──────────────────────────────────────────────────────
        //
        // `base.field` lowers to `IrExpr::Field { base, field: String }`.
        Expr::FieldAccess { base, field, span } => {
            let id = ctx.fresh_id(None);
            let base = Box::new(lower_expr(ctx, base));
            IrExpr::Field {
                id,
                base,
                field: field.text.clone(),
                span: *span,
            }
        }

        // ── Function call ─────────────────────────────────────────────────────
        //
        // `f x y z` lowers to `IrExpr::Call` with args in strict L→R order.
        //
        // B-3 fix (Phase 5 followup): partial-application detection.
        // If the callee's type is `Type::Fn { params, .. }` and
        // `args.len() < params.len()`, we wrap the `Call` in a synthetic
        // `Lambda` that supplies the remaining parameters.
        //
        // Callee type is looked up via `ctx.node_type` using the callee's
        // NodeId. For a bare ident callee the NodeId comes from
        // `node_id_map.get(callee_span, Ident)`. For other callee forms
        // the lookup may miss; in that case no wrapping is done (safe fallback).
        Expr::Call { callee, args, span } => {
            let id = ctx.fresh_id(None);
            // Look up callee type before lowering, while we still have the AST.
            let callee_node_type = lookup_callee_type(ctx, callee);
            let ir_callee = lower_expr(ctx, callee);
            let ir_args: Vec<IrExpr> = args.iter().map(|a| lower_expr(ctx, a)).collect();

            // Union-variant constructor application: `Circle 5` arrives as
            // `Expr::Call { callee: Expr::Record(Bare("Circle"), []), args: [5] }`.
            // After lowering the callee we get `IrExpr::Construct { ctor: UnionVariant, fields: [] }`.
            // Fold the call args into the construct fields so codegen can emit
            // the correct tagged tuple `{Circle, 5}` via the IrExpr::Construct path.
            let is_nullary_union_ctor = matches!(
                &ir_callee,
                IrExpr::Construct {
                    ctor: SymbolRef::Constructor {
                        ctor_kind: ridge_ir::CtorKind::UnionVariant,
                        ..
                    },
                    fields,
                    ..
                } if fields.is_empty()
            );
            if is_nullary_union_ctor {
                // Fold positional args into construct fields.
                // Codegen drops field names for UnionVariant, so empty-string
                // names are the correct convention (see codegen-erl expr.rs).
                let combined_fields: Vec<(String, IrExpr)> =
                    ir_args.into_iter().map(|a| (String::new(), a)).collect();
                let IrExpr::Construct { ctor, .. } = ir_callee else {
                    unreachable!("guarded by is_nullary_union_ctor match above");
                };
                return IrExpr::Construct {
                    id,
                    ctor,
                    fields: combined_fields,
                    span: *span,
                };
            }

            // Dictionary-passing: when the callee is a constrained fn,
            // prepend one dict argument per constraint in the callee's scheme.
            //
            // For `DictPlan::Static`: the concrete type was resolved by the
            // constraint solver. The dict arg references the module-level
            // instance dict constant `$inst_{ClassName}_{TypeName}`.
            //
            // For `DictPlan::Forward`: the caller is itself constrained for the
            // same class → forward the caller's own incoming dict param
            // `$dict_{ClassName}_{TyVar}`.
            //
            // When neither can be resolved (test scaffolding without a wired
            // workspace, or the callee has no constraint entry), the dict arg
            // falls back to `IrExpr::Lit(Unit)` — a defensive no-op that keeps
            // the compiler from crashing. Real programs will have all dict plans
            // resolved by the typecheck pass.
            // Collect each call argument's fully-resolved type so the dictionary
            // resolver can build the exact instance dictionary from the concrete
            // type flowing into the constrained parameter. The full type spine —
            // not just its head constructor — is required: a parametric instance
            // such as `Encode (Option a)` needs the element type to pick the
            // element dictionary, and two call sites that share a head (an
            // `Option Int` and an `Option Text`) must each get their own.
            let arg_types: Vec<Option<Type>> = args
                .iter()
                .map(|a| {
                    ctx.node_id_map
                        .as_ref()
                        .and_then(|m| m.get(a.span(), NodeKind::Expr))
                        .and_then(|nid| ctx.node_type(nid).cloned())
                })
                .collect();
            let dict_args = build_dict_args(ctx, &ir_callee, &arg_types, *span);
            let all_args: Vec<IrExpr> = dict_args.into_iter().chain(ir_args.clone()).collect();

            let call = IrExpr::Call {
                id,
                callee: Box::new(ir_callee),
                args: all_args,
                span: *span,
            };
            // B-3: wrap in a synthetic Lambda if partial application is detected.
            // Pass `ir_args` (user-only args, not dict args) for the arity check.
            wrap_partial_application_if_needed(ctx, call, &ir_args, callee_node_type, *span)
        }

        // ── Spawn actor ───────────────────────────────────────────────────────
        //
        // `spawn Actor arg*` lowers to `IrExpr::Spawn`.
        // OQ-PHASE45-006: actor module resolved via BindingMap → actor_module_cache
        // → ctx.module_id (three-step precedence; see resolve_actor_module).
        Expr::Spawn { actor, args, span } => {
            let id = ctx.fresh_id(None);
            // OQ-PHASE45-006: resolve actor module via three-step precedence.
            let actor_module = resolve_actor_module(ctx, actor);
            let args = args.iter().map(|a| lower_expr(ctx, a)).collect();
            IrExpr::Spawn {
                id,
                actor: SymbolRef::ActorType {
                    module: actor_module,
                    name: actor.text.clone(),
                },
                args,
                span: *span,
            }
        }

        // ── Send message to actor ─────────────────────────────────────────────
        //
        // `handle ! message` lowers to `IrExpr::Send`.
        //
        // The parser stores the whole right-hand side in `message: Box<Expr>`,
        // so `partner ! bounce x y` arrives here as
        // `Send { message: Call { callee: Ident("bounce"), args: [x, y] } }`
        // and `partner ! finish` as `Send { message: Ident("finish") }`.
        // The lowering peels that off into the IR's `(handler_name, args)` pair.
        //
        // OQ-PHASE45-006: actor_module falls back to ctx.module_id (same-module
        // dominant case). Authoritative cross-module resolution requires the
        // handle's typed binding.
        Expr::Send {
            handle,
            message,
            span,
        } => {
            let id = ctx.fresh_id(None);
            // OQ-PHASE45-006: actor_module resolved via current-module fallback.
            // Authoritative lookup requires reading the handle's typed binding
            // (Binding::Local + associated Handle X type).
            let actor_module = ctx.module_id;
            let handle = Box::new(lower_expr(ctx, handle));
            let (handler_name, msg_args) = unfold_send_message(message);
            // Treat `h ! name ()` as `h ! name` so a 0-arity handler decl
            // `on name ()` and the call form `name ()` produce the same
            // wire shape (a bare `{name}` tag tuple).
            let drop_unit = msg_args.len() == 1 && matches!(msg_args[0], Expr::Unit(_));
            let args = if drop_unit {
                Vec::new()
            } else {
                msg_args.iter().map(|a| lower_expr(ctx, a)).collect()
            };
            IrExpr::Send {
                id,
                handle,
                message: SymbolRef::Handler {
                    actor_module,
                    // PHASE45-Group-B: Binding::Local carries no type info at resolve
                    // time; actor name cannot be recovered from the binding alone.
                    // OQ-PHASE45-007: stays String::new() — no regression from prior state.
                    actor: String::new(),
                    handler: handler_name,
                },
                args,
                span: *span,
            }
        }

        // ── Ask actor (synchronous request) ──────────────────────────────────
        //
        // `handle ?> message arg* [timeout <ms|never>]` lowers to `IrExpr::Ask`.
        // OQ-PHASE45-006: actor_module falls back to ctx.module_id (same-module
        // dominant case). Authoritative cross-module resolution requires typed
        // binding access.
        //
        // Phase 6 T0 (OQ-E001): the `timeout` field is lowered 1:1 from the AST.
        // Existing Phase 5 `Expr::Ask` nodes (without `timeout`) default to `None`
        // in the AST and produce `timeout: None` in IR — strict additivity preserved.
        Expr::Ask {
            handle,
            message,
            args,
            timeout,
            span,
        } => {
            let id = ctx.fresh_id(None);
            // OQ-PHASE45-006: actor_module resolved via current-module fallback.
            // Authoritative lookup requires reading the handle's typed binding
            // (Binding::Local + associated Handle X type).
            let actor_module = ctx.module_id;
            let handle = Box::new(lower_expr(ctx, handle));
            // Treat `h ?> name ()` as `h ?> name` so a 0-arity handler decl
            // `on name ()` and the call form `name ()` produce the same wire
            // shape (a bare `{name}` tag tuple).
            let drop_unit = args.len() == 1 && matches!(&args[0], Expr::Unit(_));
            let args = if drop_unit {
                Vec::new()
            } else {
                args.iter().map(|a| lower_expr(ctx, a)).collect()
            };

            // Lower AST AskTimeout → IR IrTimeout 1:1.
            // The wildcard arm is required by #[non_exhaustive] on AskTimeout.
            #[allow(clippy::match_same_arms)]
            let ir_timeout = timeout.as_ref().map(|t| match t {
                ridge_ast::AskTimeout::Never => ridge_ir::IrTimeout::Never,
                ridge_ast::AskTimeout::Millis(ms_expr) => {
                    ridge_ir::IrTimeout::Millis(Box::new(lower_expr(ctx, ms_expr)))
                }
                // #[non_exhaustive] guard — defensive catch for future variants.
                _ => ridge_ir::IrTimeout::Never,
            });

            IrExpr::Ask {
                id,
                handle,
                message: SymbolRef::Handler {
                    actor_module,
                    // PHASE45-Group-B: Binding::Local carries no type info at resolve
                    // time; actor name cannot be recovered from the binding alone.
                    // OQ-PHASE45-007: stays String::new() — no regression from prior state.
                    actor: String::new(),
                    handler: message.text.clone(),
                },
                args,
                timeout: ir_timeout,
                span: *span,
            }
        }

        // ── Inline record literal ─────────────────────────────────────────────
        //
        // `{ f = v, … }` lowers to `IrExpr::Construct { Record, owner = anon_id }`.
        // The codegen layer drops the constructor tag for Record-kind constructs and
        // emits a BEAM map — no codegen change needed.
        Expr::RecordLit { fields, span } => {
            // Step 1: lower each field value.
            let ir_fields: Vec<(String, IrExpr)> = fields
                .iter()
                .map(|fi| {
                    let val_expr = fi
                        .value
                        .as_ref()
                        .map_or_else(|| Expr::Ident(fi.name.clone()), Clone::clone);
                    (fi.name.text.clone(), lower_expr(ctx, &val_expr))
                })
                .collect();

            // Step 2: look up the anon TyConId from the typecheck node-type table.
            // The typecheck pass stamped `Type::Con(anon_id, [])` for this expression
            // under NodeKind::Expr.
            let anon_id: TyConId = ctx
                .node_id_map
                .as_ref()
                .and_then(|m| m.get(*span, NodeKind::Expr))
                .and_then(|nid| ctx.node_type(nid))
                .and_then(|ty| {
                    if let Type::Con(id, _) = ty {
                        Some(*id)
                    } else {
                        None
                    }
                })
                // Defensive fallback for unit-test scaffolding that does not
                // wire the node-type table.  TyConId(0) is the `Int` builtin in
                // the normal arena; under test scaffolding the arena is usually
                // empty and this produces no-op IR.
                .unwrap_or(TyConId(0));

            // Step 3: look up the anon decl name for the SymbolRef.
            let anon_name = ctx
                .workspace
                .and_then(|ws| ws.tycons.get(anon_id.0 as usize))
                .map_or_else(
                    || format!("{{anon record #{}}}", anon_id.0),
                    |d| d.name.clone(),
                );

            let id = ctx.fresh_id(None);
            IrExpr::Construct {
                id,
                ctor: SymbolRef::Constructor {
                    ctor_kind: ridge_ir::CtorKind::Record,
                    owner_type: anon_id,
                    name: anon_name,
                    variant: 0,
                },
                fields: ir_fields,
                span: *span,
            }
        }
    }
}

// ── Public dispatcher — patterns ──────────────────────────────────────────────

/// Lower a single [`Pattern`] node to its [`IrPat`] equivalent.
///
/// Delegates to [`lower_pattern_full`] from `match_lower` for all patterns,
/// which handles the full lowering table (§4.8.1) including `Constructor`,
/// `Tuple`, `Cons`, `As`, and `Paren` patterns.
pub fn lower_pattern(ctx: &mut LowerCtx<'_>, pat: &Pattern) -> IrPat {
    lower_pattern_full(ctx, pat)
}

// ── Actor-module resolution helper (§3.1 / OQ-PHASE45-006) ──────────────────

/// Resolve an actor ident to its declaring `ModuleId` using the three-step
/// precedence from plan §3.1 step 3.
///
/// 1. **`BindingMap` first** — `ctx.binding_map.get(actor_ident.node_id)` →
///    `Some(Binding::ActorName { module, .. })` → use that `module`.
/// 2. **Bare-name cache fallback** — `ctx.lookup_actor_module(&actor_ident.text)`.
/// 3. **Current module final fallback** — `ctx.module_id`.
///
/// // OQ-PHASE45-006: bare-name cache is a fallback; `BindingMap` is authoritative.
fn resolve_actor_module(ctx: &mut LowerCtx<'_>, actor_ident: &ridge_ast::Ident) -> ModuleId {
    // Step 1: BindingMap — look up by the ident's NodeId (requires node_id_map).
    let binding_module: Option<ModuleId> = ctx
        .node_id_map
        .as_ref()
        .and_then(|m| m.get(actor_ident.span, NodeKind::Ident))
        .and_then(|nid| {
            ctx.binding_map
                .and_then(|bm| bm.get(nid.0 as usize).and_then(Option::as_ref))
        })
        .and_then(|binding| {
            if let ridge_resolve::imports::Binding::ActorName { module, .. } = binding {
                Some(*module)
            } else {
                None
            }
        });

    if let Some(module) = binding_module {
        return module;
    }

    // Step 2: bare-name cache.
    if let Some(module) = ctx.lookup_actor_module(&actor_ident.text) {
        return module;
    }

    // Step 3: current-module fallback.
    ctx.module_id
}

// ── Dictionary-passing helpers ────────────────────────────────────────────────

/// Build the implicit dictionary arguments to prepend before user args when
/// calling a constrained function.
///
/// Inspects the lowered callee: when it is a `SymbolRef::Local` whose fn has
/// constraints in the workspace scheme table, one dict arg is produced per
/// constraint.
///
/// Resolution strategy (in order):
///
/// 1. **Forward** (`DictPlan::Forward`): the caller is itself constrained for
///    the same class. The dict arg is the caller's own incoming dict param
///    `$dict_{ClassName}_{TyVar}`.
/// 2. **Static** (`DictPlan::Static`): the constraint's variable was pinned to a
///    concrete type by the argument flowing into the constrained parameter. The
///    dictionary is built from that resolved type — the full `Type::Con` spine,
///    recursively — so a parametric instance receives the correct element
///    dictionaries.
/// 3. **Fallback**: no resolution available (test scaffolding without a wired
///    workspace). Returns `IrExpr::Lit(Unit)` as a defensive placeholder.
fn build_dict_args(
    ctx: &mut LowerCtx<'_>,
    callee: &IrExpr,
    arg_types: &[Option<Type>],
    span: Span,
) -> Vec<IrExpr> {
    // Only `SymbolRef::Local` callees can be constrained top-level fns.
    let callee_name = match callee {
        IrExpr::Symbol {
            sym: SymbolRef::Local { name, .. },
            ..
        } => name.clone(),
        _ => return vec![],
    };

    // Look up the callee's constraints. Returns `&[]` for unknown fns.
    let constraints = ctx.lookup_fn_constraints(&callee_name).to_vec();
    if constraints.is_empty() {
        return vec![];
    }
    // The callee scheme's parameter types tell us which argument pins each
    // constraint variable.
    let param_types = ctx.lookup_fn_param_types(&callee_name).to_vec();

    let mut dict_args: Vec<IrExpr> = Vec::with_capacity(constraints.len());

    for c in &constraints {
        let class_name = ctx.class_name(c.class).unwrap_or("Unknown").to_owned();

        // The concrete type the constraint variable was unified to at this call
        // site: walk the scheme's parameter types in lockstep with the resolved
        // argument types, find where `c.ty` appears, and read off the matching
        // sub-type. `None` when the variable cannot be located (no type info).
        let constraint_ty =
            constraint_arg_type(&param_types, arg_types, c.ty).map(|ty| deep_peel_alias(&ty));

        let dict_expr = resolve_dict_arg(ctx, c.class, &class_name, constraint_ty.as_ref(), span);
        dict_args.push(dict_expr);
    }

    dict_args
}

/// Peel transparent alias wrappers from a type, leaving the underlying shape.
fn deep_peel_alias(ty: &Type) -> Type {
    match ty {
        Type::Alias { body, .. } => deep_peel_alias(body),
        other => other.clone(),
    }
}

/// Find the concrete type a constraint variable was unified to at a call site.
///
/// Walks each scheme parameter type alongside the resolved type of the
/// corresponding call argument. When a scheme parameter mentions `var` (e.g. the
/// `a` in a `List a` parameter), the structurally-aligned position in the
/// resolved argument type is the concrete type that satisfies the constraint
/// (the element of a `List Int`, or the whole argument when the parameter is a
/// bare `a`). Returns `None` when the variable is not found or type information
/// is missing.
fn constraint_arg_type(
    param_types: &[Type],
    arg_types: &[Option<Type>],
    var: ridge_types::TyVid,
) -> Option<Type> {
    for (param, arg) in param_types.iter().zip(arg_types.iter()) {
        let Some(arg_ty) = arg else { continue };
        if let Some(found) = align_var(param, arg_ty, var) {
            return Some(found);
        }
    }
    None
}

/// Structurally align a scheme parameter type with a resolved argument type to
/// extract the sub-type sitting at `var`'s position.
///
/// `List a` vs `List Int` with `var = a` yields `Int`; a bare `a` vs `Option Int`
/// yields the whole `Option Int`. Returns `None` when `var` does not occur in
/// `param` or the two shapes do not align.
fn align_var(param: &Type, arg: &Type, var: ridge_types::TyVid) -> Option<Type> {
    match param {
        Type::Var(v) if *v == var => Some(arg.clone()),
        Type::Var(_) => None,
        Type::Alias { body, .. } => align_var(body, arg, var),
        _ => {
            let arg = deep_peel_alias(arg);
            match (param, &arg) {
                (Type::Con(_, pargs), Type::Con(_, aargs)) => pargs
                    .iter()
                    .zip(aargs.iter())
                    .find_map(|(p, a)| align_var(p, a, var)),
                (Type::Tuple(ps), Type::Tuple(as_)) => ps
                    .iter()
                    .zip(as_.iter())
                    .find_map(|(p, a)| align_var(p, a, var)),
                (
                    Type::Fn {
                        params: pps,
                        ret: pret,
                        ..
                    },
                    Type::Fn {
                        params: aps,
                        ret: aret,
                        ..
                    },
                ) => pps
                    .iter()
                    .zip(aps.iter())
                    .find_map(|(p, a)| align_var(p, a, var))
                    .or_else(|| align_var(pret, aret, var)),
                _ => None,
            }
        }
    }
}

/// Resolve the dictionary value for one constraint at a call site.
///
/// Forwards the caller's own incoming dict param when the caller is constrained
/// for the same class; otherwise builds the dictionary directly from the
/// concrete type that pinned the constraint, recursing through the instance
/// registry so parametric instances receive their element dictionaries. Falls
/// back to a unit literal when neither path applies (test scaffolding).
fn resolve_dict_arg(
    ctx: &mut LowerCtx<'_>,
    class: ridge_types::ClassId,
    class_name: &str,
    constraint_ty: Option<&Type>,
    span: Span,
) -> IrExpr {
    // Determine whether the CALLER is itself constrained for this class.
    // If so, use the Forward path (forward the caller's own incoming dict param).
    // This is the correct dispatch for polymorphic call sites:
    //   - `fn announce (x: a) -> Text where Show a = describe x` → forward
    //   - `fn main_static () -> Text = describe Red` → no caller constraint → Static
    let caller_constraint = ctx
        .current_fn_constraints
        .iter()
        .find(|c| c.class == class)
        .cloned();

    if let Some(c) = caller_constraint {
        let id = ctx.fresh_id(None);
        return IrExpr::Local {
            id,
            name: format!("$dict_{class_name}_{}", c.ty.0),
            span,
        };
    }

    // The caller is not constrained for this class — it is a monomorphic call
    // site. Build the dictionary plan from the concrete type the argument pinned
    // the constraint to, then lower it. This recurses through the type's spine
    // so a parametric instance gets the right element dictionaries, and two call
    // sites sharing a head constructor each get their own.
    if let Some(ty) = constraint_ty {
        if let Some(plan) = build_dict_plan_from_type(ctx, class, ty) {
            return dict_plan_to_expr(ctx, class, plan, class_name, span);
        }
    } else if let Some(plan) = single_static_plan_for_class(ctx, class) {
        // No pinning type is available (a bare class-method reference used as a
        // value, with no enclosing constraint). Fall back to the sole Static
        // plan for the class in this module's resolution table. This keeps the
        // single-instance dispatch that predates parametric instances working;
        // an ambiguous multi-plan case here would already be flagged upstream.
        return dict_plan_to_expr(ctx, class, plan, class_name, span);
    }

    // Defensive no-op: emit a unit literal. This should not happen in
    // well-typed programs — a typecheck error would fire for missing instances.
    let id = ctx.fresh_id(None);
    IrExpr::Lit {
        id,
        value: ridge_ir::IrLit::Unit,
        span,
    }
}

/// The unique `DictPlan::Static` for `class` in the current module's resolution
/// table, if exactly one exists. Used only when no argument type is available to
/// pin the constraint (a bare class-method value). Returns `None` when there is
/// no plan or more than one (ambiguous).
fn single_static_plan_for_class(
    ctx: &LowerCtx<'_>,
    class: ridge_types::ClassId,
) -> Option<ridge_typecheck::DictPlan> {
    let tmod = ctx
        .workspace
        .and_then(|ws| ws.modules.get(ctx.module_id.0 as usize))?;
    let mut found: Option<&ridge_typecheck::DictPlan> = None;
    for ((cid, _), plan) in &tmod.dict_resolution {
        if *cid == class && matches!(plan, ridge_typecheck::DictPlan::Static { .. }) {
            if found.is_some() {
                return None; // more than one — ambiguous, do not guess
            }
            found = Some(plan);
        }
    }
    found.cloned()
}

/// Build a [`ridge_typecheck::DictPlan`] for `(class, ty)` directly from a
/// resolved type, recursing through the instance registry.
///
/// This is the lowering-side counterpart to the solver's plan resolution: it
/// reads the registered instance for the type's head constructor, then resolves
/// each context constraint against the concrete type argument at the recorded
/// head position. A `List Int` yields `Static { List, [Static { Int }] }`; a
/// `Result Int Text` yields two element plans; a nested `List (Option Int)`
/// nests. A bare type variable yields a `Forward` (the enclosing scope threads
/// the dictionary). Returns `None` when no instance is registered for the head —
/// a typecheck error (T029) would already have fired for that case.
fn build_dict_plan_from_type(
    ctx: &LowerCtx<'_>,
    class: ridge_types::ClassId,
    ty: &Type,
) -> Option<ridge_typecheck::DictPlan> {
    use ridge_typecheck::DictPlan;

    match deep_peel_alias(ty) {
        Type::Con(tycon, args) => {
            let env = ctx.instance_env?;
            let info = env.get((class, tycon))?;
            // Resolve one sub-dictionary per context constraint, reading the
            // concrete type argument at the constraint's recorded head position.
            let mut sub_dicts: Vec<DictPlan> = Vec::with_capacity(info.ctx_constraints.len());
            for (ctx_c, &pos) in info
                .ctx_constraints
                .iter()
                .zip(info.head_var_positions.iter())
            {
                let elem_ty = args.get(pos).cloned().unwrap_or(Type::Error);
                let sub =
                    build_dict_plan_from_type(ctx, ctx_c.class, &elem_ty).unwrap_or_else(|| {
                        // The element resolved to a variable (or no instance):
                        // forward a dictionary parameter. For a well-typed program
                        // this is a genuine forward; a truly unsatisfiable element
                        // is reported as T030 by the solver before lowering runs.
                        DictPlan::Forward(ridge_types::Constraint {
                            class: ctx_c.class,
                            ty: forward_var_of(&elem_ty),
                        })
                    });
                sub_dicts.push(sub);
            }
            Some(DictPlan::Static {
                info: Box::new(info.clone()),
                tycon,
                args: sub_dicts,
            })
        }
        // Neither a bare type variable nor any non-constructor shape resolves to
        // a concrete instance here.
        _ => None,
    }
}

/// The forward variable to thread for an unresolved element type. When the
/// element is itself a type variable, forward that variable; otherwise use the
/// sentinel `TyVid(0)` (the dictionary is built structurally and the variable is
/// not consulted).
fn forward_var_of(ty: &Type) -> ridge_types::TyVid {
    match deep_peel_alias(ty) {
        Type::Var(v) => v,
        _ => ridge_types::TyVid(0),
    }
}

/// Convert a resolved [`DictPlan`] to the `IrExpr` that threads the dictionary.
///
/// `class` is the [`ClassId`] the dictionary satisfies — needed to recognise
/// the prelude `Encode`/`Decode` instances, whose dictionaries are synthesised
/// inline (see [`crate::prelude_dict`]) because they have no module-level
/// `$inst_` constant.
fn dict_plan_to_expr(
    ctx: &mut LowerCtx<'_>,
    class: ridge_types::ClassId,
    plan: ridge_typecheck::DictPlan,
    class_name: &str,
    span: Span,
) -> IrExpr {
    use ridge_typecheck::DictPlan;
    match plan {
        DictPlan::Static { tycon, args, .. } => {
            // Recursively lower the sub-dictionaries first. For a parametric
            // instance `Encode (List a)` the args carry the element dict plan;
            // each sub-dict is resolved through the SAME class (the context
            // constraint shares the head class for the codec instances).
            let sub_dicts: Vec<IrExpr> = args
                .into_iter()
                .map(|sub| dict_plan_to_expr(ctx, class, sub, class_name, span))
                .collect();

            // Prelude-reserved `Encode`/`Decode` instances (the JSON primitives
            // and the `List`/`Option`/`Map`/`Result` containers) have no runtime
            // `$inst_` value — the deriving path inlines their behaviour and the
            // container instances are registered in Rust with no source body.
            // Synthesise the dictionary map inline at the use site instead, with
            // the already-lowered element dicts threaded in. This is what makes
            // `List Int` / `Option Text` / etc. run.
            if crate::prelude_dict::is_prelude_codec_instance(class, tycon) {
                return crate::prelude_dict::synth_prelude_dict(ctx, class, tycon, sub_dicts, span)
                    .unwrap_or_else(|| {
                        // `is_prelude_codec_instance` already matched, so synth
                        // returns Some; this branch is unreachable in practice.
                        let id = ctx.fresh_id(None);
                        IrExpr::Lit {
                            id,
                            value: ridge_ir::IrLit::Unit,
                            span,
                        }
                    });
            }

            // A user-defined instance: reference its module-level `$inst_`
            // constant. For a hand-written *parametric* instance the constant is
            // a function of the element dict(s), so apply it to `sub_dicts`
            // (dict-of-dicts). A non-parametric instance has no sub-dicts and the
            // bare symbol is the dictionary map.
            let type_name = ctx
                .workspace
                .and_then(|ws| ws.tycons.get(tycon.0 as usize))
                .map_or_else(|| format!("TyCon{}", tycon.0), |decl| decl.name.clone());
            let dict_const_name = format!("$inst_{class_name}_{type_name}");
            let id = ctx.fresh_id(None);
            let dict_symbol = IrExpr::Symbol {
                id,
                sym: SymbolRef::Local {
                    name: dict_const_name,
                    module: ctx.module_id,
                },
                span,
            };
            if sub_dicts.is_empty() {
                return dict_symbol;
            }
            let call_id = ctx.fresh_id(None);
            IrExpr::Call {
                id: call_id,
                callee: Box::new(dict_symbol),
                args: sub_dicts,
                span,
            }
        }
        DictPlan::Forward(c) => {
            let id = ctx.fresh_id(None);
            IrExpr::Local {
                id,
                name: format!("$dict_{class_name}_{}", c.ty.0),
                span,
            }
        }
    }
}

// ── Lambda, record, and call helpers ─────────────────────────────────────────

/// Convert an AST [`LambdaParam`] to an [`IrParam`].
///
/// For `Annotated` params the declared type annotation is lowered via
/// [`lower_ast_type`].  For bare `Pattern` params the type is resolved by
/// looking up the parent lambda's [`Type::Fn`] via
/// `ctx.node_id_map.get(lambda_span, NodeKind::Expr)` → `ctx.node_type(nid)`
/// and indexing `params[param_idx]`.  Falls back to `Type::Error` when the
/// mapping is absent (test scaffolding or unannotated lambda not in the
/// side-table).
///
/// Bare param type lifted from parent lambda's `Type::Fn` params[i].
fn lambda_param_to_ir_param(
    ctx: &mut LowerCtx<'_>,
    lambda_span: Span,
    param_idx: usize,
    param: &LambdaParam,
) -> IrParam {
    match param {
        LambdaParam::Pattern(pat) => {
            // Extract name from bare pattern; fall back to "_" for non-Var shapes.
            let (name, param_span) = match pat {
                Pattern::Var { name, .. } => (name.text.clone(), name.span),
                other => ("_".to_owned(), other.span()),
            };
            // Look up the parent lambda's Type::Fn from node_types via
            // (lambda_span, NodeKind::Expr), then pick params[param_idx].
            // node_types is keyed by Expr positions only — param ident spans carry
            // no type entry; the correct source is the enclosing lambda's Fn type.
            let ty = ctx
                .node_id_map
                .as_ref()
                .and_then(|m| m.get(lambda_span, NodeKind::Expr))
                .and_then(|nid| ctx.node_type(nid).cloned())
                .and_then(|fn_ty| {
                    if let Type::Fn { params, .. } = fn_ty {
                        params.into_iter().nth(param_idx)
                    } else {
                        None
                    }
                })
                .unwrap_or(Type::Error);
            IrParam {
                name,
                ty,
                span: param_span,
            }
        }
        LambdaParam::Annotated { pat, ty, span } => {
            let name = match pat {
                Pattern::Var { name, .. } => name.text.clone(),
                _ => "_".to_owned(),
            };
            IrParam {
                name,
                ty: lower_ast_type(ctx, ty),
                span: *span,
            }
        }
    }
}

// ── B-3 helpers (partial application detection) ──────────────────────────────

/// Look up the resolved `Type` for the callee AST `Expr`.
///
/// Phase 4's `infer_expr` stamps the resolved type under `NodeKind::Expr` for
/// every expression node (line 84 of `infer.rs`: `write_node_type(span, Expr, ty)`).
/// So we always use `NodeKind::Expr` with the callee's span, regardless of the
/// concrete AST variant.
///
/// For all other shapes (e.g. field access, call) returns `None` — partial-app
/// wrapping only fires for simple ident/qualified-name callees.
fn lookup_callee_type(ctx: &LowerCtx<'_>, callee: &Expr) -> Option<Type> {
    let span = match callee {
        Expr::Ident(id) => id.span,
        Expr::Qualified(qname) => qname.span,
        Expr::Paren { inner, .. } => return lookup_callee_type(ctx, inner),
        _ => return None,
    };
    let nid = ctx
        .node_id_map
        .as_ref()
        .and_then(|m| m.get(span, NodeKind::Expr))?;
    ctx.node_type(nid).cloned()
}

/// If `call` is a partial application (args supplied < callee arity), wrap it
/// in a synthetic `Lambda` that supplies the remaining parameters.
///
/// B-3: `meetsThreshold threshold` where `meetsThreshold : A -> B -> Bool`
/// becomes `Lambda { [__pa_0: B], Call { meetsThreshold, [threshold, __pa_0] } }`.
///
/// No wrapping occurs when:
/// - `callee_ty` is `None` (no type information).
/// - `callee_ty` is not `Type::Fn`.
/// - `applied_args.len() >= callee_fn_params.len()` (full or over-application).
///
/// This preserves the no-information-loss invariant: when types are unknown
/// the current behaviour is preserved.
fn wrap_partial_application_if_needed(
    ctx: &mut LowerCtx<'_>,
    call: IrExpr,
    applied_args: &[IrExpr],
    callee_ty: Option<Type>,
    span: Span,
) -> IrExpr {
    let Some(Type::Fn {
        params: fn_params,
        caps: cap_row,
        ..
    }) = callee_ty
    else {
        return call;
    };
    let applied = applied_args.len();
    let total = fn_params.len();
    if applied >= total {
        // Full or over-application — unchanged.
        return call;
    }
    // Extract CapabilitySet from the CapRow; fall back to PURE for row variables.
    let caps = match cap_row {
        CapRow::Concrete(cs) => cs,
        CapRow::Var(_) | _ => ridge_types::CapabilitySet::PURE,
    };
    // Remaining params become synthetic lambda params.
    let extra_tys = fn_params.into_iter().skip(applied);
    let mut synth_params: Vec<IrParam> = Vec::new();
    let mut extra_locals: Vec<IrExpr> = Vec::new();
    for ty in extra_tys {
        let name = ctx.fresh_local("__pa");
        let local_id = ctx.fresh_id(None);
        extra_locals.push(IrExpr::Local {
            id: local_id,
            name: name.clone(),
            span,
        });
        synth_params.push(IrParam { name, ty, span });
    }

    // Rebuild the Call to include the extra locals as additional args.
    let lambda_id = ctx.fresh_id(None);
    let call_id = ctx.fresh_id(None);
    match call {
        IrExpr::Call { callee, args, .. } => {
            let mut all = args;
            all.extend(extra_locals);
            let inner_call = IrExpr::Call {
                id: call_id,
                callee,
                args: all,
                span,
            };
            IrExpr::Lambda {
                id: lambda_id,
                params: synth_params,
                body: Box::new(inner_call),
                caps,
                span,
            }
        }
        other => {
            // Not a Call shape (defensive) — return unchanged.
            other
        }
    }
}

/// Extract the handler name string from an AST message expression.
///
/// For `handle ! message` the AST `message` field is `Box<Expr>`.  The most
/// common form is `Expr::Ident(name)`.  Falls back to the empty string for
/// non-ident shapes (e.g. dynamic sends) so the IR is still structurally valid.
///
/// The handler name is extracted from the message expression directly; the actor
/// module is resolved via the three-step `resolve_actor_module` precedence
/// (`BindingMap` → bare-name cache → current-module fallback) — Group B 3.1.
// PHASE45-Group-B: handler name extracted from ident; module resolved via BindingMap.
/// Decompose the message expression of a `Send` into its handler name and
/// argument list.
///
/// The parser stores `handle ! tag arg1 arg2` as
/// `Send { message: Call { callee: Ident("tag"), args: [arg1, arg2] } }` and
/// `handle ! tag` as `Send { message: Ident("tag") }`. Anything else
/// (qualified names, computed expressions) falls back to an empty handler
/// name and no args — same best-effort behaviour as before but applied at a
/// shallower depth.
fn unfold_send_message(expr: &Expr) -> (String, Vec<&Expr>) {
    match expr {
        Expr::Ident(ident) => (ident.text.clone(), Vec::new()),
        Expr::Call { callee, args, .. } => match callee.as_ref() {
            Expr::Ident(ident) => (ident.text.clone(), args.iter().collect()),
            _ => (String::new(), Vec::new()),
        },
        _ => (String::new(), Vec::new()),
    }
}

/// Extract the constructor name string from a [`RecordCtor`].
///
/// For bare constructors (`User { … }`) returns `"User"`.
/// For qualified constructors (`Http.Response { … }`) returns the last
/// segment name (`"Response"`).
fn record_ctor_name(ctor: &RecordCtor) -> String {
    match ctor {
        RecordCtor::Bare(ident) => ident.text.clone(),
        RecordCtor::Qualified(qname) => qname
            .segments
            .last()
            .map_or_else(String::new, |s| s.text.clone()),
    }
}

/// Extract the span of the constructor ident from a [`RecordCtor`].
///
/// For bare constructors returns the ident's span.
/// For qualified constructors returns the span of the last segment (the
/// constructor name proper, not the module prefix). Used by the `BindingMap`
/// path in record-constructor `TyConId` resolution (§3.2 / OQ-PHASE45-007).
fn record_ctor_span(ctor: &RecordCtor) -> ridge_ast::Span {
    match ctor {
        RecordCtor::Bare(ident) => ident.span,
        RecordCtor::Qualified(qname) => {
            qname.segments.last().map_or_else(|| qname.span, |s| s.span)
        }
    }
}

// ── Literal lowering ─────────────────────────────────────────────────────────

/// Lower an AST [`Literal`] to an [`IrExpr::Lit`].
///
/// Integer variants (`IntDec`, `IntBin`, `IntOct`, `IntHex`) are parsed to
/// `i64`.  On parse failure a defensive `L999` error is emitted and
/// `IrLit::Int(0)` is returned so the surrounding expression tree is still
/// structurally valid.
fn lower_literal(ctx: &mut LowerCtx<'_>, lit: &Literal) -> IrExpr {
    let span = lit.span();
    let id = ctx.fresh_id(None);
    let value = match lit {
        Literal::IntDec { raw, .. } => parse_int_dec(ctx, raw, span),
        Literal::IntBin { raw, .. } => parse_int_bin(ctx, raw, span),
        Literal::IntOct { raw, .. } => parse_int_oct(ctx, raw, span),
        Literal::IntHex { raw, .. } => parse_int_hex(ctx, raw, span),
        Literal::Float { raw, .. } => parse_float(ctx, raw, span),
        Literal::Bool { value, .. } => IrLit::Bool(*value),
        Literal::Text { raw, .. } => IrLit::Text(strip_text_quotes(raw)),
        // Raw strings carry literal bytes; escape decoding must be skipped.
        Literal::RawText { raw, .. } => IrLit::Text(raw.clone()),
    };
    IrExpr::Lit { id, value, span }
}

// ── Ident lowering ────────────────────────────────────────────────────────────

/// Lower an [`Ident`] atom to `IrExpr::Local` (for locals) or `IrExpr::Symbol`
/// (for module symbols, stdlib, prelude).
///
/// Resolves via the `BindingMap` attached to `ctx`.  If the binding map is
/// absent (test scaffold) or the `NodeId` is not found, a defensive `L999` is
/// emitted and a `Local` reference is returned (structurally valid for
/// downstream passes).
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive dispatch over all Binding variants with defensive fallbacks"
)]
fn lower_ident(ctx: &mut LowerCtx<'_>, ident: &Ident) -> IrExpr {
    let span = ident.span;

    // B-5 fix (Phase 5 followup): state-field-read precedence rule.
    //
    // Inside an actor handler/init body, a bare ident that names an actor
    // state field must emit `IrExpr::Field { base: Local("__state"), field }`.
    // Phase 6's `lower_field` turns that into `maps:get('field', V_State)`.
    //
    // OQ-PF002 resolved: the synthetic base local is "__state" — Phase 6 mangles
    // it to V_State via name_to_erl_var("__state") = "V_State".
    //
    // OQ-PF003 resolved: the state-field name wins unconditionally over all
    // other bindings when in_actor_body == true AND the name is in
    // current_state_fields. Phase 4 is expected to prevent param/state-field
    // name collision; if it allows the collision, shadowing semantics give the
    // state-field priority (matching the write-side precedence in block.rs).
    if ctx.in_actor_body
        && ctx
            .current_state_fields
            .as_ref()
            .is_some_and(|s| s.contains(ident.text.as_str()))
    {
        let field_id = ctx.fresh_id(None);
        let base_id = ctx.fresh_id(None);
        return IrExpr::Field {
            id: field_id,
            base: Box::new(IrExpr::Local {
                id: base_id,
                name: "__state".to_owned(),
                span,
            }),
            field: ident.text.clone(),
            span,
        };
    }

    // Look up NodeId for this ident span.
    let node_id = ctx
        .node_id_map
        .as_ref()
        .and_then(|m| m.get(span, NodeKind::Ident));

    let binding = node_id.and_then(|nid| {
        ctx.binding_map
            .and_then(|bm| bm.get(nid.0 as usize).and_then(Option::as_ref))
    });

    match binding {
        None => {
            // Either no binding map is attached or the ident has no binding entry.
            // Emit a defensive error and fall back to a Local reference so
            // subsequent passes have something structurally valid.
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!(
                    "no binding found for ident `{}` at {span:?}; binding map absent or NodeId missing",
                    ident.text
                ),
            });
            IrExpr::Local {
                id,
                name: ident.text.clone(),
                span,
            }
        }

        Some(Binding::Local(_local_id)) => {
            // A let-bound, fn-param, lambda-param, or pattern-bound local.
            let id = ctx.fresh_id(None);
            IrExpr::Local {
                id,
                name: ident.text.clone(),
                span,
            }
        }

        Some(Binding::ModuleSymbol { module, symbol: _ }) => {
            // A top-level symbol in the current module.  Use the ident text as
            // the canonical name — it is the source name for module symbols.
            let id = ctx.fresh_id(None);
            IrExpr::Symbol {
                id,
                sym: SymbolRef::Local {
                    name: ident.text.clone(),
                    module: *module,
                },
                span,
            }
        }

        Some(Binding::ImportedSymbol {
            module, symbol: _, ..
        }) => {
            // A symbol imported from another module (same project or external).
            let id = ctx.fresh_id(None);
            IrExpr::Symbol {
                id,
                sym: SymbolRef::External {
                    module: *module,
                    name: ident.text.clone(),
                },
                span,
            }
        }

        Some(Binding::StdlibSymbol {
            module: stdlib_id,
            name,
        }) => {
            let id = ctx.fresh_id(None);
            // B-1 fix (Phase 5 followup): the closed prelude constructor set
            // (Option/Result plus the seven JsonValue variants) must emit
            // SymbolRef::Prelude so Phase 6 can emit the correct Erlang
            // tuple/atom form. All other stdlib symbols remain Stdlib.
            if matches!(
                name.as_str(),
                "Ok" | "Err"
                    | "Some"
                    | "None"
                    | "JNull"
                    | "JBool"
                    | "JInt"
                    | "JFloat"
                    | "JText"
                    | "JList"
                    | "JObject"
            ) {
                IrExpr::Symbol {
                    id,
                    sym: SymbolRef::Prelude { name: name.clone() },
                    span,
                }
            } else {
                let module_name = stdlib_module_name(*stdlib_id);
                IrExpr::Symbol {
                    id,
                    sym: SymbolRef::Stdlib {
                        module: module_name,
                        name: name.clone(),
                    },
                    span,
                }
            }
        }

        Some(Binding::ActorName { module, .. }) => {
            // Actor names resolve to an `ActorType` symbol.
            let id = ctx.fresh_id(None);
            IrExpr::Symbol {
                id,
                sym: SymbolRef::ActorType {
                    module: *module,
                    name: ident.text.clone(),
                },
                span,
            }
        }

        Some(Binding::ModuleAlias { .. }) => {
            // A module-alias identifier (`List`, `Io`, …) is not a value
            // expression — it should only appear in qualified-name prefixes.
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!(
                    "ident `{}` resolves to a module alias, not a value expression",
                    ident.text
                ),
            });
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }

        Some(Binding::Constructor {
            owner_type,
            variant,
            is_record,
        }) => {
            // OQ-PHASE45-007: emit Symbol { Constructor } using the resolved
            // TyConId (via lookup_constructor_tycon → owner-type name →
            // lookup_tycon_by_name). Falls back to TyConId(0) when the symbol
            // table or workspace is absent (defensive; no L999 emitted).
            //
            // Use the resolver-supplied `is_record` flag to determine constructor kind.
            let id = ctx.fresh_id(None);
            let tycon_id = ctx
                .lookup_constructor_tycon(*owner_type)
                .unwrap_or(TyConId(0));
            let ctor_kind = if *is_record {
                ridge_ir::CtorKind::Record
            } else {
                ridge_ir::CtorKind::UnionVariant
            };
            IrExpr::Symbol {
                id,
                sym: SymbolRef::Constructor {
                    ctor_kind,
                    owner_type: tycon_id,
                    name: ident.text.clone(),
                    variant: *variant,
                },
                span,
            }
        }

        Some(Binding::FieldAccessor { field }) => {
            // Field-accessor shorthands `(.name)` lower to lambdas in the
            // field_accessor module; an ident binding to FieldAccessor is unexpected here.
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!(
                    "field accessor `{field}` encountered as ident; \
                     use Expr::FieldAccessorFn for field-accessor expressions"
                ),
            });
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }

        Some(Binding::ClassMethod { class_name, method }) => {
            // Lower a bare class method reference to a dictionary projection.
            //
            // Two sub-cases, mirroring the `toText` dispatch in interp.rs:
            //
            // (a) Forward — the enclosing fn is constrained for this class:
            //     emit `IrExpr::Field { base: Local("$dict_{Class}_{tyvid}"), field: method }`.
            //
            // (b) Static — monomorphic call site: the constraint solver placed a
            //     `DictPlan::Static` in `dict_resolution`. Resolve it via
            //     `resolve_dict_arg` (same helper used by `build_dict_args`) and
            //     project the field out of the resulting dict expression.
            //
            // The outer `Expr::Call` node then applies this field-projection value
            // to the user arguments; no extra dict args are prepended because the
            // dictionary has already been embedded in the callee position.
            // Prefer ctx.class_table (already a &ClassTable) when available;
            // fall back to the workspace-level ClassTable.
            let class_id = ctx
                .class_table
                .or_else(|| ctx.workspace.map(|ws| &ws.class_table))
                .and_then(|ct| ct.id_by_name(class_name));

            let dict_expr = if let Some(cid) = class_id {
                // A bare class-method reference carries no call arguments here to
                // pin the constraint by type; the surrounding call applies the
                // result. With no pinning type, `resolve_dict_arg` forwards an
                // enclosing dict param or falls back to the sole Static plan.
                resolve_dict_arg(ctx, cid, class_name, None, span)
            } else {
                // No workspace or unknown class — fall back to a unit literal.
                let id = ctx.fresh_id(None);
                IrExpr::Lit {
                    id,
                    value: IrLit::Unit,
                    span,
                }
            };

            let field_id = ctx.fresh_id(None);
            IrExpr::Field {
                id: field_id,
                base: Box::new(dict_expr),
                field: method.clone(),
                span,
            }
        }

        Some(Binding::Error) => {
            // Name resolution already emitted an R### diagnostic; lower to
            // a Unit literal to suppress cascading L### errors.
            let id = ctx.fresh_id(None);
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }

        // `Binding` is `#[non_exhaustive]`; handle future variants defensively.
        Some(_) => {
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!("unrecognised binding variant for ident `{}`", ident.text),
            });
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }
    }
}

// ── Qualified-name lowering ───────────────────────────────────────────────────

/// Lower a [`QualifiedName`] atom to `IrExpr::Symbol`.
///
/// Qualified names always resolve to a symbol (stdlib, external, local module
/// symbol, or prelude).  If the binding map is absent or the name has no
/// binding entry, a defensive `L999` is emitted and a `Unit` literal is
/// returned.
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive dispatch over all Binding variants with defensive fallbacks"
)]
fn lower_qualified(ctx: &mut LowerCtx<'_>, qname: &QualifiedName) -> IrExpr {
    let span = qname.span;

    // The QualifiedName node itself is keyed by its full span under
    // `NodeKind::QualifiedName` in the `NodeIdMap`.
    let node_id = ctx
        .node_id_map
        .as_ref()
        .and_then(|m| m.get(span, NodeKind::QualifiedName));

    let binding = node_id.and_then(|nid| {
        ctx.binding_map
            .and_then(|bm| bm.get(nid.0 as usize).and_then(Option::as_ref))
    });

    // Human-readable name for diagnostics.
    let name_text: String = qname
        .segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(".");

    // Last segment name used in SymbolRef construction.
    let last_name = qname
        .segments
        .last()
        .map_or_else(|| name_text.clone(), |s| s.text.clone());

    match binding {
        None => {
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!(
                    "no binding found for qualified name `{name_text}` at {span:?}; binding map absent or NodeId missing"
                ),
            });
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }

        Some(Binding::StdlibSymbol {
            module: stdlib_id,
            name,
        }) => {
            let module_name = stdlib_module_name(*stdlib_id);
            let id = ctx.fresh_id(None);
            IrExpr::Symbol {
                id,
                sym: SymbolRef::Stdlib {
                    module: module_name,
                    name: name.clone(),
                },
                span,
            }
        }

        Some(Binding::ImportedSymbol {
            module, symbol: _, ..
        }) => {
            let id = ctx.fresh_id(None);
            IrExpr::Symbol {
                id,
                sym: SymbolRef::External {
                    module: *module,
                    name: last_name,
                },
                span,
            }
        }

        Some(Binding::ModuleSymbol { module, symbol: _ }) => {
            let id = ctx.fresh_id(None);
            IrExpr::Symbol {
                id,
                sym: SymbolRef::Local {
                    module: *module,
                    name: last_name,
                },
                span,
            }
        }

        Some(Binding::ModuleAlias { .. }) => {
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!(
                    "qualified name `{name_text}` resolves to a module alias, not a value"
                ),
            });
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }

        Some(Binding::Constructor {
            owner_type,
            variant,
            ..
        }) => {
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!(
                    "qualified constructor `{name_text}` (owner={owner_type:?}, variant={variant}) \
                     encountered in ident lowering; use IrExpr::Construct for constructor expressions"
                ),
            });
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }

        Some(
            Binding::Error
            | Binding::Local(_)
            | Binding::ActorName { .. }
            | Binding::FieldAccessor { .. },
        ) => {
            let id = ctx.fresh_id(None);
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }

        // Defensive catch-all for future `Binding` variants.
        Some(_) => {
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!("unrecognised binding variant for qualified name `{name_text}`"),
            });
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }
    }
}

// ── Interp lowering ───────────────────────────────────────────────────────────

/// Lower an `Interp` expression.
///
/// Single-part text-only interpolation lowers to a plain text literal.
/// Any other shape (multiple parts, expression holes) dispatches to
/// [`lower_interp_full`] from the `interp` module (§4.6).
fn lower_interp(ctx: &mut LowerCtx<'_>, parts: &[InterpPart], span: Span) -> IrExpr {
    if let [InterpPart::Text { raw, .. }] = parts {
        // Single-part, text-only: no fold needed — emit a literal directly.
        let id = ctx.fresh_id(None);
        IrExpr::Lit {
            id,
            value: IrLit::Text(raw.clone()),
            span,
        }
    } else {
        // Multi-part or hole-containing — dispatch to the full interp lowering rule.
        lower_interp_full(ctx, parts, span)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the dot-separated stdlib module path string for a `StdlibModuleId`.
///
/// Falls back to `"std.unknown"` for an out-of-range id (defensive).
fn stdlib_module_name(id: StdlibModuleId) -> String {
    BUILTINS
        .get(id.0 as usize)
        .map_or_else(|| "std.unknown".to_owned(), |m| m.name.to_owned())
}

/// Parse a decimal integer raw lexeme (possibly with `_` separators) to `i64`.
fn parse_int_dec(ctx: &mut LowerCtx<'_>, raw: &str, span: Span) -> IrLit {
    let cleaned = raw.replace('_', "");
    match cleaned.parse::<i64>() {
        Ok(n) => IrLit::Int(n),
        Err(e) => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!("decimal integer `{raw}` could not be parsed as i64: {e}"),
            });
            IrLit::Int(0)
        }
    }
}

/// Parse a binary integer raw lexeme (`0b…`) to `i64`.
fn parse_int_bin(ctx: &mut LowerCtx<'_>, raw: &str, span: Span) -> IrLit {
    let cleaned = raw.trim_start_matches("0b").replace('_', "");
    match i64::from_str_radix(&cleaned, 2) {
        Ok(n) => IrLit::Int(n),
        Err(e) => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!("binary integer `{raw}` could not be parsed as i64: {e}"),
            });
            IrLit::Int(0)
        }
    }
}

/// Parse an octal integer raw lexeme (`0o…`) to `i64`.
fn parse_int_oct(ctx: &mut LowerCtx<'_>, raw: &str, span: Span) -> IrLit {
    let cleaned = raw.trim_start_matches("0o").replace('_', "");
    match i64::from_str_radix(&cleaned, 8) {
        Ok(n) => IrLit::Int(n),
        Err(e) => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!("octal integer `{raw}` could not be parsed as i64: {e}"),
            });
            IrLit::Int(0)
        }
    }
}

/// Parse a hex integer raw lexeme (`0x…`) to `i64`.
fn parse_int_hex(ctx: &mut LowerCtx<'_>, raw: &str, span: Span) -> IrLit {
    let cleaned = raw.trim_start_matches("0x").replace('_', "");
    match i64::from_str_radix(&cleaned, 16) {
        Ok(n) => IrLit::Int(n),
        Err(e) => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!("hex integer `{raw}` could not be parsed as i64: {e}"),
            });
            IrLit::Int(0)
        }
    }
}

/// Parse a float raw lexeme (possibly with `_` separators) to `f64`.
fn parse_float(ctx: &mut LowerCtx<'_>, raw: &str, span: Span) -> IrLit {
    let cleaned = raw.replace('_', "");
    match cleaned.parse::<f64>() {
        Ok(f) => IrLit::Float(f),
        Err(e) => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!("float literal `{raw}` could not be parsed as f64: {e}"),
            });
            IrLit::Float(0.0)
        }
    }
}

/// Decode a `Text` raw lexeme into its runtime value.
///
/// The lexer (`crates/ridge-lexer/src/raw_scan.rs::scan`) already strips the
/// surrounding `"` delimiters before storing the raw bytes in
/// `Token::TextLit(content)` — historically this function ALSO did a
/// belt-and-braces `strip_prefix('"')` + `strip_suffix('"')`, but that was
/// silently broken: when a string ended with `\"` (escaped quote as the last
/// content byte), `strip_suffix('"')` would chop off the legitimate trailing
/// quote, leaving the orphan `\` and yielding `"hi\"` → `"hi\` on output.
///
/// Today this function only decodes the validated escape sequences.
/// The lexer validates escapes eagerly (see
/// `crates/ridge-lexer/src/strings.rs`), so by the time we reach here every
/// `\<char>` sequence is one of the spec-permitted forms (`\n`, `\t`, `\r`,
/// `\0`, `\"`, `\\`, `\u{HHHH}`) or has already produced a lex-time
/// diagnostic.  We additionally accept `\$` so interpolation `$"..."` can
/// suppress the `$` hole marker.
pub(crate) fn strip_text_quotes(raw: &str) -> String {
    decode_text_escapes(raw)
}

/// Decode the validated escape sequences inside a text literal body.
///
/// Shared between `Literal::Text` lowering and `InterpPart::Text` lowering
/// (see [`crate::interp`]).  Both call sites previously stored the raw source
/// bytes (including backslashes), which silently produced strings that
/// contained the literal escape sequences at runtime.
pub(crate) fn decode_text_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('$') => out.push('$'),
            Some('u') => {
                // \u{HHHH} — lexer validated the braces and hex.  Read until '}'.
                let mut hex = String::new();
                if chars.next() == Some('{') {
                    for c2 in chars.by_ref() {
                        if c2 == '}' {
                            break;
                        }
                        hex.push(c2);
                    }
                    if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(cp) {
                            out.push(ch);
                            continue;
                        }
                    }
                }
                // Defensive: lex-time validation should have caught this; if
                // we still get here, preserve the raw bytes so output is
                // recoverable rather than silently lossy.
                out.push('\\');
                out.push('u');
                out.push('{');
                out.push_str(&hex);
                out.push('}');
            }
            Some(other) => {
                // Lexer rejects unknown escapes; defensive passthrough.
                out.push('\\');
                out.push(other);
            }
            None => {
                // Trailing backslash — lexer would have raised
                // unterminated-string; defensive passthrough.
                out.push('\\');
            }
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::Span;
    use ridge_ir::{IrExpr, IrLit, IrNodeId};
    use ridge_resolve::{BindingMap, ModuleId, NodeIdMap, NodeKind};
    use ridge_types::CapabilitySet;

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    // ── Literal::IntDec ───────────────────────────────────────────────────────

    #[test]
    fn lower_expr_literal_int() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let expr = Expr::Literal(Literal::IntDec {
            raw: "42".into(),
            span,
        });
        let ir = lower_expr(&mut ctx, &expr);
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Lit {
                id,
                value: IrLit::Int(n),
                span: s,
            } => {
                assert_eq!(id, IrNodeId(0));
                assert_eq!(n, 42);
                assert_eq!(s, span);
            }
            other => panic!("expected IrExpr::Lit Int(42), got {other:?}"),
        }
    }

    // ── Literal::Bool ─────────────────────────────────────────────────────────

    #[test]
    fn lower_expr_literal_bool() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let expr = Expr::Literal(Literal::Bool { value: true, span });
        let ir = lower_expr(&mut ctx, &expr);
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Lit {
                value: IrLit::Bool(b),
                ..
            } => {
                assert!(b, "expected true");
            }
            other => panic!("expected IrExpr::Lit Bool(true), got {other:?}"),
        }
    }

    // ── Literal::Text ─────────────────────────────────────────────────────────

    #[test]
    fn lower_expr_literal_text() {
        let mut ctx = fresh_ctx();
        let span = sp();
        // The lexer's TextLit token carries the content between the quotes
        // (already stripped, see `raw_scan::scan`).  Lowering decodes any
        // validated escape sequences but does not strip quotes a second time.
        let expr = Expr::Literal(Literal::Text {
            raw: "hi".into(),
            span,
        });
        let ir = lower_expr(&mut ctx, &expr);
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Lit {
                value: IrLit::Text(s),
                ..
            } => {
                assert_eq!(s, "hi");
            }
            other => panic!("expected IrExpr::Lit Text(\"hi\"), got {other:?}"),
        }
    }

    /// Regression: a literal whose decoded content ends in `"` (from a final
    /// `\"` escape) must NOT have the trailing byte stripped by lowering.
    #[test]
    fn lower_expr_literal_text_trailing_escaped_quote() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let expr = Expr::Literal(Literal::Text {
            raw: r#"hi\""#.into(),
            span,
        });
        let ir = lower_expr(&mut ctx, &expr);
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Lit {
                value: IrLit::Text(s),
                ..
            } => {
                assert_eq!(s, "hi\"");
            }
            other => panic!("expected IrExpr::Lit Text(hi\"), got {other:?}"),
        }
    }

    /// Regression: every spec-permitted escape must decode to its actual
    /// character, not pass through verbatim.
    #[test]
    fn lower_expr_literal_text_decodes_all_escapes() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let expr = Expr::Literal(Literal::Text {
            raw: r#"a\nb\tc\rd\0e\"f\\g"#.into(),
            span,
        });
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Lit {
                value: IrLit::Text(s),
                ..
            } => {
                assert_eq!(s, "a\nb\tc\rd\0e\"f\\g");
            }
            other => panic!("expected IrExpr::Lit Text(...), got {other:?}"),
        }
    }

    // ── Expr::Unit ────────────────────────────────────────────────────────────

    #[test]
    fn lower_expr_unit() {
        let mut ctx = fresh_ctx();
        let span = sp_at(5, 7);
        let expr = Expr::Unit(span);
        let ir = lower_expr(&mut ctx, &expr);
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Lit {
                value: IrLit::Unit,
                span: s,
                ..
            } => {
                assert_eq!(s, span);
            }
            other => panic!("expected IrExpr::Lit Unit, got {other:?}"),
        }
    }

    // ── Ident resolving to Local ──────────────────────────────────────────────

    #[test]
    fn lower_expr_ident_local() {
        // Construct a NodeIdMap and BindingMap that maps the ident's span to
        // a Local binding.
        let span = Span::new(0, 5);

        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(span, NodeKind::Ident).unwrap();

        // BindingMap: slot `node_id.0` = Local binding.
        let local_id = ridge_resolve::LocalId(0);
        let mut binding_map: BindingMap = vec![None; (node_id.0 + 1) as usize];
        binding_map[node_id.0 as usize] = Some(Binding::Local(local_id));

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(binding_map)));

        let expr = Expr::Ident(Ident {
            text: "x".into(),
            span,
        });
        let ir = lower_expr(&mut ctx, &expr);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Local { name, span: s, .. } => {
                assert_eq!(name, "x");
                assert_eq!(s, span);
            }
            other => panic!("expected IrExpr::Local, got {other:?}"),
        }
    }

    // ── Ident resolving to Stdlib ─────────────────────────────────────────────

    #[test]
    fn lower_expr_ident_stdlib() {
        let span = Span::new(10, 18);

        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(span, NodeKind::Ident).unwrap();

        let stdlib_id = StdlibModuleId(0); // "std.int"
        let mut binding_map: BindingMap = vec![None; (node_id.0 + 1) as usize];
        binding_map[node_id.0 as usize] = Some(Binding::StdlibSymbol {
            module: stdlib_id,
            name: "toText".into(),
        });

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(binding_map)));

        let expr = Expr::Ident(Ident {
            text: "toText".into(),
            span,
        });
        let ir = lower_expr(&mut ctx, &expr);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        match ir {
            IrExpr::Symbol {
                sym: SymbolRef::Stdlib { module, name },
                ..
            } => {
                assert_eq!(module, "std.int");
                assert_eq!(name, "toText");
            }
            other => panic!("expected IrExpr::Symbol(Stdlib), got {other:?}"),
        }
    }

    // ── Pipe lowers to a Call ─────────────────────────────────────────────────
    //
    // `xs |> f` lowers to a `Call` node. `xs |> f` where `rhs` is `Unit`
    // (an invalid pipe shape) emits L002 and returns a Unit stub — but what we
    // really want to verify is that the *span* contract is still honoured.  Use
    // a bare-Ident RHS (which is a valid bare-callable shape) to get a proper
    // `Call` back and assert the outer pipe span is on the `Call` node.

    #[test]
    fn lower_expr_pipe_produces_call_with_pipe_span() {
        use ridge_ast::Ident;

        let mut ctx = fresh_ctx();
        let span = Span::new(20, 35);
        let lhs = Box::new(Expr::Unit(Span::point(20)));
        // Use an Ident as rhs so lower_pipe takes the bare-callable path.
        let rhs = Box::new(Expr::Ident(Ident {
            text: "f".into(),
            span: Span::point(24),
        }));
        let expr = Expr::Pipe { lhs, rhs, span };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Call { span: s, args, .. } => {
                assert_eq!(s, span, "Call must carry the pipe expression's span");
                assert_eq!(args.len(), 1, "bare-callable pipe: exactly 1 arg (the lhs)");
            }
            other => panic!("expected IrExpr::Call for Pipe, got {other:?}"),
        }
    }

    // ── source_map provenance for atoms ──────────────────────────────────────
    //
    // Atom lowering calls `ctx.fresh_id(None)` — no upstream NodeId is
    // available because `Expr` nodes don't carry `NodeId` directly (side-table
    // convention).  This test asserts the current behaviour so a regression
    // is flagged immediately.

    #[test]
    fn lower_expr_unit_no_provenance() {
        let mut ctx = fresh_ctx();
        let expr = Expr::Unit(sp());
        let _ir = lower_expr(&mut ctx, &expr);
        // Atom lowering passes None as origin — no source_map entry expected.
        assert!(
            ctx.source_map.is_empty(),
            "atom lowering must not record provenance; got {:?}",
            ctx.source_map
        );
    }

    // ── lower_pattern — wildcard ──────────────────────────────────────────────

    #[test]
    fn lower_pattern_wildcard() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let pat = Pattern::Wildcard { span };
        let ir_pat = lower_pattern(&mut ctx, &pat);
        assert!(matches!(ir_pat, IrPat::Wild { span: s } if s == span));
    }

    // ── lower_pattern — var binding ───────────────────────────────────────────

    #[test]
    fn lower_pattern_var() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let pat = Pattern::Var {
            name: Ident {
                text: "x".into(),
                span,
            },
            span,
        };
        let ir_pat = lower_pattern(&mut ctx, &pat);
        match ir_pat {
            IrPat::Bind {
                name, inner: None, ..
            } => assert_eq!(name, "x"),
            other => panic!("expected IrPat::Bind, got {other:?}"),
        }
    }

    // ── Tuple lowers to IrExpr::Tuple with correct element count ─────────────

    #[test]
    fn lower_expr_tuple_two_elems() {
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 10);
        let expr = Expr::Tuple {
            elems: vec![
                Expr::Literal(Literal::Bool {
                    value: true,
                    span: sp(),
                }),
                Expr::Literal(Literal::IntDec {
                    raw: "2".into(),
                    span: sp(),
                }),
            ],
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Tuple { elems, span: s, .. } => {
                assert_eq!(elems.len(), 2, "tuple must have 2 elements");
                assert_eq!(s, span);
                assert!(matches!(
                    elems[0],
                    IrExpr::Lit {
                        value: IrLit::Bool(true),
                        ..
                    }
                ));
                assert!(matches!(
                    elems[1],
                    IrExpr::Lit {
                        value: IrLit::Int(2),
                        ..
                    }
                ));
            }
            other => panic!("expected IrExpr::Tuple, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── List lowers to IrExpr::ListLit with correct element count ────────────

    #[test]
    fn lower_expr_list_three_elems() {
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 15);
        let expr = Expr::List {
            elems: vec![
                Expr::Literal(Literal::IntDec {
                    raw: "1".into(),
                    span: sp(),
                }),
                Expr::Literal(Literal::IntDec {
                    raw: "2".into(),
                    span: sp(),
                }),
                Expr::Literal(Literal::IntDec {
                    raw: "3".into(),
                    span: sp(),
                }),
            ],
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::ListLit { elems, span: s, .. } => {
                assert_eq!(elems.len(), 3, "list must have 3 elements");
                assert_eq!(s, span);
            }
            other => panic!("expected IrExpr::ListLit, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── Return lowers to IrExpr::Return with lowered value ───────────────────

    #[test]
    fn lower_expr_return_int() {
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 10);
        let expr = Expr::Return {
            value: Box::new(Expr::Literal(Literal::IntDec {
                raw: "42".into(),
                span: sp(),
            })),
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Return { value, span: s, .. } => {
                assert_eq!(s, span);
                assert!(matches!(
                    *value,
                    IrExpr::Lit {
                        value: IrLit::Int(42),
                        ..
                    }
                ));
            }
            other => panic!("expected IrExpr::Return, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── Lambda lowers to IrExpr::Lambda with correct param count ─────────────

    #[test]
    fn lower_expr_lambda_one_param() {
        use ridge_ast::{expr::LambdaParam, Literal};

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 20);
        let expr = Expr::Lambda {
            params: vec![LambdaParam::Pattern(Pattern::Var {
                name: Ident {
                    text: "x".into(),
                    span: sp(),
                },
                span: sp(),
            })],
            body: Box::new(Expr::Literal(Literal::IntDec {
                raw: "1".into(),
                span: sp(),
            })),
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Lambda {
                params,
                body,
                caps,
                span: s,
                ..
            } => {
                assert_eq!(params.len(), 1, "lambda must have 1 param");
                assert_eq!(params[0].name, "x");
                assert_eq!(s, span);
                assert!(matches!(
                    *body,
                    IrExpr::Lit {
                        value: IrLit::Int(1),
                        ..
                    }
                ));
                assert_eq!(caps, CapabilitySet::PURE, "caps placeholder must be PURE");
            }
            other => panic!("expected IrExpr::Lambda, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── FieldAccess lowers to IrExpr::Field ──────────────────────────────────

    #[test]
    fn lower_expr_field_access() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 10);
        let expr = Expr::FieldAccess {
            base: Box::new(Expr::Unit(sp())),
            field: Ident {
                text: "name".into(),
                span: sp(),
            },
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Field { field, span: s, .. } => {
                assert_eq!(field, "name");
                assert_eq!(s, span);
            }
            other => panic!("expected IrExpr::Field, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── Call lowers to IrExpr::Call with correct arg count ───────────────────

    #[test]
    fn lower_expr_call_two_args() {
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 15);
        let expr = Expr::Call {
            callee: Box::new(Expr::Unit(sp())),
            args: vec![
                Expr::Literal(Literal::IntDec {
                    raw: "1".into(),
                    span: sp(),
                }),
                Expr::Literal(Literal::IntDec {
                    raw: "2".into(),
                    span: sp(),
                }),
            ],
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Call { args, span: s, .. } => {
                assert_eq!(args.len(), 2, "call must have 2 args");
                assert_eq!(s, span);
            }
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── Spawn lowers to IrExpr::Spawn with ActorType symbol ──────────────────

    #[test]
    fn lower_expr_spawn_actor_name() {
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 15);
        let expr = Expr::Spawn {
            actor: Ident {
                text: "Store".into(),
                span: sp(),
            },
            args: vec![Expr::Literal(Literal::IntDec {
                raw: "0".into(),
                span: sp(),
            })],
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Spawn {
                actor,
                args,
                span: s,
                ..
            } => {
                assert_eq!(s, span);
                assert_eq!(args.len(), 1);
                match actor {
                    SymbolRef::ActorType { name, .. } => assert_eq!(name, "Store"),
                    other => panic!("expected ActorType, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Spawn, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── Send lowers to IrExpr::Send with Handler symbol ──────────────────────

    /// Bare-tag send: `handle ! report` (no args). Parser stores `message` as
    /// a plain `Expr::Ident`; lowering must produce `args: []`.
    #[test]
    fn lower_expr_send_handler_name() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 20);
        let expr = Expr::Send {
            handle: Box::new(Expr::Unit(sp())),
            message: Box::new(Expr::Ident(Ident {
                text: "report".into(),
                span: sp(),
            })),
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Send {
                message,
                args,
                span: s,
                ..
            } => {
                assert_eq!(s, span);
                assert!(args.is_empty(), "0-arg send must lower to empty args");
                match message {
                    SymbolRef::Handler { handler, .. } => assert_eq!(handler, "report"),
                    other => panic!("expected Handler, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Send, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    /// Send with args: `handle ! report "url" 42`. Parser stores `message` as
    /// `Expr::Call { callee: Ident("report"), args: [...] }`. Lowering must
    /// unfold the call and propagate the args into `IrExpr::Send.args`.
    ///
    /// Regression for the codegen-erl path where missing args meant
    /// `ridge_rt:send` was called with a payload of `{''}` (1-tuple with empty
    /// atom): the receiver could not pattern-match against
    /// `<{'report', V_A, V_B}> when ...` and dropped the message.
    #[test]
    fn lower_expr_send_handler_with_args() {
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 25);
        let call_span = sp_at(5, 25);
        let expr = Expr::Send {
            handle: Box::new(Expr::Unit(sp())),
            message: Box::new(Expr::Call {
                callee: Box::new(Expr::Ident(Ident {
                    text: "report".into(),
                    span: sp(),
                })),
                args: vec![
                    Expr::Literal(Literal::Text {
                        raw: r#""url""#.into(),
                        span: sp(),
                    }),
                    Expr::Literal(Literal::IntDec {
                        raw: "42".into(),
                        span: sp(),
                    }),
                ],
                span: call_span,
            }),
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Send {
                message,
                args,
                span: s,
                ..
            } => {
                assert_eq!(s, span);
                assert_eq!(args.len(), 2, "send must propagate the two args");
                match message {
                    SymbolRef::Handler { handler, .. } => assert_eq!(handler, "report"),
                    other => panic!("expected Handler, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Send, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── Ask lowers to IrExpr::Ask with Handler symbol and args ───────────────

    #[test]
    fn lower_expr_ask_handler_with_args() {
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 25);
        let expr = Expr::Ask {
            handle: Box::new(Expr::Unit(sp())),
            message: Ident {
                text: "shorten".into(),
                span: sp(),
            },
            args: vec![Expr::Literal(Literal::Text {
                raw: r#""url""#.into(),
                span: sp(),
            })],
            timeout: None,
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Ask {
                message,
                args,
                timeout,
                span: s,
                ..
            } => {
                assert_eq!(s, span);
                assert_eq!(args.len(), 1, "ask must carry 1 arg");
                assert!(
                    timeout.is_none(),
                    "ask with no timeout postfix must lower to timeout: None"
                );
                match message {
                    SymbolRef::Handler { handler, .. } => assert_eq!(handler, "shorten"),
                    other => panic!("expected Handler, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Ask, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── Record lowers to IrExpr::Construct with field values ─────────────────

    #[test]
    fn lower_expr_record_two_fields() {
        use ridge_ast::{
            expr::{FieldInit, RecordCtor},
            Literal,
        };

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 30);
        let expr = Expr::Record {
            constructor: RecordCtor::Bare(Ident {
                text: "Point".into(),
                span: sp(),
            }),
            fields: vec![
                FieldInit {
                    name: Ident {
                        text: "x".into(),
                        span: sp(),
                    },
                    value: Some(Expr::Literal(Literal::IntDec {
                        raw: "1".into(),
                        span: sp(),
                    })),
                    span: sp(),
                },
                FieldInit {
                    name: Ident {
                        text: "y".into(),
                        span: sp(),
                    },
                    value: Some(Expr::Literal(Literal::IntDec {
                        raw: "2".into(),
                        span: sp(),
                    })),
                    span: sp(),
                },
            ],
            span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Construct {
                ctor,
                fields,
                span: s,
                ..
            } => {
                assert_eq!(s, span);
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].0, "x");
                assert_eq!(fields[1].0, "y");
                match ctor {
                    SymbolRef::Constructor { name, .. } => assert_eq!(name, "Point"),
                    other => panic!("expected Constructor, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Construct, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── Union-variant call folding ────────────────────────────────────────────
    //
    // `Circle 5` parses as `Expr::Call { callee: Expr::Record(Bare("Circle"), []),
    // args: [5] }`. The callee lowers to `IrExpr::Construct { UnionVariant, fields: [] }`.
    // The fix folds the call args into the construct so codegen emits `{Circle, 5}`.

    fn make_union_ctor_ctx(ctor_name: &str) -> (LowerCtx<'static>, Span) {
        use ridge_resolve::SymbolId;
        let span = sp_at(10, 16);
        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(span, NodeKind::Ident).unwrap();
        let mut bm: BindingMap = vec![None; (node_id.0 + 1) as usize];
        bm[node_id.0 as usize] = Some(Binding::Constructor {
            owner_type: SymbolId(1),
            variant: 1,
            is_record: false,
        });
        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(bm)));
        let _ = ctor_name;
        (ctx, span)
    }

    /// `Circle 5` — single positional arg — must produce `IrExpr::Construct`
    /// with one field, not `IrExpr::Call`.
    #[test]
    fn lower_union_ctor_call_one_arg_produces_construct() {
        use ridge_ast::expr::RecordCtor;

        let (mut ctx, ctor_span) = make_union_ctor_ctx("Circle");
        let call_span = sp_at(0, 20);

        // Callee: bare `Circle` with no fields.
        let callee = Expr::Record {
            constructor: RecordCtor::Bare(Ident {
                text: "Circle".into(),
                span: ctor_span,
            }),
            fields: vec![],
            span: ctor_span,
        };
        let expr = Expr::Call {
            callee: Box::new(callee),
            args: vec![Expr::Literal(Literal::IntDec {
                raw: "5".into(),
                span: sp(),
            })],
            span: call_span,
        };

        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        match ir {
            IrExpr::Construct {
                ctor,
                fields,
                span: s,
                ..
            } => {
                assert_eq!(s, call_span);
                assert_eq!(fields.len(), 1, "expected 1 positional field");
                assert!(
                    matches!(
                        &fields[0].1,
                        IrExpr::Lit {
                            value: IrLit::Int(5),
                            ..
                        }
                    ),
                    "expected Int(5) field"
                );
                match ctor {
                    SymbolRef::Constructor {
                        ctor_kind: ridge_ir::CtorKind::UnionVariant,
                        ..
                    } => {}
                    other => panic!("expected UnionVariant ctor, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Construct, got {other:?}"),
        }
    }

    /// `Rectangle 4 6` — two positional args — must produce `IrExpr::Construct`
    /// with two fields.
    #[test]
    fn lower_union_ctor_call_two_args_produces_construct() {
        use ridge_ast::expr::RecordCtor;

        let (mut ctx, ctor_span) = make_union_ctor_ctx("Rectangle");
        let call_span = sp_at(0, 25);

        let callee = Expr::Record {
            constructor: RecordCtor::Bare(Ident {
                text: "Rectangle".into(),
                span: ctor_span,
            }),
            fields: vec![],
            span: ctor_span,
        };
        let expr = Expr::Call {
            callee: Box::new(callee),
            args: vec![
                Expr::Literal(Literal::IntDec {
                    raw: "4".into(),
                    span: sp(),
                }),
                Expr::Literal(Literal::IntDec {
                    raw: "6".into(),
                    span: sp(),
                }),
            ],
            span: call_span,
        };

        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        match ir {
            IrExpr::Construct { ctor, fields, .. } => {
                assert_eq!(fields.len(), 2, "expected 2 positional fields");
                assert!(
                    matches!(
                        &fields[0].1,
                        IrExpr::Lit {
                            value: IrLit::Int(4),
                            ..
                        }
                    ),
                    "expected Int(4) as first field"
                );
                assert!(
                    matches!(
                        &fields[1].1,
                        IrExpr::Lit {
                            value: IrLit::Int(6),
                            ..
                        }
                    ),
                    "expected Int(6) as second field"
                );
                match ctor {
                    SymbolRef::Constructor {
                        ctor_kind: ridge_ir::CtorKind::UnionVariant,
                        ..
                    } => {}
                    other => panic!("expected UnionVariant ctor, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Construct, got {other:?}"),
        }
    }

    // ── Prelude constructor routing ───────────────────────────────────────────

    /// Build a NodeIdMap + BindingMap entry that resolves a single ident span to
    /// a given Binding. Returns both maps and the span used.
    fn make_binding_ctx(binding: Binding) -> (LowerCtx<'static>, Span) {
        use ridge_resolve::LocalId;
        let span = sp_at(10, 15);
        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(span, NodeKind::Ident).unwrap();
        let mut bm: BindingMap = vec![None; (node_id.0 + 1) as usize];
        bm[node_id.0 as usize] = Some(binding);
        // Avoid unused-import warning for LocalId — pattern match guarantees call.
        let _ = LocalId(0);
        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(bm)));
        (ctx, span)
    }

    /// `Ok` ident (no fields) with `StdlibSymbol { "Ok" }` binding →
    /// `Symbol { Prelude("Ok") }` (function-style: no fields → Symbol, not Construct).
    ///
    /// B-1 regression guard: bare-ident prelude constructors must no longer
    /// lower to `Constructor { Record }`. When used as a callee with no record
    /// fields (e.g. `Ok x` parsed as `Expr::Record { "Ok", [] }` applied to `x`),
    /// the emitted node must be `IrExpr::Symbol { Prelude("Ok") }` so that Phase 6's
    /// `lower_call` routes through `lower_prelude_call(args=[x])` correctly.
    #[test]
    fn lower_record_prelude_ok() {
        use ridge_ast::expr::RecordCtor;
        use ridge_resolve::StdlibModuleId;

        let (mut ctx, ctor_span) = make_binding_ctx(Binding::StdlibSymbol {
            module: StdlibModuleId(8), // std.result
            name: "Ok".to_string(),
        });

        let expr = Expr::Record {
            constructor: RecordCtor::Bare(Ident {
                text: "Ok".into(),
                span: ctor_span,
            }),
            fields: vec![],
            span: sp_at(10, 20),
        };
        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        // No fields → function-style → Symbol { Prelude("Ok") }, not Construct.
        match ir {
            IrExpr::Symbol { sym, .. } => match sym {
                SymbolRef::Prelude { name } => assert_eq!(name, "Ok"),
                other => panic!("B-1: expected Symbol Prelude(Ok), got {other:?}"),
            },
            other => panic!("B-1: expected Symbol, got {other:?}"),
        }
    }

    /// `Some` with a field argument (simulating `Some val`): binding is StdlibSymbol
    /// → emitted ctor must be `Prelude("Some")`, not `Constructor { Record }`.
    #[test]
    fn lower_record_prelude_some_with_arg() {
        use ridge_ast::{
            expr::{FieldInit, RecordCtor},
            Literal,
        };
        use ridge_resolve::StdlibModuleId;

        let (mut ctx, ctor_span) = make_binding_ctx(Binding::StdlibSymbol {
            module: StdlibModuleId(7), // std.option
            name: "Some".to_string(),
        });

        let expr = Expr::Record {
            constructor: RecordCtor::Bare(Ident {
                text: "Some".into(),
                span: ctor_span,
            }),
            fields: vec![FieldInit {
                name: Ident {
                    text: "value".into(),
                    span: sp(),
                },
                value: Some(Expr::Literal(Literal::IntDec {
                    raw: "1".into(),
                    span: sp(),
                })),
                span: sp(),
            }],
            span: sp_at(10, 25),
        };
        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        match ir {
            IrExpr::Construct { ctor, .. } => match ctor {
                SymbolRef::Prelude { name } => assert_eq!(name, "Some"),
                other => panic!("B-1: expected Prelude(Some), got {other:?}"),
            },
            other => panic!("expected Construct, got {other:?}"),
        }
    }

    /// User record constructor `Point { x = 1, y = 2 }` with `Constructor { variant: 0 }`
    /// binding → must still emit `Constructor { Record, variant: 0 }`. Regression guard.
    #[test]
    fn lower_record_user_record_unchanged() {
        use ridge_ast::{
            expr::{FieldInit, RecordCtor},
            Literal,
        };
        use ridge_resolve::SymbolId;

        let (mut ctx, ctor_span) = make_binding_ctx(Binding::Constructor {
            owner_type: SymbolId(0),
            variant: 0,
            is_record: true,
        });

        let expr = Expr::Record {
            constructor: RecordCtor::Bare(Ident {
                text: "Point".into(),
                span: ctor_span,
            }),
            fields: vec![FieldInit {
                name: Ident {
                    text: "x".into(),
                    span: sp(),
                },
                value: Some(Expr::Literal(Literal::IntDec {
                    raw: "1".into(),
                    span: sp(),
                })),
                span: sp(),
            }],
            span: sp_at(10, 30),
        };
        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        match ir {
            IrExpr::Construct { ctor, .. } => match ctor {
                SymbolRef::Constructor {
                    ctor_kind: ridge_ir::CtorKind::Record,
                    name,
                    variant,
                    ..
                } => {
                    assert_eq!(name, "Point", "name must be Point");
                    assert_eq!(variant, 0, "record auto-ctor must have variant 0");
                }
                other => {
                    panic!("B-1 regression: expected Constructor(Record,Point), got {other:?}")
                }
            },
            other => panic!("expected Construct, got {other:?}"),
        }
    }

    /// Union variant `Info` with `Constructor { variant: 1 }` binding
    /// → must emit `Constructor { UnionVariant, variant: 1 }`.
    #[test]
    fn lower_record_user_union_variant() {
        use ridge_ast::expr::RecordCtor;
        use ridge_resolve::SymbolId;

        let (mut ctx, ctor_span) = make_binding_ctx(Binding::Constructor {
            owner_type: SymbolId(0),
            variant: 1,
            is_record: false,
        });

        let expr = Expr::Record {
            constructor: RecordCtor::Bare(Ident {
                text: "Info".into(),
                span: ctor_span,
            }),
            fields: vec![],
            span: sp_at(10, 20),
        };
        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        match ir {
            IrExpr::Construct { ctor, .. } => match ctor {
                SymbolRef::Constructor {
                    ctor_kind: ridge_ir::CtorKind::UnionVariant,
                    name,
                    variant,
                    ..
                } => {
                    assert_eq!(name, "Info", "name must be Info");
                    assert_eq!(variant, 1, "union variant must have variant 1");
                }
                other => panic!("B-1: expected Constructor(UnionVariant,Info,1), got {other:?}"),
            },
            other => panic!("expected Construct, got {other:?}"),
        }
    }

    // ── Tuple-pattern lambda param destructuring ──────────────────────────────

    /// `fn (a, b) -> a` — tuple-pattern param gets a synthetic name and the
    /// body is wrapped in a Match that destructures (a, b).
    #[test]
    fn lower_lambda_tuple_param() {
        use ridge_ast::expr::LambdaParam;
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let lambda_span = sp_at(0, 20);
        let tup_span = sp_at(3, 8);

        let expr = Expr::Lambda {
            params: vec![LambdaParam::Pattern(Pattern::Tuple {
                elems: vec![
                    Pattern::Var {
                        name: Ident {
                            text: "a".into(),
                            span: sp_at(4, 5),
                        },
                        span: sp_at(4, 5),
                    },
                    Pattern::Var {
                        name: Ident {
                            text: "b".into(),
                            span: sp_at(7, 8),
                        },
                        span: sp_at(7, 8),
                    },
                ],
                span: tup_span,
            })],
            body: Box::new(Expr::Literal(Literal::IntDec {
                raw: "1".into(),
                span: sp(),
            })),
            span: lambda_span,
        };

        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);

        match ir {
            IrExpr::Lambda { params, body, .. } => {
                // The single param must have a synthetic name (starts with __tuple_param).
                assert_eq!(params.len(), 1, "must have exactly 1 IR param");
                assert!(
                    params[0].name.starts_with("__tuple_param"),
                    "synthetic param must start with __tuple_param, got {:?}",
                    params[0].name
                );
                // The body must be a Match that destructures into (a, b).
                match *body {
                    IrExpr::Match {
                        scrutinee, arms, ..
                    } => {
                        // Scrutinee must be Local(<synthetic_name>).
                        match *scrutinee {
                            IrExpr::Local { name, .. } => {
                                assert!(
                                    name.starts_with("__tuple_param"),
                                    "scrutinee must be the synthetic local"
                                );
                            }
                            other => panic!("expected Local scrutinee, got {other:?}"),
                        }
                        assert_eq!(arms.len(), 1, "match must have 1 arm");
                        match &arms[0].pat {
                            IrPat::Tuple { elems, .. } => {
                                assert_eq!(elems.len(), 2, "tuple pat must have 2 elems");
                                assert!(
                                    matches!(&elems[0], IrPat::Bind { name, .. } if name == "a"),
                                    "first elem must be Bind(a)"
                                );
                                assert!(
                                    matches!(&elems[1], IrPat::Bind { name, .. } if name == "b"),
                                    "second elem must be Bind(b)"
                                );
                            }
                            other => panic!("expected Tuple pat, got {other:?}"),
                        }
                    }
                    other => panic!("B-2: expected Match body, got {other:?}"),
                }
            }
            other => panic!("expected Lambda, got {other:?}"),
        }
    }

    /// `fn x -> x` — Var-only param is unchanged (regression guard).
    #[test]
    fn lower_lambda_var_param_unchanged() {
        use ridge_ast::expr::LambdaParam;
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let lambda_span = sp_at(0, 10);
        let expr = Expr::Lambda {
            params: vec![LambdaParam::Pattern(Pattern::Var {
                name: Ident {
                    text: "x".into(),
                    span: sp(),
                },
                span: sp(),
            })],
            body: Box::new(Expr::Literal(Literal::IntDec {
                raw: "1".into(),
                span: sp(),
            })),
            span: lambda_span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        match ir {
            IrExpr::Lambda { params, body, .. } => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, "x", "Var param must keep its name");
                // Body must NOT be wrapped in a Match.
                assert!(
                    !matches!(*body, IrExpr::Match { .. }),
                    "B-2 regression: Var-only param body must NOT be wrapped in Match"
                );
            }
            other => panic!("expected Lambda, got {other:?}"),
        }
    }

    /// `fn x (a, b) -> a` — mixed Var + Tuple params: only the tuple param
    /// gets a synthetic name and a Match wrapper.
    #[test]
    fn lower_lambda_mixed_var_and_tuple() {
        use ridge_ast::expr::LambdaParam;
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let lambda_span = sp_at(0, 25);
        let tup_span = sp_at(5, 12);
        let expr = Expr::Lambda {
            params: vec![
                LambdaParam::Pattern(Pattern::Var {
                    name: Ident {
                        text: "x".into(),
                        span: sp_at(3, 4),
                    },
                    span: sp_at(3, 4),
                }),
                LambdaParam::Pattern(Pattern::Tuple {
                    elems: vec![
                        Pattern::Var {
                            name: Ident {
                                text: "a".into(),
                                span: sp_at(6, 7),
                            },
                            span: sp_at(6, 7),
                        },
                        Pattern::Var {
                            name: Ident {
                                text: "b".into(),
                                span: sp_at(9, 10),
                            },
                            span: sp_at(9, 10),
                        },
                    ],
                    span: tup_span,
                }),
            ],
            body: Box::new(Expr::Literal(Literal::IntDec {
                raw: "1".into(),
                span: sp(),
            })),
            span: lambda_span,
        };
        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        match ir {
            IrExpr::Lambda { params, body, .. } => {
                assert_eq!(params.len(), 2, "must have 2 IR params");
                assert_eq!(params[0].name, "x", "first param must keep its name");
                assert!(
                    params[1].name.starts_with("__tuple_param"),
                    "second param must be synthetic"
                );
                // Body must be a Match wrapper.
                assert!(
                    matches!(*body, IrExpr::Match { .. }),
                    "B-2: mixed lambda body must be wrapped in Match for the tuple param"
                );
            }
            other => panic!("expected Lambda, got {other:?}"),
        }
    }

    // ── Partial application detection ────────────────────────────────────────

    /// Build a ctx with a node_type entry for a given callee span.
    ///
    /// Phase 4 stamps the callee type under `NodeKind::Expr` (not `NodeKind::Ident`),
    /// so we register two NodeIdMap entries for the callee span:
    ///  - `NodeKind::Ident`  → NodeId for the binding (Local binding)
    ///  - `NodeKind::Expr`   → NodeId for the type stamp
    fn make_ctx_with_callee_type(callee_span: Span, callee_ty: Type) -> LowerCtx<'static> {
        use ridge_resolve::LocalId;
        let mut nid_map = NodeIdMap::default();
        // NodeId for the Ident binding lookup (used by lower_ident).
        let ident_nid = nid_map.assign(callee_span, NodeKind::Ident).unwrap();
        // NodeId for the Expr type stamp (used by lookup_callee_type / B-3).
        let expr_nid = nid_map.assign(callee_span, NodeKind::Expr).unwrap();

        // node_types must be large enough for both NodeIds.
        let max_nid = ident_nid.0.max(expr_nid.0) as usize;
        let mut node_types: Vec<Option<Type>> = vec![None; max_nid + 1];
        node_types[expr_nid.0 as usize] = Some(callee_ty);

        // BindingMap only needs the Ident entry.
        let local_id = LocalId(0);
        let bm_size = ident_nid.0.max(expr_nid.0) as usize + 1;
        let mut bm: BindingMap = vec![None; bm_size];
        bm[ident_nid.0 as usize] = Some(Binding::Local(local_id));

        let node_types_leaked: &'static [Option<Type>] = Box::leak(node_types.into_boxed_slice());
        let mut ctx = LowerCtx::new(ModuleId(0), node_types_leaked);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(bm)));
        ctx
    }

    /// `f x` where `f : A -> B -> C` (arity 2, 1 arg) → Lambda wrapper.
    #[test]
    fn lower_partial_app_arity2_one_arg() {
        use ridge_ast::Literal;
        use ridge_types::CapRow;

        let callee_span = sp_at(0, 1);
        let callee_ty = Type::Fn {
            params: vec![Type::Error, Type::Error],
            ret: Box::new(Type::Error),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let mut ctx = make_ctx_with_callee_type(callee_span, callee_ty);

        let expr = Expr::Call {
            callee: Box::new(Expr::Ident(Ident {
                text: "f".into(),
                span: callee_span,
            })),
            args: vec![Expr::Literal(Literal::IntDec {
                raw: "1".into(),
                span: sp(),
            })],
            span: sp_at(0, 10),
        };
        let ir = lower_expr(&mut ctx, &expr);
        // B-3: must be a Lambda wrapping an inner Call.
        match ir {
            IrExpr::Lambda { params, body, .. } => {
                assert_eq!(params.len(), 1, "one extra param for the missing arg");
                assert!(
                    params[0].name.starts_with("__pa"),
                    "synthetic param must start with __pa, got {:?}",
                    params[0].name
                );
                assert!(
                    matches!(*body, IrExpr::Call { .. }),
                    "lambda body must be the full-arity Call"
                );
            }
            other => panic!("B-3: expected Lambda, got {other:?}"),
        }
    }

    /// `f x y` where `f : A -> B -> C` (arity 2, 2 args) → unchanged Call.
    #[test]
    fn lower_full_app_unchanged() {
        use ridge_ast::Literal;
        use ridge_types::CapRow;

        let callee_span = sp_at(0, 1);
        let callee_ty = Type::Fn {
            params: vec![Type::Error, Type::Error],
            ret: Box::new(Type::Error),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let mut ctx = make_ctx_with_callee_type(callee_span, callee_ty);

        let expr = Expr::Call {
            callee: Box::new(Expr::Ident(Ident {
                text: "f".into(),
                span: callee_span,
            })),
            args: vec![
                Expr::Literal(Literal::IntDec {
                    raw: "1".into(),
                    span: sp(),
                }),
                Expr::Literal(Literal::IntDec {
                    raw: "2".into(),
                    span: sp(),
                }),
            ],
            span: sp_at(0, 15),
        };
        let ir = lower_expr(&mut ctx, &expr);
        // Full application: must be a plain Call, not a Lambda.
        assert!(
            matches!(ir, IrExpr::Call { .. }),
            "B-3 regression: full application must NOT be wrapped in Lambda"
        );
    }

    /// When `node_type` lookup fails, no rewriting occurs.
    #[test]
    fn lower_no_node_type_unchanged() {
        use ridge_ast::Literal;

        // ctx with no node_type info (empty table).
        let mut ctx = fresh_ctx();

        let expr = Expr::Call {
            callee: Box::new(Expr::Ident(Ident {
                text: "f".into(),
                span: sp(),
            })),
            args: vec![Expr::Literal(Literal::IntDec {
                raw: "1".into(),
                span: sp(),
            })],
            span: sp_at(0, 10),
        };
        let ir = lower_expr(&mut ctx, &expr);
        // No type info → no rewriting → plain Call.
        assert!(
            matches!(ir, IrExpr::Call { .. }),
            "B-3: without node_type info, call must not be wrapped"
        );
    }

    /// `f x` where `f : A -> B -> C -> D` (arity 3, 1 arg) → Lambda with 2 extra params.
    #[test]
    fn lower_partial_app_arity3_one_arg() {
        use ridge_ast::Literal;
        use ridge_types::CapRow;

        let callee_span = sp_at(0, 1);
        let callee_ty = Type::Fn {
            params: vec![Type::Error, Type::Error, Type::Error],
            ret: Box::new(Type::Error),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let mut ctx = make_ctx_with_callee_type(callee_span, callee_ty);

        let expr = Expr::Call {
            callee: Box::new(Expr::Ident(Ident {
                text: "f".into(),
                span: callee_span,
            })),
            args: vec![Expr::Literal(Literal::IntDec {
                raw: "1".into(),
                span: sp(),
            })],
            span: sp_at(0, 10),
        };
        let ir = lower_expr(&mut ctx, &expr);
        match ir {
            IrExpr::Lambda { params, body, .. } => {
                assert_eq!(
                    params.len(),
                    2,
                    "arity-3 with 1 applied arg → 2 extra params"
                );
                assert!(
                    matches!(*body, IrExpr::Call { ref args, .. } if args.len() == 3),
                    "inner Call must have 3 total args (1 original + 2 extra)"
                );
            }
            other => panic!("B-3: expected Lambda, got {other:?}"),
        }
    }

    // ── State-field-read precedence in lower_ident ───────────────────────────

    /// Build a binding ctx (with NodeIdMap + BindingMap) for a single ident
    /// span, and also set `in_actor_body + current_state_fields`.
    fn make_actor_body_ctx(
        ident_span: Span,
        binding: Binding,
        state_fields: Vec<&str>,
    ) -> LowerCtx<'static> {
        use rustc_hash::FxHashSet;
        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(ident_span, NodeKind::Ident).unwrap();
        let mut bm: BindingMap = vec![None; (node_id.0 + 1) as usize];
        bm[node_id.0 as usize] = Some(binding);
        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(bm)));
        ctx.in_actor_body = true;
        ctx.current_state_fields = Some(
            state_fields
                .into_iter()
                .map(str::to_string)
                .collect::<FxHashSet<_>>(),
        );
        ctx
    }

    /// Bare `tokens` ident with `in_actor_body=true` and `current_state_fields={"tokens"}`
    /// → `Field { Local("__state"), "tokens" }`.
    #[test]
    fn lower_ident_state_field_in_handler() {
        use ridge_resolve::LocalId;

        let ident_span = sp_at(10, 16);
        let mut ctx = make_actor_body_ctx(ident_span, Binding::Local(LocalId(0)), vec!["tokens"]);

        let ident = Ident {
            text: "tokens".into(),
            span: ident_span,
        };
        let ir = lower_ident(&mut ctx, &ident);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);

        match ir {
            IrExpr::Field { base, field, .. } => {
                assert_eq!(field, "tokens");
                match *base {
                    IrExpr::Local { name, .. } => {
                        assert_eq!(name, "__state", "base must be __state");
                    }
                    other => panic!("expected Local(__state) base, got {other:?}"),
                }
            }
            other => panic!("B-5: expected Field {{ __state, tokens }}, got {other:?}"),
        }
    }

    /// Same ident with `in_actor_body=false` → original `Local` behaviour preserved.
    #[test]
    fn lower_ident_state_field_outside_actor_unchanged() {
        use ridge_resolve::LocalId;

        let ident_span = sp_at(10, 16);
        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(ident_span, NodeKind::Ident).unwrap();
        let mut bm: BindingMap = vec![None; (node_id.0 + 1) as usize];
        bm[node_id.0 as usize] = Some(Binding::Local(LocalId(0)));

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(bm)));
        // in_actor_body is false (default).
        ctx.current_state_fields = None;

        let ident = Ident {
            text: "tokens".into(),
            span: ident_span,
        };
        let ir = lower_ident(&mut ctx, &ident);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);

        match ir {
            IrExpr::Local { name, .. } => {
                assert_eq!(name, "tokens", "outside actor body: must be plain Local");
            }
            other => panic!("B-5 regression: expected Local, got {other:?}"),
        }
    }

    /// If `tokens` is a state field AND in_actor_body=true, the state-field
    /// rule takes priority (OQ-PF003 resolution: state-field wins).
    #[test]
    fn lower_ident_param_shadows_state_field() {
        use ridge_resolve::LocalId;

        let ident_span = sp_at(10, 16);
        // Binding is Local (same as a handler param would produce).
        let mut ctx = make_actor_body_ctx(
            ident_span,
            Binding::Local(LocalId(99)), // simulate a param with local id 99
            vec!["tokens"],
        );

        let ident = Ident {
            text: "tokens".into(),
            span: ident_span,
        };
        let ir = lower_ident(&mut ctx, &ident);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);

        // OQ-PF003: state-field wins unconditionally inside in_actor_body.
        // If Phase 4 forbids param/state collision, this case never arises;
        // if it allows it, the state-field takes priority.
        match ir {
            IrExpr::Field { field, .. } => {
                assert_eq!(
                    field, "tokens",
                    "state-field precedence: must be Field(tokens)"
                );
            }
            other => {
                panic!("B-5/OQ-PF003: expected Field(tokens) (state-field wins), got {other:?}")
            }
        }
    }
}
