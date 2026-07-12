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
use ridge_types::{CapRow, TyConId, TyConKind, Type};

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
        // Params lower via `lower_lambda_params`: plain `Var`/`_` bind directly;
        // any destructuring pattern feeds a `match` wrapped around the body so
        // its bindings survive lowering.
        Expr::Lambda { params, body, span } => {
            // Quotation: a lambda captured as a quote during type-checking is
            // reified into a `QExpr` tree rather than lowered to a closure. A
            // grouped-aggregate quote (`having`/`summarize`) reifies over the
            // group vocabulary (`g.key`, `g.count`, `g.sum(col)`, …) instead.
            if let Some(qi) = ctx.lookup_quoted(*span) {
                if qi.group {
                    return reify_group_quote(ctx, body, *span, &qi.param_name);
                }
                return reify_quote(ctx, body, *span, params, qi.avg_interval);
            }
            let id = ctx.fresh_id(None);
            let (ir_params, pattern_entries) = lower_lambda_params(ctx, *span, params);

            let lowered_body = lower_expr(ctx, body);
            let wrapped_body = wrap_pattern_params(ctx, lowered_body, pattern_entries);

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
            } else if matches!(&binding, Some(Binding::StdlibSymbol { .. })) {
                // A constructor of a reconciled stdlib type. Resolve was unable to
                // carry its owner/variant (it bound a bare `StdlibSymbol`), so the
                // shape comes from the arena decl interned in the reserved block.
                if let Some((owner_type, variant, is_record)) = ctx.lookup_stdlib_ctor(&ctor_name) {
                    let ctor_kind = if is_record {
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
                            variant,
                        },
                        fields: ir_fields,
                        span: *span,
                    }
                } else {
                    // Unknown stdlib constructor (no reconciled decl): defensive
                    // fallback, same shape as the no-binding arm below.
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
            } else if let Some(Binding::Constructor {
                owner_type: sym_id,
                variant,
                is_record,
                ..
            }) = &binding
            {
                // User constructor (record auto-ctor or union variant).
                // Use the `is_record` flag carried by `Binding::Constructor`
                // which the resolver sets accurately based on the type body.
                let owner_type = ctx
                    .lookup_constructor_tycon(*sym_id)
                    .or_else(|| ctx.lookup_tycon_by_name(&ctor_name))
                    .unwrap_or(TyConId(0));
                if !*is_record && !ir_fields.is_empty() {
                    // Record-payload union variant `Login { userId = 7, at = t }`.
                    // Its runtime shape is `{'Login', #{userId => 7, at => t}}` — a
                    // tagged tuple whose single payload is the record map. Nest the
                    // fields inside a `Construct { Record }` slot so the existing
                    // UnionVariant (tagged tuple) and Record (map literal) codegen
                    // arms compose without a bespoke case. Field order is irrelevant
                    // because the payload is a map keyed by name.
                    let inner_id = ctx.fresh_id(None);
                    let inner = IrExpr::Construct {
                        id: inner_id,
                        ctor: SymbolRef::Constructor {
                            ctor_kind: ridge_ir::CtorKind::Record,
                            owner_type,
                            name: ctor_name.clone(),
                            variant: 0,
                        },
                        fields: ir_fields,
                        span: *span,
                    };
                    IrExpr::Construct {
                        id,
                        ctor: SymbolRef::Constructor {
                            ctor_kind: ridge_ir::CtorKind::UnionVariant,
                            owner_type,
                            name: ctor_name,
                            variant: *variant,
                        },
                        fields: vec![("0".to_string(), inner)],
                        span: *span,
                    }
                } else {
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

            // Bare class-method call (`describe Red`): pin the constraint from
            // the argument occupying the class type variable's position, so two
            // distinct instances each dispatch to the right dictionary. Without
            // this, the callee lowering falls back to the sole-static-plan
            // heuristic, which cannot tell two same-class instances apart.
            if let Some(call) = try_lower_classmethod_call(ctx, callee, args, id, *span) {
                return call;
            }

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
            // The call's own result type pins a constraint variable that lives
            // only in the callee's return type (a return-pinned class method).
            let call_result_ty = ctx
                .node_id_map
                .as_ref()
                .and_then(|m| m.get(*span, NodeKind::Expr))
                .and_then(|nid| ctx.node_type(nid).cloned());
            let dict_args =
                build_dict_args(ctx, &ir_callee, &arg_types, call_result_ty.as_ref(), *span);
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
        // `{ f = v, … }` lowers to `IrExpr::Construct { Record, .. }`. The codegen
        // layer drops the constructor tag for Record-kind constructs and emits a
        // BEAM map, so the owner is a placeholder — no codegen change needed.
        Expr::RecordLit { fields, span } => {
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

            // Record literals are structural: the type checker infers them as
            // `Type::Record`, never a nominal `Type::Con`, so there is no anon
            // `TyConId` to recover from the node-type table. Codegen lowers a
            // Record `Construct` to a bare BEAM map (see `lower_construct`) and
            // ignores `owner_type`/`name`, so the placeholder owner is what ends
            // up emitted regardless. `name` stays a readable label for the IR.
            let owner_type = TyConId(0);
            let record_name = ctx
                .workspace
                .and_then(|ws| ws.tycons.get(owner_type.0 as usize))
                .map_or_else(
                    || format!("{{anon record #{}}}", owner_type.0),
                    |d| d.name.clone(),
                );

            let id = ctx.fresh_id(None);
            IrExpr::Construct {
                id,
                ctor: SymbolRef::Constructor {
                    ctor_kind: ridge_ir::CtorKind::Record,
                    owner_type,
                    name: record_name,
                    variant: 0,
                },
                fields: ir_fields,
                span: *span,
            }
        }
    }
}

// ── Quotation reification (std.query) ─────────────────────────────────────────

/// The `QExpr` builtin `TyConId` (see `ridge_types::builtins`).
const QEXPR_TYCON: TyConId = TyConId(25);

/// Reify a quoted lambda body into a `Quote { tree }` value.
///
/// `QExpr` and `Quote` are prelude builtins, so the tree is built directly as
/// `Construct` nodes over the `QExpr` constructors (the same way `Ordering` and
/// `JsonValue` are synthesised), and the `Quote` wrapper is a record that lowers
/// to a map. No other module's constructors are referenced.
///
/// A multi-parameter quote (a join condition or projection) ranges over one
/// entity per parameter — a left and a right for a binary join, three or more
/// for an N-ary one. A column reifies to the node for its source's leaf index:
/// `QCol` for the first (or only) parameter, `QColR` for the second, and
/// `QColAt <i>` for the third onward. The leaf order is the parameter order, so
/// it lines up with the left-to-right walk of the join tree. `params` carries
/// the lambda's parameters so each one's name can be matched to its index.
///
/// `avg_interval` is set by the type-checker (`mark_avg_interval_accessor`) for
/// a scalar `avgOf` accessor whose column is a `Duration`: the reified tree is
/// wrapped in `QAggAvgInterval`, the same node a grouped `g.avg` reifies to for
/// an interval column, so `std.repo`'s scalar `avgOf` reads it to pick the
/// interval-aware `AVG_INTERVAL` keyword instead of a runtime `SqlType`
/// dictionary, which the accessor's fundep'd instance cannot resolve reliably.
fn reify_quote(
    ctx: &mut LowerCtx<'_>,
    body: &Expr,
    span: Span,
    params: &[LambdaParam],
    avg_interval: bool,
) -> IrExpr {
    let names: Vec<String> = params
        .iter()
        .map(|p| lambda_param_name(Some(p)).unwrap_or_default())
        .collect();
    let mut tree = reify_node(ctx, body, &names);
    if avg_interval {
        tree = qexpr_node(ctx, "QAggAvgInterval", 40, vec![tree], span);
    }
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: ridge_ir::CtorKind::Record,
            owner_type: TyConId(0),
            name: "Quote".to_string(),
            variant: 0,
        },
        fields: vec![("tree".to_string(), tree)],
        span,
    }
}

/// Build a `QExpr` union-variant node with positional payloads named `$0`, `$1`.
fn qexpr_node(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    variant: u32,
    args: Vec<IrExpr>,
    span: Span,
) -> IrExpr {
    let fields = args
        .into_iter()
        .enumerate()
        .map(|(i, a)| (format!("${i}"), a))
        .collect();
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: ridge_ir::CtorKind::UnionVariant,
            owner_type: QEXPR_TYCON,
            name: name.to_string(),
            variant,
        },
        fields,
        span,
    }
}

/// The resolved type of a captured identifier at `span`, peeled of aliases.
/// `infer_expr` writes ident node types under `NodeKind::Expr`; the `Ident`
/// fallback guards against any future wrapper-keying change.
fn captured_ident_type(ctx: &LowerCtx<'_>, span: Span) -> Option<Type> {
    let m = ctx.node_id_map.as_ref()?;
    let nid = m
        .get(span, NodeKind::Expr)
        .or_else(|| m.get(span, NodeKind::Ident))?;
    ctx.node_type(nid).cloned().map(|t| deep_peel_alias(&t))
}

/// Whether the grouped `g.avg` at `span` folds a `Duration` column. The quotation
/// checker stamps the folded column's type at the aggregate call, so an interval
/// average reifies to the interval-aware node — Postgres cannot cast an interval
/// average to `float8`, so it reads the average's epoch milliseconds instead. Any
/// other column type, or a missing stamp, keeps the plain `QAggAvg`.
fn agg_col_is_interval(ctx: &LowerCtx<'_>, span: Span) -> bool {
    let Some(ws) = ctx.workspace else {
        return false;
    };
    matches!(captured_ident_type(ctx, span), Some(Type::Con(id, _)) if id == ws.builtins.duration)
}

/// The `QExpr` literal constructor (`name`, `variant`) for a captured scalar's
/// resolved type. The quotation checker accepts the same scalar set
/// (`is_quote_scalar`: Int/Text/Bool/Float/Decimal/Uuid/Timestamp/Bytes/Date/Time),
/// so a `None` here is an internal invariant violation, not a user error.
fn captured_scalar_qlit(ctx: &LowerCtx<'_>, ty: &Type) -> Option<(&'static str, u32)> {
    let b = &ctx.workspace?.builtins;
    match ty {
        Type::Con(id, _) if *id == b.int => Some(("QLitInt", 1)),
        Type::Con(id, _) if *id == b.float => Some(("QLitFloat", 4)),
        Type::Con(id, _) if *id == b.bool => Some(("QLitBool", 3)),
        Type::Con(id, _) if *id == b.text => Some(("QLitText", 2)),
        Type::Con(id, _) if *id == b.decimal => Some(("QLitDecimal", 33)),
        Type::Con(id, _) if *id == b.uuid => Some(("QLitUuid", 34)),
        Type::Con(id, _) if *id == b.timestamp => Some(("QLitInstant", 35)),
        Type::Con(id, _) if *id == b.bytes => Some(("QLitBytes", 36)),
        Type::Con(id, _) if *id == b.date => Some(("QLitDate", 37)),
        Type::Con(id, _) if *id == b.time => Some(("QLitTime", 38)),
        Type::Con(id, _) if *id == b.duration => Some(("QLitInterval", 39)),
        _ => None,
    }
}

/// Reify one node of a quoted predicate into a `QExpr` value.
///
/// The shape was already validated by the quotation type-checker, so anything
/// unexpected here is an internal invariant violation, not a user error.
/// Pick the `SymbolRef` for an imported symbol. A cross-stdlib-module target
/// (the producer is a stdlib module) routes through the stdlib bridge so its
/// BEAM atom is the dotted FQN (`'std.sql':sqlInt`); a user-module target keeps
/// the `ridge_module_<id>` external mangle.
fn imported_symbol_ref(ctx: &LowerCtx<'_>, module: ModuleId, name: String) -> SymbolRef {
    if let Some(fqn) = ctx.stdlib_fqn(module) {
        SymbolRef::Stdlib {
            module: fqn.to_string(),
            name,
        }
    } else {
        SymbolRef::External { module, name }
    }
}

/// The bound name of a lambda parameter, if it is a plain (optionally annotated)
/// name. The quotation checker has already rejected any other parameter shape.
fn lambda_param_name(p: Option<&LambdaParam>) -> Option<String> {
    match p? {
        LambdaParam::Pattern(Pattern::Var { name, .. })
        | LambdaParam::Annotated {
            pat: Pattern::Var { name, .. },
            ..
        } => Some(name.text.clone()),
        _ => None,
    }
}

/// Reify one node of a quoted body into a `QExpr` value. `params` is the quote's
/// parameter names in order, so a column access can be tagged with its source's
/// leaf index: the first parameter reifies to `QCol`, the second to `QColR`, and
/// the third onward to `QColAt <i>`. A single-parameter quote passes one name and
/// every column stays `QCol`.
#[expect(
    clippy::too_many_lines,
    reason = "one linear walk over the quoted-body node shapes (column access, literals, comparisons, boolean operators); splitting it would scatter the shared reification setup"
)]
fn reify_node(ctx: &mut LowerCtx<'_>, e: &Expr, params: &[String]) -> IrExpr {
    use ridge_ast::BinOp;
    match e {
        Expr::Paren { inner, .. } => reify_node(ctx, inner, params),

        // `u.field` → the column node for `u`'s leaf: `QCol` for the first
        // parameter (or a single-table quote), `QColR` for the second, and
        // `QColAt <i>` for the third onward.
        Expr::FieldAccess { base, field, span } => {
            let col = ridge_ast::column_mirror::column_sql_name(&field.text);
            let name_lit = IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Text(col),
                span: *span,
            };
            let leaf = match base.as_ref() {
                Expr::Ident(id) => params.iter().position(|n| n == &id.text),
                _ => None,
            };
            match leaf {
                Some(1) => qexpr_node(ctx, "QColR", 15, vec![name_lit], *span),
                Some(i) if i >= 2 => {
                    let idx_lit = IrExpr::Lit {
                        id: ctx.fresh_id(None),
                        value: IrLit::Int(i64::try_from(i).unwrap_or(i64::MAX)),
                        span: *span,
                    };
                    qexpr_node(ctx, "QColAt", 22, vec![idx_lit, name_lit], *span)
                }
                _ => qexpr_node(ctx, "QCol", 0, vec![name_lit], *span),
            }
        }

        // A literal → `QLit{Int,Text,Bool,Float} <value>`.
        Expr::Literal(lit) => {
            let span = lit.span();
            let value = lower_expr(ctx, e);
            let (name, variant) = match lit {
                Literal::IntDec { .. }
                | Literal::IntBin { .. }
                | Literal::IntOct { .. }
                | Literal::IntHex { .. } => ("QLitInt", 1),
                Literal::Float { .. } => ("QLitFloat", 4),
                Literal::Decimal { .. } => ("QLitDecimal", 33),
                Literal::Bool { .. } => ("QLitBool", 3),
                Literal::Text { .. } | Literal::RawText { .. } => ("QLitText", 2),
            };
            qexpr_node(ctx, name, variant, vec![value], span)
        }

        // A comparison or boolean connective → the matching `QExpr` node.
        Expr::Binary { op, lhs, rhs, span } => {
            let (name, variant) = match op {
                BinOp::And => ("QAnd", 5),
                BinOp::Or => ("QOr", 6),
                BinOp::Eq => ("QEq", 8),
                BinOp::Ne => ("QNe", 9),
                BinOp::Lt => ("QLt", 10),
                BinOp::Gt => ("QGt", 11),
                BinOp::Le => ("QLe", 12),
                BinOp::Ge => ("QGe", 13),
                // Arithmetic value nodes — both operands reify recursively, so a
                // nested `price * qty + 1` builds the matching `QAdd`/`QMul` tree.
                BinOp::Add => ("QAdd", 26),
                BinOp::Sub => ("QSub", 27),
                BinOp::Mul => ("QMul", 28),
                BinOp::Div => ("QDiv", 29),
                BinOp::Mod => ("QMod", 30),
                _ => {
                    ctx.errors.push(LowerError::InternalLoweringError {
                        span: *span,
                        message: "unsupported operator survived quote checking".into(),
                    });
                    return unit_lit(ctx, *span);
                }
            };
            let l = reify_node(ctx, lhs, params);
            let r = reify_node(ctx, rhs, params);
            qexpr_node(ctx, name, variant, vec![l, r], *span)
        }

        // A conditional → `QCase <cond> <then> <else>`. The condition and both
        // branches reify recursively. The quotation checker has guaranteed an
        // else branch and that the branches agree — two values of one type, or
        // two predicates — so the same node serves a value CASE and a boolean
        // CASE alike.
        Expr::If {
            cond,
            then_branch,
            else_branch,
            span,
        } => {
            let c = reify_node(ctx, cond, params);
            let t = reify_node(ctx, then_branch, params);
            let e = if let Some(eb) = else_branch {
                reify_node(ctx, eb, params)
            } else {
                ctx.errors.push(LowerError::InternalLoweringError {
                    span: *span,
                    message: "if without else survived quote checking".into(),
                });
                unit_lit(ctx, *span)
            };
            qexpr_node(ctx, "QCase", 31, vec![c, t, e], *span)
        }

        // A predicate helper — `Text.like`/`contains`/`startsWith`/`endsWith` →
        // `QLike`, `List.contains` → `QIn`. The quotation checker has already pinned
        // which operand is the column and which the literal, so the column reifies
        // through the same `QCol`/`QColR`/`QColAt` path as a comparison's and the
        // literal supplies the pattern or the IN set.
        Expr::Call { callee, args, span } => {
            let arg_refs: Vec<&Expr> = args.iter().collect();
            if let Some(negated) = exists_verb_kind(callee_last_name(callee)) {
                return reify_exists(ctx, &arg_refs, negated, *span, params);
            }
            reify_predicate_call(ctx, callee, &arg_refs, *span, params)
        }
        // `value |> f rest` reifies the same as `f rest value`: the piped value is
        // the call's last argument, mirroring the pipe desugaring, so a predicate
        // helper reads the same written either way.
        Expr::Pipe { lhs, rhs, span } => {
            let rhs_inner = peel_paren(rhs);
            match rhs_inner {
                Expr::Call { callee, args, .. } => {
                    let mut arg_refs: Vec<&Expr> = args.iter().collect();
                    arg_refs.push(lhs.as_ref());
                    if let Some(negated) = exists_verb_kind(callee_last_name(callee)) {
                        return reify_exists(ctx, &arg_refs, negated, *span, params);
                    }
                    reify_predicate_call(ctx, callee, &arg_refs, *span, params)
                }
                Expr::Ident(_) | Expr::Qualified(_) => {
                    reify_predicate_call(ctx, rhs_inner, &[lhs.as_ref()], *span, params)
                }
                _ => {
                    ctx.errors.push(LowerError::InternalLoweringError {
                        span: *span,
                        message: "unsupported pipe survived quote checking".into(),
                    });
                    unit_lit(ctx, *span)
                }
            }
        }

        // `{ field = u.col, … }` (and the named `Shape { field = u.col, … }`,
        // whose constructor only names the decode target) → `QProj [(alias,
        // QCol "col"), …]` — a select-list. Each field's name is the output
        // alias (its SQL-cased column name); the value reifies to the projected
        // column. The constructor name is irrelevant to the SQL, so both record
        // forms reify the same way.
        Expr::RecordLit { fields, span } | Expr::Record { fields, span, .. } => {
            let items: Vec<IrExpr> = fields
                .iter()
                .map(|fi| {
                    let alias = ridge_ast::column_mirror::column_sql_name(&fi.name.text);
                    let alias_lit = IrExpr::Lit {
                        id: ctx.fresh_id(None),
                        value: IrLit::Text(alias),
                        span: fi.span,
                    };
                    let col = if let Some(v) = &fi.value {
                        reify_node(ctx, v, params)
                    } else {
                        ctx.errors.push(LowerError::InternalLoweringError {
                            span: fi.span,
                            message: "shorthand projection field survived quote checking".into(),
                        });
                        unit_lit(ctx, fi.span)
                    };
                    IrExpr::Tuple {
                        id: ctx.fresh_id(None),
                        elems: vec![alias_lit, col],
                        span: fi.span,
                    }
                })
                .collect();
            let list = IrExpr::ListLit {
                id: ctx.fresh_id(None),
                elems: items,
                span: *span,
            };
            qexpr_node(ctx, "QProj", 14, vec![list], *span)
        }

        // A variable captured from the enclosing scope → the matching `QLit*`
        // node wrapping its runtime value, so it lands as a `$N` bind exactly
        // like an inline literal. The quotation checker has already verified the
        // variable is a base scalar (Int/Text/Bool/Float). `lower_expr` resolves
        // the identifier to its local binding, so the value plugged into the node
        // is the variable's value at the point the quote is built.
        Expr::Ident(id) => {
            let span = id.span;
            if let Some((name, variant)) =
                captured_ident_type(ctx, span).and_then(|ty| captured_scalar_qlit(ctx, &ty))
            {
                let value = lower_expr(ctx, e);
                qexpr_node(ctx, name, variant, vec![value], span)
            } else {
                ctx.errors.push(LowerError::InternalLoweringError {
                    span,
                    message: "captured variable of unsupported type survived quote checking".into(),
                });
                unit_lit(ctx, span)
            }
        }

        other => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span: other.span(),
                message: "unsupported expression survived quote checking".into(),
            });
            unit_lit(ctx, other.span())
        }
    }
}

/// How a recognised text predicate builds its SQL LIKE pattern.
#[derive(Clone, Copy)]
enum LikeMode {
    /// `Text.like` — the literal is the pattern verbatim.
    Raw,
    /// `Text.contains` — `%needle%`, the needle's wildcards escaped.
    Contains,
    /// `Text.startsWith` — `needle%`.
    Prefix,
    /// `Text.endsWith` — `%needle`.
    Suffix,
}

/// Peel any number of parentheses from an expression.
fn peel_paren(e: &Expr) -> &Expr {
    match e {
        Expr::Paren { inner, .. } => peel_paren(inner),
        other => other,
    }
}

/// The last segment of a call's callee — the function name, whether written bare
/// (`contains`) or qualified (`List.contains`).
fn callee_last_name(callee: &Expr) -> Option<&str> {
    match callee {
        Expr::Ident(id) => Some(id.text.as_str()),
        Expr::Qualified(qn) => qn.segments.last().map(|s| s.text.as_str()),
        _ => None,
    }
}

/// Whether `e` is a column access on one of the quote's parameters (`u.field`).
fn is_param_column(e: &Expr, params: &[String]) -> bool {
    matches!(
        peel_paren(e),
        Expr::FieldAccess { base, .. }
            if matches!(base.as_ref(), Expr::Ident(id) if params.iter().any(|n| n == &id.text))
    )
}

/// Reify a predicate-helper call into a `QLike` or `QIn` node. The checker has
/// validated the operands, so one is a column of the quote and the other a literal
/// (a text pattern, or a list of literals for `IN`).
fn reify_predicate_call(
    ctx: &mut LowerCtx<'_>,
    callee: &Expr,
    args: &[&Expr],
    span: Span,
    params: &[String],
) -> IrExpr {
    let internal = |ctx: &mut LowerCtx<'_>| -> IrExpr {
        ctx.errors.push(LowerError::InternalLoweringError {
            span,
            message: "unsupported predicate helper survived quote checking".into(),
        });
        unit_lit(ctx, span)
    };
    if args.len() != 2 {
        return internal(ctx);
    }
    let (a0, a1) = (peel_paren(args[0]), peel_paren(args[1]));
    let (col, other) = if is_param_column(a0, params) {
        (a0, a1)
    } else if is_param_column(a1, params) {
        (a1, a0)
    } else {
        return internal(ctx);
    };
    match callee_last_name(callee) {
        Some("contains") => {
            if let Expr::List { elems, .. } = other {
                reify_in(ctx, col, elems, span, params)
            } else if let Some(node) = reify_in_runtime(ctx, col, other, span, params) {
                node
            } else {
                reify_like(ctx, col, other, LikeMode::Contains, span, params)
            }
        }
        Some("startsWith") => reify_like(ctx, col, other, LikeMode::Prefix, span, params),
        Some("endsWith") => reify_like(ctx, col, other, LikeMode::Suffix, span, params),
        Some("like") => reify_like(ctx, col, other, LikeMode::Raw, span, params),
        _ => internal(ctx),
    }
}

/// `exists` reifies to a bare `QExists`; `notExists` wraps it in a `QNot`. Any
/// other name is not a correlated-subquery verb.
fn exists_verb_kind(name: Option<&str>) -> Option<bool> {
    match name {
        Some("exists") => Some(false),
        Some("notExists") => Some(true),
        _ => None,
    }
}

/// Reify `exists inner (fn p -> <corr>)` / `notExists …` into a `QExists` node.
/// The inner table's name is read off the captured repo at run time (`repo.table`),
/// so the node carries it as a runtime `Text` rather than a baked literal — the same
/// runtime capture a scalar predicate parameter takes. The correlated predicate is
/// reified over the outer row(s) followed by the inner row, so the outer columns
/// keep their `QCol`/`QColR` leaf and the inner row becomes the next leaf (`QColR`
/// for a single-table outer) — the two-row shape the backends' join-predicate path
/// already evaluates. `notExists` wraps the probe in a `QNot`.
fn reify_exists(
    ctx: &mut LowerCtx<'_>,
    args: &[&Expr],
    negated: bool,
    span: Span,
    params: &[String],
) -> IrExpr {
    let internal = |ctx: &mut LowerCtx<'_>| -> IrExpr {
        ctx.errors.push(LowerError::InternalLoweringError {
            span,
            message: "unsupported exists survived quote checking".into(),
        });
        unit_lit(ctx, span)
    };
    if args.len() != 2 {
        return internal(ctx);
    }
    let (a0, a1) = (peel_paren(args[0]), peel_paren(args[1]));
    let (repo_expr, lambda) = if matches!(a1, Expr::Lambda { .. }) {
        (a0, a1)
    } else if matches!(a0, Expr::Lambda { .. }) {
        (a1, a0)
    } else {
        return internal(ctx);
    };
    let Expr::Lambda {
        params: lam_params,
        body,
        ..
    } = lambda
    else {
        return internal(ctx);
    };
    let Some(inner_name) = lambda_param_name(lam_params.first()) else {
        return internal(ctx);
    };
    let mut inner_params: Vec<String> = params.to_vec();
    inner_params.push(inner_name);
    let corr = reify_node(ctx, body, &inner_params);
    let repo_ir = lower_expr(ctx, repo_expr);
    let table_ir = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(repo_ir),
        field: "table".to_string(),
        span,
    };
    let node = qexpr_node(ctx, "QExists", 32, vec![table_ir, corr], span);
    if negated {
        qexpr_node(ctx, "QNot", 7, vec![node], span)
    } else {
        node
    }
}

/// Build a `QLike (column, QLitText pattern)` node, wrapping and escaping the
/// literal needle per `mode`.
fn reify_like(
    ctx: &mut LowerCtx<'_>,
    col: &Expr,
    pat: &Expr,
    mode: LikeMode,
    span: Span,
    params: &[String],
) -> IrExpr {
    let col_ir = reify_node(ctx, col, params);
    let needle = decoded_text(ctx, pat).unwrap_or_default();
    let pattern = match mode {
        LikeMode::Raw => needle,
        LikeMode::Contains => format!("%{}%", escape_like(&needle)),
        LikeMode::Prefix => format!("{}%", escape_like(&needle)),
        LikeMode::Suffix => format!("%{}", escape_like(&needle)),
    };
    let pat_ir = build_qlit_text(ctx, pattern, span);
    qexpr_node(ctx, "QLike", 24, vec![col_ir, pat_ir], span)
}

/// Build a `QIn (column, [elements])` node from a list literal of literals.
fn reify_in(
    ctx: &mut LowerCtx<'_>,
    col: &Expr,
    elems: &[Expr],
    span: Span,
    params: &[String],
) -> IrExpr {
    let col_ir = reify_node(ctx, col, params);
    let items: Vec<IrExpr> = elems.iter().map(|el| reify_node(ctx, el, params)).collect();
    let list_ir = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: items,
        span,
    };
    qexpr_node(ctx, "QIn", 25, vec![col_ir, list_ir], span)
}

/// Reify `List.contains col capturedList`, where `capturedList` is a value from the
/// enclosing scope of type `List <scalar>`. Each element is wrapped at run time in
/// its `QLit*` node, so the captured list renders through the same `QIn` path as a
/// literal list — a runtime `IN (…)` with one `$N` bind per element, the parity of
/// `ids.Contains(row.col)`. Returns `None` when `other` is not a captured scalar
/// list, so a `contains` over a text needle still falls through to the substring
/// `LIKE`.
fn reify_in_runtime(
    ctx: &mut LowerCtx<'_>,
    col: &Expr,
    other: &Expr,
    span: Span,
    params: &[String],
) -> Option<IrExpr> {
    let Expr::Ident(id) = peel_paren(other) else {
        return None;
    };
    let list_ty = captured_ident_type(ctx, id.span)?;
    let elem_ty = list_elem_type(ctx, &list_ty)?;
    let (name, variant) = captured_scalar_qlit(ctx, &elem_ty)?;
    let col_ir = reify_node(ctx, col, params);
    let list_ir = lower_expr(ctx, other);
    let items = map_to_qlit(ctx, name, variant, &elem_ty, list_ir, span);
    Some(qexpr_node(ctx, "QIn", 25, vec![col_ir, items], span))
}

/// The element type of a `List a`, peeled of aliases. `None` for any other shape.
fn list_elem_type(ctx: &LowerCtx<'_>, ty: &Type) -> Option<Type> {
    let list = ctx.workspace?.builtins.list;
    match ty {
        Type::Con(id, args) if *id == list && args.len() == 1 => Some(deep_peel_alias(&args[0])),
        _ => None,
    }
}

/// Build `std.list.map (fn x -> QLit* x) list` — the runtime `List QExpr` of
/// literal nodes a `QIn` consumes. The wrapping lambda is pure and `map` is
/// capability-transparent, so the synthesised call stays pure.
fn map_to_qlit(
    ctx: &mut LowerCtx<'_>,
    qlit_name: &str,
    variant: u32,
    elem_ty: &Type,
    list_ir: IrExpr,
    span: Span,
) -> IrExpr {
    let pname = ctx.fresh_local("__in");
    let param = IrParam {
        name: pname.clone(),
        ty: elem_ty.clone(),
        span,
    };
    let local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: pname,
        span,
    };
    let body = qexpr_node(ctx, qlit_name, variant, vec![local], span);
    let lambda = IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params: vec![param],
        body: Box::new(body),
        caps: ridge_types::CapabilitySet::PURE,
        span,
    };
    let map_sym = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.list".into(),
            name: "map".into(),
        },
        span,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(map_sym),
        args: vec![lambda, list_ir],
        span,
    }
}

/// The decoded value of a text literal, read back from its lowered `IrLit::Text`
/// so escape sequences are already resolved.
fn decoded_text(ctx: &mut LowerCtx<'_>, e: &Expr) -> Option<String> {
    match lower_expr(ctx, e) {
        IrExpr::Lit {
            value: IrLit::Text(s),
            ..
        } => Some(s),
        _ => None,
    }
}

/// Wrap a finished pattern string in a `QLitText` node.
fn build_qlit_text(ctx: &mut LowerCtx<'_>, s: String, span: Span) -> IrExpr {
    let lit = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Text(s),
        span,
    };
    qexpr_node(ctx, "QLitText", 2, vec![lit], span)
}

/// Escape the SQL LIKE metacharacters in a literal needle so a `contains`/
/// `startsWith`/`endsWith` match treats it as plain text. `\` is the escape
/// character (Postgres' default), so it, `%`, and `_` each get a leading `\`.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ── Grouped-aggregate quote reification ───────────────────────────────────────
//
// A `having`/`summarize` body reifies over the group vocabulary rather than the
// row columns: `g.key` → `QGroupKey`, `g.count` → `QAggCount`, and
// `g.sum`/`avg`/`min`/`max (fn u -> u.col)` → `QAgg{Sum,Avg,Min,Max} (QCol col)`.
// A `summarize` projection record reifies to a `QProj` of these; a `having`
// predicate reifies its comparisons and connectives the same way a row predicate
// does, with the aggregate nodes as operands.

/// Reify a grouped-aggregate lambda body into a `Quote { tree }` value. `g_name`
/// is the group parameter's name (the base of `g.key`/`g.count`/`g.sum(…)`).
fn reify_group_quote(ctx: &mut LowerCtx<'_>, body: &Expr, span: Span, g_name: &str) -> IrExpr {
    let tree = reify_group_node(ctx, body, g_name);
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: ridge_ir::CtorKind::Record,
            owner_type: TyConId(0),
            name: "Quote".to_string(),
            variant: 0,
        },
        fields: vec![("tree".to_string(), tree)],
        span,
    }
}

/// Whether `base` is the group parameter `g_name`.
fn is_group_base(base: &Expr, g_name: &str) -> bool {
    matches!(base, Expr::Ident(id) if id.text == g_name)
}

/// Reify one node of a grouped-aggregate body into a `QExpr` value. The shape was
/// validated by the quotation checker, so anything unexpected is an internal
/// invariant violation, not a user error.
#[expect(
    clippy::too_many_lines,
    reason = "one linear walk over the grouped-quote node shapes (g.key/g.count, the scalar aggregates, having comparisons, the projection record); splitting it would scatter the QExpr variant mapping"
)]
fn reify_group_node(ctx: &mut LowerCtx<'_>, e: &Expr, g_name: &str) -> IrExpr {
    use ridge_ast::BinOp;
    match e {
        Expr::Paren { inner, .. } => reify_group_node(ctx, inner, g_name),

        // `g.key` → `QGroupKey`; `g.count` → `QAggCount`.
        Expr::FieldAccess { base, field, span } if is_group_base(base, g_name) => {
            match field.text.as_str() {
                "key" => qexpr_node(ctx, "QGroupKey", 16, vec![], *span),
                "count" => qexpr_node(ctx, "QAggCount", 17, vec![], *span),
                _ => {
                    ctx.errors.push(LowerError::InternalLoweringError {
                        span: *span,
                        message: "unsupported group field survived quote checking".into(),
                    });
                    unit_lit(ctx, *span)
                }
            }
        }

        // `g.sum`/`avg`/`min`/`max (fn u -> u.col)` → `QAgg* (QCol col)`.
        Expr::Call { callee, args, span } => {
            if let Expr::FieldAccess { base, field, .. } = callee.as_ref() {
                if is_group_base(base, g_name) {
                    let agg = match field.text.as_str() {
                        "sum" => Some(("QAggSum", 18)),
                        "avg" => Some(if agg_col_is_interval(ctx, *span) {
                            ("QAggAvgInterval", 40)
                        } else {
                            ("QAggAvg", 19)
                        }),
                        "min" => Some(("QAggMin", 20)),
                        "max" => Some(("QAggMax", 21)),
                        _ => None,
                    };
                    if let (Some((name, variant)), Some(arg)) = (agg, args.first()) {
                        let col = reify_group_agg_col(ctx, arg);
                        return qexpr_node(ctx, name, variant, vec![col], *span);
                    }
                }
            }
            ctx.errors.push(LowerError::InternalLoweringError {
                span: *span,
                message: "unsupported group call survived quote checking".into(),
            });
            unit_lit(ctx, *span)
        }

        // A literal operand of a `having` comparison.
        Expr::Literal(lit) => {
            let span = lit.span();
            let value = lower_expr(ctx, e);
            let (name, variant) = match lit {
                Literal::IntDec { .. }
                | Literal::IntBin { .. }
                | Literal::IntOct { .. }
                | Literal::IntHex { .. } => ("QLitInt", 1),
                Literal::Float { .. } => ("QLitFloat", 4),
                Literal::Decimal { .. } => ("QLitDecimal", 33),
                Literal::Bool { .. } => ("QLitBool", 3),
                Literal::Text { .. } | Literal::RawText { .. } => ("QLitText", 2),
            };
            qexpr_node(ctx, name, variant, vec![value], span)
        }

        // A `having` comparison or connective → the matching `QExpr` node, its
        // operands reified over the group vocabulary.
        Expr::Binary { op, lhs, rhs, span } => {
            let (name, variant) = match op {
                BinOp::And => ("QAnd", 5),
                BinOp::Or => ("QOr", 6),
                BinOp::Eq => ("QEq", 8),
                BinOp::Ne => ("QNe", 9),
                BinOp::Lt => ("QLt", 10),
                BinOp::Gt => ("QGt", 11),
                BinOp::Le => ("QLe", 12),
                BinOp::Ge => ("QGe", 13),
                _ => {
                    ctx.errors.push(LowerError::InternalLoweringError {
                        span: *span,
                        message: "unsupported operator survived group quote checking".into(),
                    });
                    return unit_lit(ctx, *span);
                }
            };
            let l = reify_group_node(ctx, lhs, g_name);
            let r = reify_group_node(ctx, rhs, g_name);
            qexpr_node(ctx, name, variant, vec![l, r], *span)
        }

        // A `summarize` projection: `Stats { dept = g.key, … }` → `QProj
        // [(alias, <agg node>), …]`. Each field name is the output alias; the
        // value reifies to its group aggregate.
        Expr::RecordLit { fields, span } | Expr::Record { fields, span, .. } => {
            let items: Vec<IrExpr> = fields
                .iter()
                .map(|fi| {
                    let alias = ridge_ast::column_mirror::column_sql_name(&fi.name.text);
                    let alias_lit = IrExpr::Lit {
                        id: ctx.fresh_id(None),
                        value: IrLit::Text(alias),
                        span: fi.span,
                    };
                    let agg = if let Some(v) = &fi.value {
                        reify_group_node(ctx, v, g_name)
                    } else {
                        ctx.errors.push(LowerError::InternalLoweringError {
                            span: fi.span,
                            message: "shorthand group projection field survived quote checking"
                                .into(),
                        });
                        unit_lit(ctx, fi.span)
                    };
                    IrExpr::Tuple {
                        id: ctx.fresh_id(None),
                        elems: vec![alias_lit, agg],
                        span: fi.span,
                    }
                })
                .collect();
            let list = IrExpr::ListLit {
                id: ctx.fresh_id(None),
                elems: items,
                span: *span,
            };
            qexpr_node(ctx, "QProj", 14, vec![list], *span)
        }

        other => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span: other.span(),
                message: "unsupported expression survived group quote checking".into(),
            });
            unit_lit(ctx, other.span())
        }
    }
}

/// Reify a group aggregate's inner column accessor into the column node it names.
/// A one-row accessor (`fn u -> u.col`) names a single left column; a join accessor
/// (`fn u p -> p.col`) names a column from either side, tagged by leaf index exactly
/// as a row quote tags one — `QCol` for the first parameter, `QColR` for the second,
/// `QColAt <i>` for the third onward.
fn reify_group_agg_col(ctx: &mut LowerCtx<'_>, arg: &Expr) -> IrExpr {
    let mut inner = arg;
    while let Expr::Paren { inner: i, .. } = inner {
        inner = i;
    }
    if let Expr::Lambda { params, body, .. } = inner {
        let names: Vec<String> = params
            .iter()
            .map(|p| lambda_param_name(Some(p)).unwrap_or_default())
            .collect();
        let mut bd: &Expr = body;
        while let Expr::Paren { inner: i, .. } = bd {
            bd = i;
        }
        return reify_node(ctx, bd, &names);
    }
    ctx.errors.push(LowerError::InternalLoweringError {
        span: arg.span(),
        message: "group aggregate column accessor survived quote checking".into(),
    });
    unit_lit(ctx, arg.span())
}

fn unit_lit(ctx: &mut LowerCtx<'_>, span: Span) -> IrExpr {
    IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Unit,
        span,
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
pub(crate) fn build_dict_args(
    ctx: &mut LowerCtx<'_>,
    callee: &IrExpr,
    arg_types: &[Option<Type>],
    call_result_ty: Option<&Type>,
    span: Span,
) -> Vec<IrExpr> {
    // Constrained callees take one dict arg per constraint. A `SymbolRef::Local`
    // callee is a (possibly constrained) top-level fn in this module; a
    // `SymbolRef::Stdlib` callee may be a constrained reconciled stdlib fn (e.g.
    // std.repo's `all`/`insertRow`, typed `where Adapter a, Row e`), whose
    // constraints come from the reconciled scheme table rather than this
    // module's fns. Both feed the same dict-building loop below. `ret_ty` is the
    // callee scheme's return type; aligned against the call's own result type it
    // pins a constraint variable that appears only in the result.
    let (constraints, param_types, ret_ty) = match callee {
        IrExpr::Symbol {
            sym: SymbolRef::Local { name, .. },
            ..
        } => {
            let name = name.clone();
            let constraints = ctx.lookup_fn_constraints(&name).to_vec();
            if constraints.is_empty() {
                return vec![];
            }
            let param_types = ctx.lookup_fn_param_types(&name).to_vec();
            let ret_ty = ctx.lookup_fn_ret_type(&name);
            (constraints, param_types, ret_ty)
        }
        IrExpr::Symbol {
            sym: SymbolRef::Stdlib { module, name },
            ..
        } => match ctx.reconciled_stdlib_fn_dict_sig(module, name) {
            Some((constraints, param_types, ret)) if !constraints.is_empty() => {
                (constraints, param_types, Some(ret))
            }
            _ => return vec![],
        },
        _ => return vec![],
    };

    // Extend the pinning search with the callee's return type aligned against the
    // call's own result type. A return-pinned class method (e.g. `Row`'s
    // `fromRow`, whose class variable sits only in `Result e Error`) is otherwise
    // unpinnable when no argument carries the variable — which happens for a
    // piped repository whose type the node map did not record. The result type
    // always carries it, so this keeps the right instance threaded even with two
    // `deriving (Row)` records in scope.
    let mut pin_params: Vec<Type> = param_types;
    let mut pin_args: Vec<Option<Type>> = arg_types.to_vec();
    if let (Some(ret), Some(result)) = (ret_ty, call_result_ty) {
        pin_params.push(ret);
        pin_args.push(Some(result.clone()));
    }

    let mut dict_args: Vec<IrExpr> = Vec::with_capacity(constraints.len());

    // How many constraints of each class this call takes in total. When a class
    // appears more than once (`decodePairs … where Row e, Row f`), each is forwarded
    // by ORDER rather than by variable, since a forwarding instance method's
    // incoming dicts carry positional sentinels, not the real variables.
    let mut class_total: rustc_hash::FxHashMap<ridge_types::ClassId, usize> =
        rustc_hash::FxHashMap::default();
    for c in &constraints {
        *class_total.entry(c.class).or_insert(0) += 1;
    }

    // Count how many constraints of each class precede the current one in this
    // call's own list — its occurrence index within the class.
    let mut class_seen: rustc_hash::FxHashMap<ridge_types::ClassId, usize> =
        rustc_hash::FxHashMap::default();

    for c in &constraints {
        let class_name = ctx.class_name(c.class).unwrap_or("Unknown").to_owned();

        let occurrence = {
            let seen = class_seen.entry(c.class).or_insert(0);
            let n = *seen;
            *seen += 1;
            n
        };
        let call_needs_multiple = class_total.get(&c.class).copied().unwrap_or(0) > 1;

        // The concrete type the constraint variable was unified to at this call
        // site: walk the scheme's parameter (and return) types in lockstep with
        // the resolved argument (and result) types, find where `c.ty` appears,
        // and read off the matching sub-type. `None` when the variable cannot be
        // located (no type info).
        let constraint_ty =
            constraint_arg_type(&pin_params, &pin_args, c.sole_ty()).map(|ty| deep_peel_alias(&ty));

        let dict_expr = resolve_dict_arg(
            ctx,
            c.class,
            &class_name,
            constraint_ty.as_ref(),
            occurrence,
            call_needs_multiple,
            span,
        );
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

/// Substitute alias parameters with concrete arguments in a type body.
fn subst_alias_params(t: &Type, params: &[ridge_types::TyVid], args: &[Type]) -> Type {
    match t {
        Type::Var(v) => params
            .iter()
            .position(|p| p == v)
            .and_then(|i| args.get(i))
            .cloned()
            .unwrap_or_else(|| t.clone()),
        Type::Con(id, sub_args) => Type::Con(
            *id,
            sub_args
                .iter()
                .map(|a| subst_alias_params(a, params, args))
                .collect(),
        ),
        Type::Tuple(ts) => Type::Tuple(
            ts.iter()
                .map(|a| subst_alias_params(a, params, args))
                .collect(),
        ),
        _ => t.clone(),
    }
}

/// If `ty` is a `Type::Con` whose tycon is declared as a `TyConKind::Alias`,
/// substitute the alias parameters and return the expanded body; otherwise
/// return the type unchanged.
///
/// Used so that a transparent alias such as `Join e f a = Joined (Query e a) f a`
/// dispatches through `Joined`'s instances when building sub-dictionary plans.
///
/// If the outer type carried more arguments than the alias has parameters (e.g. an
/// augmented receiver `Join e f a s s s` padded for `Row s`), the extra arguments
/// are appended to the expanded body's argument list so the augmented spine survives
/// the expansion.
fn expand_tycon_alias_once(ctx: &LowerCtx<'_>, ty: &Type) -> Type {
    let Type::Con(tycon, args) = ty else {
        return ty.clone();
    };
    let Some(decl) = ctx.workspace.and_then(|ws| ws.tycons.get(tycon.0 as usize)) else {
        return ty.clone();
    };
    let TyConKind::Alias { params, body } = &decl.kind else {
        return ty.clone();
    };
    let n_params = params.len();
    let expanded = subst_alias_params(body, params, args);
    // When the outer type had more args than the alias's parameter count, those
    // extra slots are application arguments on the expanded body (the alias was
    // over-applied with augmentation padding). Append them to the expanded Con's
    // arg list so they remain accessible at the expected head positions.
    if args.len() > n_params {
        if let Type::Con(exp_id, mut exp_args) = expanded {
            exp_args.extend_from_slice(&args[n_params..]);
            return Type::Con(exp_id, exp_args);
        }
    }
    expanded
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
    occurrence: usize,
    call_needs_multiple: bool,
    span: Span,
) -> IrExpr {
    // Determine whether the CALLER is itself constrained for this class.
    // If so, use the Forward path (forward the caller's own incoming dict param).
    // This is the correct dispatch for polymorphic call sites:
    //   - `fn announce (x: a) -> Text where Show a = describe x` → forward
    //   - `fn main_static () -> Text = describe Red` → no caller constraint → Static
    //
    // Disambiguating WHICH incoming dict to forward depends on how many of this
    // class the CALL itself takes:
    //   - One (the overwhelmingly common case): the constraint's resolved type
    //     variable selects the matching incoming dict — `fromRow` on a left row
    //     forwards `$dict_Row_e`, on a right row `$dict_Row_f` — falling back to
    //     the first when the variable cannot be matched.
    //   - Several of the same class (e.g. `decodePairs … where Row e, Row f`,
    //     which decodes a tuple of two records): the `occurrence` index — which
    //     same-class constraint of this call we are resolving — picks the matching
    //     incoming dict by ORDER. This is used instead of the variable match
    //     because an instance method's `current_fn_constraints` carry positional
    //     sentinels (`TyVid(i)` per `where` slot, not the real variable), so a
    //     variable match would either miss or, worse, hit the wrong sentinel by
    //     coincidence. By order, a join's `toList` threads `$dict_Row_e` to the
    //     left decode and `$dict_Row_f` to the right rather than the same twice.
    let want_var = match constraint_ty {
        Some(Type::Var(v)) => Some(*v),
        _ => None,
    };
    let caller_constraint = want_var
        .and_then(|v| {
            ctx.current_fn_constraints
                .iter()
                .find(|c| c.class == class && c.sole_ty() == v)
        })
        .or_else(|| {
            if call_needs_multiple {
                // The exact variable did not match a caller constraint — which is
                // the instance-method case, where the incoming dicts carry
                // positional sentinels. Forward the `occurrence`-th same-class dict
                // by order. (Constraint order follows the `where` clause, so an
                // instance's `Row e`, `Row f` line up with the callee's.)
                ctx.current_fn_constraints
                    .iter()
                    .filter(|c| c.class == class)
                    .nth(occurrence)
            } else {
                ctx.current_fn_constraints.iter().find(|c| c.class == class)
            }
        })
        .cloned();

    if let Some(c) = caller_constraint {
        let id = ctx.fresh_id(None);
        return IrExpr::Local {
            id,
            name: format!("$dict_{class_name}_{}", c.sole_ty().0),
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
        // The argument pins only one class parameter, but a multi-parameter
        // instance is keyed by the full head tuple, which a single-type lookup
        // cannot reconstruct. Fall back to the solved Static plan when the class
        // has exactly one instance in scope (the common multi-parameter case).
        if let Some(plan) = single_static_plan_for_class(ctx, class) {
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

/// When a bare class-method reference is lowered inside a function that carries
/// several constraints for the *same* class (e.g. `fromRow` under `Row e, Row
/// f`, which decodes a tuple of two records), the method's instantiated type at
/// this use mentions exactly one of those constraint variables. Return it as a
/// `Type::Var` so [`resolve_dict_arg`] forwards the matching incoming dict rather
/// than always the first. Returns `None` when there is at most one such
/// constraint (the common case, no ambiguity) or the type does not single one
/// out, in which case the first match is used as before.
fn pin_method_dict_var(
    ctx: &LowerCtx<'_>,
    class: ridge_types::ClassId,
    method_ty: &Type,
) -> Option<Type> {
    let candidates: Vec<ridge_types::TyVid> = ctx
        .current_fn_constraints
        .iter()
        .filter(|c| c.class == class)
        .map(ridge_types::Constraint::sole_ty)
        .collect();
    if candidates.len() < 2 {
        return None;
    }
    // Reuse the scheme free-variable walk over a throwaway scheme wrapping the
    // method's instantiated type (no bound variables, so every variable is free).
    let probe = ridge_types::Scheme {
        vars: vec![],
        cap_vars: vec![],
        row_vars: vec![],
        ty: method_ty.clone(),
        constraints: vec![],
    };
    let (free, _) = probe.free_vars();
    let mut hits = candidates.into_iter().filter(|v| free.contains(v));
    let first = hits.next()?;
    if hits.next().is_some() {
        return None; // more than one caller variable appears — ambiguous
    }
    Some(Type::Var(first))
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
            match found {
                None => found = Some(plan),
                // Several call sites that resolve the SAME instance each register
                // their own entry (keyed by their constraint variable); identical
                // plans are not an ambiguity. Only distinct instances are.
                Some(prev) if same_dict_plan(prev, plan) => {}
                Some(_) => return None, // genuinely distinct instances — do not guess
            }
        }
    }
    found.cloned()
}

/// The solved `DictPlan::Static` for `class` over the receiver type `recv_ty`, from
/// this module's resolution table. Used to recover the constraint solver's plan for a
/// receiver-keyed terminal whose context constraint over its predicate's return type
/// (`SqlType n`/`Row s`) the lowering cannot rebuild from the receiver type alone — the
/// solver resolved it against the full head, including the predicate.
///
/// A composite join terminal is keyed by its receiver tycon alone (`Joined`,
/// `LeftJoined`, …), yet the same tycon serves every join depth: `select` over a
/// three-table `Joined (Join …) f` and over a four-table `Joined (Joined …) f` both
/// store a plan under that tycon. The two differ only in their receiver-bound context
/// sub-dictionaries — a deeper source resolves `JoinShape q` to the composite instance,
/// a shallower one to the binary. Keying the lookup on the tycon alone would return an
/// arbitrary one (the resolution table is unordered), threading a composite `JoinShape`
/// dictionary into a binary join (or the reverse) and crashing the decode. So when more
/// than one plan shares the tycon, pick the one whose receiver-bound sub-dictionaries
/// match the type spine the receiver carries; the single-plan case returns that plan
/// unconditionally (the fallback). Returns `None` when no plan shares the tycon.
fn stored_static_plan_for_receiver(
    ctx: &LowerCtx<'_>,
    class: ridge_types::ClassId,
    tycon: TyConId,
    recv_ty: &Type,
) -> Option<ridge_typecheck::DictPlan> {
    use ridge_typecheck::DictPlan;
    let tmod = ctx
        .workspace
        .and_then(|ws| ws.modules.get(ctx.module_id.0 as usize))?;
    // Prefer a strict match (join spine plus the projected record/column the
    // receiver was augmented with); fall back to the first plan whose join spine
    // alone agrees when no strict match exists — an average's `Float` result has
    // no `SqlType Float` plan to match, yet its return dict is inert, so the
    // spine decides. The final `any` fallback keeps the pre-spine behaviour for a
    // receiver that carries no comparable spine at all.
    let mut spine_fallback: Option<&DictPlan> = None;
    let mut any_fallback: Option<&DictPlan> = None;
    for ((cid, _), plan) in &tmod.dict_resolution {
        if *cid != class {
            continue;
        }
        let DictPlan::Static { tycon: t, .. } = plan else {
            continue;
        };
        if *t != tycon {
            continue;
        }
        any_fallback.get_or_insert(plan);
        if dict_plan_matches_receiver(ctx, plan, recv_ty, true) {
            return Some(plan.clone());
        }
        if spine_fallback.is_none() && dict_plan_matches_receiver(ctx, plan, recv_ty, false) {
            spine_fallback = Some(plan);
        }
    }
    spine_fallback.or(any_fallback).cloned()
}

/// Whether a stored dictionary plan's tycon spine agrees with the receiver type the
/// call carries — the discriminator that tells two stored plans for one composite tycon
/// at different join depths apart.
///
/// Walks both in lockstep: a `Static` plan's `tycon` must equal the type's head
/// constructor, then each receiver-bound sub-dictionary (every context whose head
/// position is not the predicate-return sentinel) must match the type argument at
/// that position, recursively. A `Forward` plan or a variable type carries no tycon
/// to compare on, so it is treated as a match — it never discriminates, only the
/// concrete spine does.
///
/// `strict` also compares the determined predicate-return position (`Row s`/`SqlType
/// n`), which the lowering pads into the receiver's trailing slots. This tells two
/// same-shape joins projecting different records (or folding different column types)
/// apart. Non-strict skips it, matching the join spine alone — used for the fallback.
fn dict_plan_matches_receiver(
    ctx: &LowerCtx<'_>,
    plan: &ridge_typecheck::DictPlan,
    ty: &Type,
    strict: bool,
) -> bool {
    use ridge_typecheck::class_env::PREDICATE_RETURN_POS;
    use ridge_typecheck::DictPlan;
    let DictPlan::Static {
        tycon, info, args, ..
    } = plan
    else {
        return true;
    };
    // Expand a transparent tycon alias on the receiver before comparing heads. A
    // join spine mixes representations: the base step `Joinable (Query e a)`
    // yields the alias `Join e f a`, while each composite step yields `Joined`.
    // The stored plan was built against the expanded `Joined (Query e a) f a`
    // (the alias has no instances of its own), so the receiver's `Join` atom must
    // expand to `Joined` here or the spine walk would mismatch one level early and
    // thread a shallower plan into a deeper join.
    let expanded = expand_tycon_alias_once(ctx, &deep_peel_alias(ty));
    let Type::Con(head, targs) = expanded else {
        return true;
    };
    // A plan never matches a receiver of a different head constructor — this is
    // what tells `JoinShape (Query e a)` (the one-leaf base) apart from
    // `JoinShape (Joined …)` (the recursive step) when both are stored for the
    // same nested-join spine. Check it before any structural short-circuit.
    if *tycon != head {
        return false;
    }
    // A non-parametric instance (no head variable positions) has matched on its
    // tycon alone and carries no further type structure to compare — for example
    // a `Row` or `Adapter` sub-dictionary whose head is already pinned. Accept it.
    if info.head_var_positions.is_empty() {
        return true;
    }
    // Where the lowering's projection padding begins: one past the highest
    // receiver-bound head position. A `Projectable`/`Aggregable` receiver is
    // augmented with the determined predicate-return type (`Row s`/`SqlType n`)
    // repeated into its trailing argument slots, so this matcher can compare it
    // even though no receiver-bound position names it. Two joins of the same
    // shape projecting different records share every receiver-bound position and
    // differ only here, so without this check the spine walk picks whichever plan
    // is stored first and threads the wrong row decoder. `None` when the receiver
    // carries no such padding (a non-augmented call), where the predicate return
    // is left unconstrained and the spine alone decides, as before.
    let proj_start = info
        .head_var_positions
        .iter()
        .filter(|&&p| p != PREDICATE_RETURN_POS)
        .map(|&p| p + 1)
        .max();
    for (i, sub) in args.iter().enumerate() {
        let Some(&pos) = info.head_var_positions.get(i) else {
            continue;
        };
        if pos == PREDICATE_RETURN_POS {
            // The determined predicate return (`Row s`/`SqlType n`), padded into the
            // receiver's trailing slots by the lowering. In strict mode a plan whose
            // return dict names a different type than this call records is rejected;
            // non-strict leaves it to the spine. An average's `Float` result has no
            // matching `SqlType` plan but its return dict is inert, so it relies on
            // the non-strict spine fallback in `stored_static_plan_for_receiver`.
            if strict {
                if let Some(proj_ty) = proj_start.and_then(|start| targs.get(start)) {
                    if !dict_plan_matches_receiver(ctx, sub, proj_ty, strict) {
                        return false;
                    }
                }
            }
            continue;
        }
        let Some(arg_ty) = targs.get(pos) else {
            continue;
        };
        if !dict_plan_matches_receiver(ctx, sub, arg_ty, strict) {
            return false;
        }
    }
    true
}

/// Structural equality for two dictionary plans, used to tell "the same instance
/// registered by two call sites" apart from "two distinct instances".
///
/// `DictPlan` is not `PartialEq` (its `InstanceInfo` payload is not), so compare
/// the resolved concrete spine: a `Static` plan is identified by its `tycon` and
/// the structure of its sub-dictionaries; a `Forward` by its class and variable.
fn same_dict_plan(a: &ridge_typecheck::DictPlan, b: &ridge_typecheck::DictPlan) -> bool {
    use ridge_typecheck::DictPlan;
    match (a, b) {
        (
            DictPlan::Static {
                tycon: ta,
                extra_head: ea,
                args: aa,
                ..
            },
            DictPlan::Static {
                tycon: tb,
                extra_head: eb,
                args: ab,
                ..
            },
        ) => {
            ta == tb
                && ea == eb
                && aa.len() == ab.len()
                && aa.iter().zip(ab).all(|(x, y)| same_dict_plan(x, y))
        }
        (DictPlan::Forward(ca), DictPlan::Forward(cb)) => ca.class == cb.class && ca.tys == cb.tys,
        _ => false,
    }
}

/// Resolve a callee to `(class_name, method)` when it binds to a class method.
/// Mirrors the binding lookup in `lower_ident`/`lower_qualified`.
///
/// Accepts both a bare `Ident` callee (`describe Red`) and a module-qualified
/// callee (`Sql.toSql n`): a qualified class-method call resolves to a
/// `StdlibSymbol`/`ClassMethod` binding stamped on the `QualifiedName` node, so
/// it routes through the same dictionary dispatch as the bare form rather than
/// being lowered to a plain stdlib symbol (which would miss the bridge map).
pub(crate) fn classmethod_binding(ctx: &LowerCtx<'_>, callee: &Expr) -> Option<(String, String)> {
    let (span, kind) = match callee {
        Expr::Ident(ident) => (ident.span, NodeKind::Ident),
        Expr::Qualified(qname) => (qname.span, NodeKind::QualifiedName),
        _ => return None,
    };
    let node_id = ctx.node_id_map.as_ref().and_then(|m| m.get(span, kind))?;
    let binding = ctx
        .binding_map
        .and_then(|bm| bm.get(node_id.0 as usize).and_then(Option::as_ref))?;
    match binding {
        Binding::ClassMethod { class_name, method } => Some((class_name.clone(), method.clone())),
        // A stdlib class method (e.g. `toSql`/`fromSql` from `std.sql`) is
        // resolved by the resolver as `StdlibSymbol` because it appears in the
        // stdlib module's export manifest. However, the class table registers
        // the method under its class, so we can recover the class-method shape
        // here and route through the dictionary dispatch path.
        //
        // Module-scoping guard: a name can be BOTH a class method AND a plain
        // `pub fn` in a different module — `toText` is the `ToText` method but
        // also `std.int`'s own `pub fn toText`, and `filter`/`map` are class-ish
        // names that exist as plain list/map verbs. A qualified `Int.toText` or
        // a by-name-imported `toText` must dispatch through the dictionary ONLY
        // when the symbol's module is the class's home module; otherwise it is
        // that module's own function and lowers as a plain stdlib symbol. This
        // matters once qualified callees reach this helper (a bare `Ident` only
        // hit it for genuinely class-method imports before).
        Binding::StdlibSymbol { module, name } => {
            let ct = ctx
                .class_table
                .or_else(|| ctx.workspace.map(|ws| &ws.class_table))?;
            let class_name = ct.class_name_for_method(name)?;
            if stdlib_class_home_module(class_name) != Some(stdlib_module_name(*module).as_str()) {
                return None;
            }
            Some((class_name.to_owned(), name.clone()))
        }
        _ => None,
    }
}

/// Align an argument's inferred type against a method parameter's AST annotation
/// and return the sub-type sitting where a class type variable appears in the
/// annotation.
///
/// For `rowColumns (witness: Option a)` called with an argument of inferred type
/// `Option e`, the annotation `Option a` carries the class variable `a` inside
/// the `Option` constructor; walking both in lockstep yields `e`. Used to pin a
/// class method whose class variable is nested in a parameter rather than the
/// bare parameter itself. Returns `None` when the shapes do not line up or the
/// annotation holds no class variable.
fn align_classvar_in_arg(
    param_ast: &ridge_ast::Type,
    arg_ty: &Type,
    class_vars: &[String],
) -> Option<Type> {
    use ridge_ast::Type as A;
    match param_ast {
        // The annotation IS a class variable: the aligned concrete type is the
        // argument type at this position.
        A::Var { name, .. } if class_vars.contains(&name.text) => Some(arg_ty.clone()),
        // `Option a`, `Map k v`, … — recurse positionally into the constructor's
        // arguments. The inferred side is a `Con` (peel any transparent alias).
        A::App { args, .. } => {
            if let Type::Con(_, ty_args) = deep_peel_alias(arg_ty) {
                args.iter()
                    .zip(ty_args.iter())
                    .find_map(|(a, t)| align_classvar_in_arg(a, t, class_vars))
            } else {
                None
            }
        }
        // `[a]` — the inferred side is the list `Con` with one element argument.
        A::List { elem, .. } => {
            if let Type::Con(_, ty_args) = deep_peel_alias(arg_ty) {
                ty_args
                    .first()
                    .and_then(|t| align_classvar_in_arg(elem, t, class_vars))
            } else {
                None
            }
        }
        A::Tuple { elems, .. } => {
            if let Type::Tuple(ty_elems) = arg_ty {
                elems
                    .iter()
                    .zip(ty_elems.iter())
                    .find_map(|(a, t)| align_classvar_in_arg(a, t, class_vars))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The concrete type pinning the class variable at a bare class-method call.
///
/// Locates the method parameter that carries the class type variable — either as
/// the bare parameter (`toRow (x: a)`) or nested inside a constructor
/// (`rowColumns (witness: Option a)`, aligned via [`align_classvar_in_arg`]) —
/// and reads the resolved type of the argument in that position. Returns `None`
/// for a return-only class variable (`decode (j) -> a`) or when no argument
/// carries the variable, leaving those to the generic lowering path.
fn classmethod_pin_type(
    ctx: &LowerCtx<'_>,
    class: ridge_types::ClassId,
    method: &str,
    args: &[Expr],
) -> Option<Type> {
    let info = ctx
        .class_table
        .or_else(|| ctx.workspace.map(|ws| &ws.class_table))?
        .get(class)?;
    let sig = info.method_sigs.iter().find(|m| m.name == method)?;

    // For source-declared class methods, `ast_param_types` carries the AST
    // annotations; find the parameter that carries the class type variable.
    if !sig.ast_param_types.is_empty() {
        // (1) A parameter that IS the class variable (`toRow (x: a)`): pin from
        //     its full argument type.
        if let Some(pos) = sig.ast_param_types.iter().position(|t| {
            matches!(t, ridge_ast::Type::Var { name, .. }
                if sig.class_ty_vars.contains(&name.text))
        }) {
            let arg = args.get(pos)?;
            let node_id = ctx
                .node_id_map
                .as_ref()
                .and_then(|m| m.get(arg.span(), NodeKind::Expr))?;
            return ctx.node_type(node_id).cloned().map(|t| deep_peel_alias(&t));
        }
        // (2) A parameter whose type CONTAINS the class variable nested in a
        //     constructor (`rowColumns (witness: Option a)`): align the argument's
        //     inferred type against the annotation and read off the sub-type at the
        //     class variable's position. This pins the forward variable the call
        //     resolves — `Option e` against `Option a` yields `e` — so two such
        //     calls on different entities (a binary outer join's left and right
        //     `rowColumns`) forward their own dictionary instead of both collapsing
        //     onto the first same-class one. The direct case above never matches
        //     here, so this only runs when the variable is genuinely nested.
        for (param_ast, arg) in sig.ast_param_types.iter().zip(args.iter()) {
            let Some(node_id) = ctx
                .node_id_map
                .as_ref()
                .and_then(|m| m.get(arg.span(), NodeKind::Expr))
            else {
                continue;
            };
            let Some(arg_ty) = ctx.node_type(node_id).cloned() else {
                continue;
            };
            if let Some(found) = align_classvar_in_arg(param_ast, &arg_ty, &sig.class_ty_vars) {
                return Some(deep_peel_alias(&found));
            }
        }
        return None;
    }

    // For stdlib-registered methods (no AST param types), try each argument
    // position. Accept the first argument whose resolved type has a registered
    // instance for this class — this pins `toSql (x: a)` from the `x` argument
    // without requiring AST annotation data.
    let env = ctx.instance_env?;
    for arg in args {
        let Some(node_id) = ctx
            .node_id_map
            .as_ref()
            .and_then(|m| m.get(arg.span(), NodeKind::Expr))
        else {
            continue;
        };
        let Some(arg_ty) = ctx.node_type(node_id).cloned().map(|t| deep_peel_alias(&t)) else {
            continue;
        };
        // Check if this argument type pins an instance for `class`. A single-
        // parameter instance is keyed by the bare head tycon; a multi-parameter
        // instance with a functional dependency from the first position
        // (`Refinable q p | q -> p`) is keyed by the full head tuple, so also
        // accept an argument whose head is the first atom of some instance head —
        // the receiver of `Repo.filter` pins `Query`/`Join`/`LeftJoin` this way.
        //
        // If no direct instance is found, try expanding a transparent tycon alias
        // (e.g. `Join e f a = Joined (Query e a) f a`) and check the expansion.
        // When found, return the expanded type so callers build the right dict.
        if let Type::Con(tycon, _) = &arg_ty {
            let single = env.get((class, *tycon)).is_some();
            let multi = env
                .instances
                .keys()
                .any(|(cid, head)| *cid == class && head.first() == Some(tycon));
            if single || multi {
                return Some(arg_ty);
            }
            // Try alias expansion.
            let expanded = expand_tycon_alias_once(ctx, &arg_ty);
            if let Type::Con(exp_tycon, _) = &expanded {
                if exp_tycon != tycon {
                    let exp_single = env.get((class, *exp_tycon)).is_some();
                    let exp_multi = env
                        .instances
                        .keys()
                        .any(|(cid, head)| *cid == class && head.first() == Some(exp_tycon));
                    if exp_single || exp_multi {
                        return Some(expanded);
                    }
                }
            }
        }
    }

    None
}

/// Lower a bare class-method call (`describe Red`) by pinning the class
/// constraint from the argument type. Returns `None` when the callee is not a
/// class method, or when no argument pins the class variable (return-polymorphic
/// methods like `decode`) AND the class is user-defined, leaving those to the
/// generic call path.
///
/// For stdlib-defined classes (whose `ast_param_types` is empty because they
/// are registered in Rust rather than from source AST), the argument pin is
/// tried first; when unavailable (e.g. for `fromSql (v: SqlValue) -> Result a Error`
/// where the class variable is in the return type), the call expression's own
/// node type is examined to extract the concrete class type argument. This lets
/// both `toSql n` and `fromSql v` resolve their dictionaries at the right
/// instance without falling back to the unreliable sole-static-plan heuristic.
pub(crate) fn try_lower_classmethod_call(
    ctx: &mut LowerCtx<'_>,
    callee: &Expr,
    args: &[Expr],
    id: ridge_ir::IrNodeId,
    span: Span,
) -> Option<IrExpr> {
    let (class_name, method) = classmethod_binding(ctx, callee)?;
    let cid = ctx
        .class_table
        .or_else(|| ctx.workspace.map(|ws| &ws.class_table))
        .and_then(|ct| ct.id_by_name(&class_name))?;

    // Try to pin the constraint type from an argument. For user-defined classes
    // the source AST carries the parameter annotations; `classmethod_pin_type`
    // scans them and locates the class type variable's position. For
    // stdlib-registered classes the AST is absent, so the function returns None
    // when the class variable cannot be found by AST scan.
    let mut pin_ty = classmethod_pin_type(ctx, cid, &method, args);

    let is_stdlib_class = stdlib_class_home_module(&class_name).is_some();

    // For user-defined classes where no argument pins the constraint (return-
    // polymorphic methods like `decode`), defer to the generic call path and
    // its own dict-arg machinery.
    if pin_ty.is_none() && !is_stdlib_class {
        return None;
    }

    // For stdlib-registered class methods where the argument does not directly
    // carry the class variable (e.g. `fromSql (v: SqlValue) -> Result a Error`),
    // try to derive the concrete class type from the call expression's inferred
    // return type. Walk the return type looking for a known registered instance.
    if pin_ty.is_none() && is_stdlib_class {
        pin_ty = stdlib_classmethod_pin_from_return(ctx, cid, span);
    }

    let ir_args: Vec<IrExpr> = args.iter().map(|a| lower_expr(ctx, a)).collect();
    // `Projectable`'s `where Adapter a, Row s` context puts `s` in the projection,
    // not the receiver `q`, so a dictionary re-derived from the receiver type alone
    // cannot resolve `Row s`. Augment the receiver with the projected element
    // (recovered from the call's result type) so that sub-dictionary resolves to a
    // concrete instance rather than an unbound forward. `Aggregable`'s `where
    // Adapter a, SqlType n` is the same shape (`n` = the folded column's type, the
    // accessor's return), so it augments too: `sumOf`/`minOf`/`maxOf` recover `n`
    // from their `Option n` result, and `avgOf` recovers a `Float` it never reads
    // (its `SqlType n` dict is inert). Other class methods — the `Adapter` seam,
    // `Refinable.filter`, `Orderable.orderBy` — have no such context and keep the
    // plain receiver pin.
    let dict_ty = if class_name == "Projectable" || class_name == "Aggregable" {
        augment_receiver_with_projection_atoms(ctx, pin_ty.as_ref(), span)
    } else {
        pin_ty
    };
    // A class-method call resolves one dictionary for its own class; there is no
    // sibling same-class constraint to order against (occurrence 0, single).
    let dict_expr = resolve_dict_arg(ctx, cid, &class_name, dict_ty.as_ref(), 0, false, span);
    let field_id = ctx.fresh_id(None);
    let field = IrExpr::Field {
        id: field_id,
        base: Box::new(dict_expr),
        field: method,
        span,
    };
    Some(IrExpr::Call {
        id,
        callee: Box::new(field),
        args: ir_args,
        span,
    })
}

/// Derive a pin type for a stdlib class method whose class variable is in the
/// return type rather than the argument list (e.g. `fromSql (v: SqlValue) -> Result a Error`).
///
/// Reads the inferred return type of the call expression at `call_span`, then
/// searches the registered instances for this class to find one whose tycon
/// appears in that return type. Returns `Some(concrete_type)` when exactly one
/// instance is a candidate; `None` when the return type is unavailable or no
/// registered instance matches.
fn stdlib_classmethod_pin_from_return(
    ctx: &LowerCtx<'_>,
    class: ridge_types::ClassId,
    call_span: Span,
) -> Option<Type> {
    // Look up the inferred type of the call expression itself.
    let call_type = ctx
        .node_id_map
        .as_ref()
        .and_then(|m| m.get(call_span, NodeKind::Expr))
        .and_then(|nid| ctx.node_type(nid).cloned())
        .map(|t| deep_peel_alias(&t))?;

    // Walk the registered instances for this class. Each registered instance
    // is keyed by (class, TyConId). We want the TyConId whose corresponding
    // base type appears somewhere inside the call's return type.
    let env = ctx.instance_env?;
    let mut candidate: Option<TyConId> = None;
    for (cid, head) in env.instances.keys() {
        if *cid != class {
            continue;
        }
        // Check whether a bare nullary application of any of this instance head's
        // constructors occurs anywhere in the call's return type.
        for &tycon in head {
            if type_contains_tycon(&call_type, tycon) {
                if candidate.is_some() {
                    // Two candidates — ambiguous, give up.
                    return None;
                }
                candidate = Some(tycon);
            }
        }
    }
    if let Some(tycon) = candidate {
        return Some(Type::Con(tycon, vec![]));
    }

    // No concrete instance appears in the return type — the call is in a
    // polymorphic context (e.g. `fromRow` inside `decodePairs … where Row e, Row
    // f`, whose result is still `Result e Error`). When the enclosing fn carries
    // several constraints for this class, the return type mentions exactly one of
    // their variables; pin it so `resolve_dict_arg` forwards the matching incoming
    // dict rather than defaulting to the first. With one constraint there is no
    // ambiguity and this stays `None` (the single dict is forwarded regardless).
    pin_method_dict_var(ctx, class, &call_type)
}

/// Return `true` when a bare `Type::Con(needle, [])` (nullary constructor)
/// occurs anywhere in the type tree of `haystack`.
fn type_contains_tycon(haystack: &Type, needle: TyConId) -> bool {
    match haystack {
        Type::Con(tycon, args) => {
            *tycon == needle && args.is_empty()
                || args.iter().any(|a| type_contains_tycon(a, needle))
        }
        Type::Tuple(elems) => elems.iter().any(|e| type_contains_tycon(e, needle)),
        Type::Fn { params, ret, .. } => {
            params.iter().any(|p| type_contains_tycon(p, needle))
                || type_contains_tycon(ret, needle)
        }
        Type::Alias { body, .. } => type_contains_tycon(body, needle),
        _ => false,
    }
}

/// Augment a receiver type so a multi-parameter class method's context
/// constraints whose variable lives in the projection — `Projectable q p |
/// q -> p` is `… where Adapter a, Row s`, with `s` the projection's return, not
/// part of the receiver `q` — resolve against a concrete instance instead of an
/// unbound forward.
///
/// The projected element `s` cannot be read from the quoted lambda argument (a
/// quote carries no lowering-time type), so it is recovered from the call's own
/// result type (`Result (List s)` / `Result (Option s)` → `s`). The receiver's
/// arguments are then padded with `s` up to the projection's return position in
/// the flattened head (3 for a one-entity query, 5 for a two-entity join), where
/// `resolve_ctx_sub_dicts` reads the `Row s` position. Only that position and the
/// receiver-resident `Adapter a` position are read, so the padding is inert
/// everywhere else. A method with no recoverable `s` (a receiver-returning verb
/// like `filter`) is left unchanged.
fn augment_receiver_with_projection_atoms(
    ctx: &LowerCtx<'_>,
    pin_ty: Option<&Type>,
    call_span: Span,
) -> Option<Type> {
    let Some(Type::Con(tycon, base)) = pin_ty.map(deep_peel_alias) else {
        return pin_ty.cloned();
    };
    let Some(s) = projected_elem_type(ctx, call_span) else {
        return Some(Type::Con(tycon, base));
    };
    // The deepest projection-return position across the query/inner-join/left-join
    // instances is 5 (a two-entity join: `[e, f, a, e, f, s]`); padding to length
    // 6 covers it and the shallower query case (`[e, a, e, s]`, position 3).
    let mut full = base;
    while full.len() < 6 {
        full.push(s.clone());
    }
    Some(Type::Con(tycon, full))
}

/// The projected element `s` of a projection call whose result is
/// `Result (List s) Error` or `Result (Option s) Error` — read from the call
/// expression's own inferred type. `None` when the call has no recorded type or
/// its result is not a `Result` wrapping a one-argument container.
fn projected_elem_type(ctx: &LowerCtx<'_>, call_span: Span) -> Option<Type> {
    let nid = ctx
        .node_id_map
        .as_ref()
        .and_then(|m| m.get(call_span, NodeKind::Expr))?;
    let call_ty = ctx.node_type(nid).cloned().map(|t| deep_peel_alias(&t))?;
    let Type::Con(_result, rargs) = call_ty else {
        return None;
    };
    // `Result <ok> <err>` — the projected container is the `ok` argument.
    let container = deep_peel_alias(rargs.first()?);
    let Type::Con(_list_or_option, inner) = container else {
        return None;
    };
    inner.into_iter().next()
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

    // Expand a transparent tycon alias (e.g. `Join e f a = Joined (Query e a) f a`)
    // before instance lookup so alias tycons dispatch through their expansions.
    let peeled = deep_peel_alias(ty);
    let expanded = expand_tycon_alias_once(ctx, &peeled);
    // Use the expanded type only when the alias body names a different tycon.
    let effective_ty = match (&peeled, &expanded) {
        (Type::Con(orig, _), Type::Con(new_id, _)) if orig != new_id => expanded,
        _ => peeled,
    };

    match effective_ty {
        Type::Con(tycon, args) => {
            let env = ctx.instance_env?;
            // An instance with a context constraint over its predicate's return type
            // (a composite terminal's `SqlType n`/`Row s`, marked `PREDICATE_RETURN_POS`)
            // cannot be rebuilt from the receiver type alone — the variable lives in the
            // predicate, not the receiver's arguments. The constraint solver already
            // resolved it against the full head, so reuse that stored plan, selected by
            // the receiver type (its tycon plus the spine its sub-dictionaries must
            // match, so two depths sharing the tycon do not cross their plans).
            if let Some(info) = env.get((class, tycon)) {
                if info
                    .head_var_positions
                    .contains(&ridge_typecheck::class_env::PREDICATE_RETURN_POS)
                {
                    if let Some(stored) = stored_static_plan_for_receiver(ctx, class, tycon, ty) {
                        return Some(stored);
                    }
                }
            }
            let Some(info) = env.get((class, tycon)) else {
                // A multi-parameter instance with a functional dependency from the
                // first position (`Refinable q p | q -> p`) is keyed by the whole
                // head tuple (`[Query, Fn1]`), so the single-tycon lookup above
                // misses. The receiver pins the first head (`Query`) and the
                // dependency determines the rest, so the instance is the unique one
                // whose head starts with `tycon`. Build its plan with the trailing
                // head constructors as `extra_head` so the dict const name matches
                // the definition (`$inst_Refinable_Query_Fn1`).
                let mut hits = env
                    .instances
                    .iter()
                    .filter(|((cid, head), _)| *cid == class && head.first() == Some(&tycon));
                let ((_, head), inst) = hits.next()?;
                if hits.next().is_some() {
                    // Two instances share the first head with no dependency to pick
                    // between them — do not guess.
                    return None;
                }
                let extra_head: smallvec::SmallVec<[TyConId; 1]> =
                    head.iter().skip(1).copied().collect();
                // Resolve the instance's context constraints (`… where Adapter a`)
                // from the receiver atom's type arguments — the flattened head
                // positions index this first atom's args, which is where a
                // receiver-bound context variable lives.
                let sub_dicts = resolve_ctx_sub_dicts(ctx, inst, &args);
                return Some(DictPlan::Static {
                    class,
                    info: Box::new(inst.clone()),
                    tycon,
                    extra_head,
                    args: sub_dicts,
                });
            };
            // Resolve one sub-dictionary per context constraint, reading the
            // concrete type argument at the constraint's recorded head position.
            let sub_dicts = resolve_ctx_sub_dicts(ctx, info, &args);
            Some(DictPlan::Static {
                class,
                info: Box::new(info.clone()),
                tycon,
                extra_head: smallvec::SmallVec::default(),
                args: sub_dicts,
            })
        }
        // Neither a bare type variable nor any non-constructor shape resolves to
        // a concrete instance here.
        _ => None,
    }
}

/// Resolve one sub-dictionary plan per context constraint of `info`, reading the
/// concrete type argument at each constraint's recorded head position from
/// `head_args`. An element that resolves to a variable (or has no instance)
/// forwards a dictionary parameter; a truly unsatisfiable element is reported as
/// T030 by the solver before lowering runs. Shared by the single-parameter and
/// multi-parameter (`extra_head`) branches of [`build_dict_plan_from_type`].
fn resolve_ctx_sub_dicts(
    ctx: &LowerCtx<'_>,
    info: &ridge_typecheck::InstanceInfo,
    head_args: &[Type],
) -> Vec<ridge_typecheck::DictPlan> {
    use ridge_typecheck::DictPlan;
    let mut sub_dicts: Vec<DictPlan> = Vec::with_capacity(info.ctx_constraints.len());
    for (ctx_c, &pos) in info
        .ctx_constraints
        .iter()
        .zip(info.head_var_positions.iter())
    {
        let elem_ty = head_args.get(pos).cloned().unwrap_or(Type::Error);
        let sub = build_dict_plan_from_type(ctx, ctx_c.class, &elem_ty).unwrap_or_else(|| {
            DictPlan::Forward(ridge_types::Constraint::single(
                ctx_c.class,
                forward_var_of(&elem_ty),
            ))
        });
        sub_dicts.push(sub);
    }
    sub_dicts
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

/// Home BEAM module for a stdlib-defined typeclass whose instance dictionaries
/// are compiled into that module and referenced cross-module from user code.
///
/// Returns `Some(module)` for stdlib classes whose `$inst_` constants must be
/// fetched via [`SymbolRef::Stdlib`]; returns `None` for user-defined classes
/// (which keep the [`SymbolRef::Local`] path). Prelude `Encode`/`Decode`
/// instances are handled by the earlier `is_prelude_codec_instance` branch and
/// never reach this function.
pub(crate) fn stdlib_class_home_module(class_name: &str) -> Option<&'static str> {
    match class_name {
        "SqlType" | "Row" => Some("std.sql"),
        "Adapter" => Some("std.data"),
        "HasSchema" => Some("std.schema"),
        "Refinable" | "Projectable" | "Orderable" | "Aggregable" | "Fetchable" | "Pageable"
        | "Countable" | "Every" | "Groupable" | "Summarizable" | "Combinable" | "Joinable"
        | "JoinShape" | "LeftJoinable" | "RightJoinable" | "FullJoinable" => Some("std.repo"),
        _ => None,
    }
}

/// The display name of a class id, read from the workspace class table. Used to
/// form `$inst_{ClassName}_…` from a plan's own class.
fn class_name_of(ctx: &LowerCtx<'_>, class: ridge_types::ClassId) -> Option<String> {
    let ct = ctx
        .class_table
        .or_else(|| ctx.workspace.map(|ws| &ws.class_table))?;
    ct.get(class).map(|info| info.name.clone())
}

/// Convert a resolved [`DictPlan`] to the `IrExpr` that threads the dictionary.
///
/// `class` is the [`ClassId`] the dictionary satisfies — needed to recognise
/// the prelude `Encode`/`Decode` instances, whose dictionaries are synthesised
/// inline (see [`crate::prelude_dict`]) because they have no module-level
/// `$inst_` constant.
pub(crate) fn dict_plan_to_expr(
    ctx: &mut LowerCtx<'_>,
    _class: ridge_types::ClassId,
    plan: ridge_typecheck::DictPlan,
    class_name: &str,
    span: Span,
) -> IrExpr {
    use ridge_typecheck::DictPlan;
    // Each plan is self-describing: a `Static` carries its instance's class, a
    // `Forward` its constraint's class. Use the plan's own class for the dict
    // constant name so a heterogeneous context sub-dictionary (the `Adapter a`
    // dict inside a `Projectable` instance, say) is named against its own class
    // rather than the enclosing instance's. Falls back to the caller's class.
    let class = match &plan {
        DictPlan::Static { class, .. } => *class,
        DictPlan::Forward(c) => c.class,
    };
    let derived_name = class_name_of(ctx, class);
    let class_name: &str = derived_name.as_deref().unwrap_or(class_name);
    match plan {
        DictPlan::Static {
            info,
            tycon,
            extra_head,
            args,
            ..
        } => {
            // Recursively lower the sub-dictionaries first. Each sub-dict re-derives
            // its own class from its plan, so a context constraint of a different
            // class than the parent (`Adapter`/`Row` inside `Projectable`) is named
            // and located correctly.
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

            // An auto-promoted instance (a bare `pub fn toText`, lifted by the
            // collect pass) emits no `$inst_` constant — its method IS the public
            // module function. A polymorphic `where ToText a` call still needs a
            // dictionary value, so synthesise it inline, closing over that
            // function. Without this the `$inst_…` reference below would dangle
            // (E001: local symbol not found in the fn-arity table).
            if info.origin == ridge_typecheck::InstanceOrigin::AutoPromoted {
                if let Some(dict) = crate::prelude_dict::synth_auto_promoted_dict(ctx, &info, span)
                {
                    return dict;
                }
            }

            // A user-defined instance: reference its module-level `$inst_`
            // constant. For a hand-written *parametric* instance the constant is
            // a function of the element dict(s), so apply it to `sub_dicts`
            // (dict-of-dicts). A non-parametric instance has no sub-dicts and the
            // bare symbol is the dictionary map.
            let decl = ctx.workspace.and_then(|ws| ws.tycons.get(tycon.0 as usize));
            let mut type_name =
                decl.map_or_else(|| format!("TyCon{}", tycon.0), |decl| decl.name.clone());
            // Multi-parameter instance: append the remaining head constructors so
            // the reference matches the generated `$inst_{Class}_{T0}_{T1}…` const.
            for extra in extra_head {
                let extra_decl = ctx.workspace.and_then(|ws| ws.tycons.get(extra.0 as usize));
                let extra_name = extra_decl
                    .map_or_else(|| format!("TyCon{}", extra.0), |decl| decl.name.clone());
                type_name.push('_');
                type_name.push_str(&extra_name);
            }
            let dict_const_name = format!("$inst_{class_name}_{type_name}");
            let id = ctx.fresh_id(None);

            // The dictionary const lives in whichever module owns the instance:
            // - a stdlib class's BUILTIN base types → the class's home module
            //   (cross-module via `SymbolRef::Stdlib` + the FFI bridge);
            // - a user type defined in ANOTHER module → that producer module
            //   (cross-module via `SymbolRef::External`, invoked as a call);
            // - a type defined in the current module → a local reference.
            let tycon_is_builtin = decl.is_some_and(|d| d.def_module_raw.is_none());
            let producer = decl.and_then(|d| d.def_module_raw);
            let is_cross_module = producer.is_some_and(|p| p != ctx.module_id.0);
            // The `$inst_…` constant is generated in the module that DECLARES the
            // instance — which is not always the module that defines the head
            // type. A user class with an instance over a builtin type
            // (`instance Tag Int`) lives in the instance's module, while the
            // builtin head carries no `def_module_raw`, so the head-type module
            // alone would mislocate the dict to the use site. Prefer the
            // instance's own declaring module; fall back to the head type's module
            // for instances co-located with their type.
            let inst_module = info.def_module;
            let inst_is_cross = inst_module.is_some_and(|m| m != ctx.module_id.0);
            let sym = if let Some(home) =
                stdlib_class_home_module(class_name).filter(|_| tycon_is_builtin)
            {
                SymbolRef::Stdlib {
                    module: home.to_owned(),
                    name: dict_const_name,
                }
            } else if let Some(m) = inst_module.filter(|_| inst_is_cross) {
                SymbolRef::External {
                    module: ridge_resolve::ModuleId(m),
                    name: dict_const_name,
                }
            } else if let Some(p) = producer.filter(|_| is_cross_module) {
                SymbolRef::External {
                    module: ridge_resolve::ModuleId(p),
                    name: dict_const_name,
                }
            } else {
                SymbolRef::Local {
                    name: dict_const_name,
                    module: ctx.module_id,
                }
            };
            let dict_symbol = IrExpr::Symbol { id, sym, span };
            // A cross-module `External` reference is a 0-arity producer-module fn
            // that must be invoked as a call (its value form is not lowered); a
            // same-module const is usable directly as a value. Parametric
            // instances always apply their sub-dictionaries.
            let emit_as_call = inst_is_cross || is_cross_module;
            if sub_dicts.is_empty() && !emit_as_call {
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
                name: format!("$dict_{class_name}_{}", c.sole_ty().0),
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
/// The inferred type of a lambda's parameter `idx`, read from the enclosing
/// lambda's `Type::Fn`.
///
/// `node_types` is keyed by `Expr` positions only — param ident spans carry no
/// type entry — so the correct source is the enclosing lambda's `Fn` type at
/// `lambda_span`. Falls back to `Type::Error` when no type is available.
fn lambda_param_ty(ctx: &LowerCtx<'_>, lambda_span: Span, idx: usize) -> Type {
    ctx.node_id_map
        .as_ref()
        .and_then(|m| m.get(lambda_span, NodeKind::Expr))
        .and_then(|nid| ctx.node_type(nid).cloned())
        .and_then(|fn_ty| {
            if let Type::Fn { params, .. } = fn_ty {
                params.into_iter().nth(idx)
            } else {
                None
            }
        })
        .unwrap_or(Type::Error)
}

/// Lower a lambda's parameters into direct IR binders plus the destructuring
/// entries the caller wraps around the body.
///
/// A plain `Var`/`_` param lowers to a direct [`IrParam`]. Any other pattern
/// (tuple, constructor, record, list, as-, literal, nested) lowers to a fresh
/// synthetic binder, and its `(name, pattern, span)` is returned so the caller
/// wraps the body in a `match` via [`wrap_pattern_params`] — the same mechanism
/// used for destructuring params on named `fn` declarations. Without that
/// wrapper a non-trivial param bound its variables during type-checking but
/// dropped them here, so the backend rejected the body with `unbound variable`.
///
/// Tuple params keep the historical `__tuple_param` synthetic-name prefix;
/// every other shape uses the general `__param` prefix (matching
/// [`synth_destructure_param`]).
fn lower_lambda_params<'a>(
    ctx: &mut LowerCtx<'_>,
    lambda_span: Span,
    params: &'a [LambdaParam],
) -> (Vec<IrParam>, Vec<(String, &'a Pattern, Span)>) {
    let mut ir_params: Vec<IrParam> = Vec::with_capacity(params.len());
    let mut pattern_entries: Vec<(String, &'a Pattern, Span)> = Vec::new();

    for (idx, p) in params.iter().enumerate() {
        // The pattern under this param plus its optional annotation.
        let (pat, ann_ty): (&Pattern, Option<&ridge_ast::Type>) = match p {
            LambdaParam::Pattern(pat) => (pat, None),
            LambdaParam::Annotated { pat, ty, .. } => (pat, Some(ty)),
        };
        if matches!(pat, Pattern::Var { .. } | Pattern::Wildcard { .. }) {
            // Plain binder — no destructuring wrapper needed.
            ir_params.push(lambda_param_to_ir_param(ctx, lambda_span, idx, p));
            continue;
        }
        // Destructuring param: a fresh binder feeds a wrapping `match`.
        let prefix = if matches!(pat, Pattern::Tuple { .. }) {
            "__tuple_param"
        } else {
            "__param"
        };
        let synth_name = ctx.fresh_local(prefix);
        // Param type: the annotation when present, else the inferred Fn type.
        let ty = if let Some(ann) = ann_ty {
            lower_ast_type(ctx, ann)
        } else {
            lambda_param_ty(ctx, lambda_span, idx)
        };
        ir_params.push(IrParam {
            name: synth_name.clone(),
            ty,
            span: pat.span(),
        });
        pattern_entries.push((synth_name, pat, pat.span()));
    }

    (ir_params, pattern_entries)
}

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
            let ty = lambda_param_ty(ctx, lambda_span, param_idx);
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

// ── Destructuring (pattern) param helpers (L9) ───────────────────────────────

/// Build the synthetic [`IrParam`] for a destructuring `PatternAnnotated` param.
///
/// A parameter that destructures (`(Point { x, y }: Point)`) lowers to a fresh
/// `__param_N` binder; the pattern itself is matched in a wrapping `match`
/// around the body (see [`wrap_pattern_params`]). Returns the param plus its
/// fresh name so the caller can build that wrapper.
pub(crate) fn synth_destructure_param(
    ctx: &mut LowerCtx<'_>,
    ty: &ridge_ast::Type,
    span: Span,
) -> (IrParam, String) {
    let name = ctx.fresh_local("__param");
    let ir = IrParam {
        name: name.clone(),
        ty: lower_ast_type(ctx, ty),
        span,
    };
    (ir, name)
}

/// Wrap `body` so each destructuring param binds its pattern via a `match`.
///
/// `entries` pairs each synthetic param name (from [`synth_destructure_param`])
/// with its source pattern and span. The wrappers nest outside-in, so the first
/// param's match is outermost. Each is a single irrefutable arm — refutability
/// was rejected in typecheck, so the match never fails at runtime.
pub(crate) fn wrap_pattern_params(
    ctx: &mut LowerCtx<'_>,
    body: IrExpr,
    entries: Vec<(String, &Pattern, Span)>,
) -> IrExpr {
    entries
        .into_iter()
        .rev()
        .fold(body, |inner, (name, pat, span)| {
            let ir_pat = crate::match_lower::lower_pattern_full(ctx, pat);
            let arm = ridge_ir::IrArm {
                pat: ir_pat,
                when: None,
                body: inner,
                span,
            };
            let scrutinee_id = ctx.fresh_id(None);
            let match_id = ctx.fresh_id(None);
            IrExpr::Match {
                id: match_id,
                scrutinee: Box::new(IrExpr::Local {
                    id: scrutinee_id,
                    name,
                    span,
                }),
                arms: vec![arm],
                span,
            }
        })
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
    // A decimal literal has no native runtime form, so it lowers to a call that
    // rebuilds the exact value from its digits. The lexer has already validated
    // the text, so `parseStrict` never fails here; the `m` suffix and any digit
    // separators are dropped before the runtime parses the number.
    if let Literal::Decimal { raw, .. } = lit {
        let text = raw.trim_end_matches(['m', 'M']).replace('_', "");
        let callee = IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.decimal".to_string(),
                name: "parseStrict".to_string(),
            },
            span,
        };
        let arg = IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Text(text),
            span,
        };
        return IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(callee),
            args: vec![arg],
            span,
        };
    }
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
        // Handled by the early return above.
        Literal::Decimal { .. } => unreachable!("decimal literal lowered above"),
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
            let sym = imported_symbol_ref(ctx, *module, ident.text.clone());
            IrExpr::Symbol { id, sym, span }
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
            ..
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
                //
                // When the enclosing fn carries several constraints for this class,
                // the method's instantiated type at this use — stamped under
                // `NodeKind::Expr` for the ident span — singles out which constraint
                // variable applies, so the right incoming dict is forwarded instead
                // of always the first.
                let pin = ctx
                    .node_id_map
                    .as_ref()
                    .and_then(|m| m.get(span, NodeKind::Expr))
                    .and_then(|nid| ctx.node_type(nid).cloned())
                    .and_then(|t| pin_method_dict_var(ctx, cid, &t));
                resolve_dict_arg(ctx, cid, class_name, pin.as_ref(), 0, false, span)
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
            let sym = imported_symbol_ref(ctx, *module, last_name);
            IrExpr::Symbol { id, sym, span }
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
            owner_module: ModuleId(0),
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
            owner_module: ModuleId(0),
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
            owner_module: ModuleId(0),
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

    /// Record-payload union variant `Login { userId = 7 }` (is_record = false,
    /// non-empty fields) → outer `Construct { UnionVariant }` whose single payload
    /// slot holds a nested `Construct { Record }`, so the runtime shape is the
    /// tagged tuple `{'Login', #{userId => 7}}`.
    #[test]
    fn lower_record_variant_construction_nests() {
        use ridge_ast::{
            expr::{FieldInit, RecordCtor},
            Literal,
        };
        use ridge_resolve::SymbolId;

        let (mut ctx, ctor_span) = make_binding_ctx(Binding::Constructor {
            owner_type: SymbolId(0),
            variant: 1,
            is_record: false,
            owner_module: ModuleId(0),
        });

        let expr = Expr::Record {
            constructor: RecordCtor::Bare(Ident {
                text: "Login".into(),
                span: ctor_span,
            }),
            fields: vec![FieldInit {
                name: Ident {
                    text: "userId".into(),
                    span: sp(),
                },
                value: Some(Expr::Literal(Literal::IntDec {
                    raw: "7".into(),
                    span: sp(),
                })),
                span: sp(),
            }],
            span: sp_at(10, 30),
        };
        let ir = lower_expr(&mut ctx, &expr);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        match ir {
            IrExpr::Construct { ctor, fields, .. } => {
                match ctor {
                    SymbolRef::Constructor {
                        ctor_kind: ridge_ir::CtorKind::UnionVariant,
                        name,
                        variant,
                        ..
                    } => {
                        assert_eq!(name, "Login");
                        assert_eq!(variant, 1, "variant index preserved");
                    }
                    other => panic!("expected outer UnionVariant, got {other:?}"),
                }
                assert_eq!(fields.len(), 1, "outer tag has one payload slot");
                match &fields[0].1 {
                    IrExpr::Construct {
                        ctor: inner_ctor,
                        fields: inner_fields,
                        ..
                    } => {
                        assert!(matches!(
                            inner_ctor,
                            SymbolRef::Constructor {
                                ctor_kind: ridge_ir::CtorKind::Record,
                                ..
                            }
                        ));
                        assert_eq!(inner_fields.len(), 1, "inner record: one field");
                        assert_eq!(inner_fields[0].0, "userId");
                    }
                    other => panic!("expected inner Record construct, got {other:?}"),
                }
            }
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

    /// `fn ((a, b), c) -> a` — a *nested* destructuring param. The inner tuple
    /// elements used to degrade to wildcards, dropping `a`/`b`; they must now
    /// lower to real binds. Guards the generalised destructuring-param path so
    /// non-`Var` sub-patterns can never again be silently discarded.
    #[test]
    fn lower_lambda_nested_tuple_param_binds_inner() {
        use ridge_ast::expr::LambdaParam;
        use ridge_ast::Literal;

        let mut ctx = fresh_ctx();
        let lambda_span = sp_at(0, 25);

        // Pattern: ((a, b), c)
        let expr = Expr::Lambda {
            params: vec![LambdaParam::Pattern(Pattern::Tuple {
                elems: vec![
                    Pattern::Tuple {
                        elems: vec![
                            Pattern::Var {
                                name: Ident {
                                    text: "a".into(),
                                    span: sp_at(5, 6),
                                },
                                span: sp_at(5, 6),
                            },
                            Pattern::Var {
                                name: Ident {
                                    text: "b".into(),
                                    span: sp_at(8, 9),
                                },
                                span: sp_at(8, 9),
                            },
                        ],
                        span: sp_at(4, 10),
                    },
                    Pattern::Var {
                        name: Ident {
                            text: "c".into(),
                            span: sp_at(12, 13),
                        },
                        span: sp_at(12, 13),
                    },
                ],
                span: sp_at(3, 15),
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
                assert_eq!(params.len(), 1, "must have exactly 1 IR param");
                assert!(
                    params[0].name.starts_with("__tuple_param"),
                    "synthetic param must start with __tuple_param, got {:?}",
                    params[0].name
                );
                match *body {
                    IrExpr::Match { arms, .. } => {
                        assert_eq!(arms.len(), 1, "match must have 1 arm");
                        match &arms[0].pat {
                            IrPat::Tuple { elems, .. } => {
                                assert_eq!(elems.len(), 2, "outer tuple pat must have 2 elems");
                                // First elem: the inner tuple (a, b) — must bind a
                                // and b, NOT degrade to a wildcard as it did before.
                                match &elems[0] {
                                    IrPat::Tuple { elems: inner, .. } => {
                                        assert_eq!(
                                            inner.len(),
                                            2,
                                            "inner tuple pat must have 2 elems"
                                        );
                                        assert!(
                                            matches!(&inner[0], IrPat::Bind { name, .. } if name == "a"),
                                            "inner first elem must be Bind(a), got {:?}",
                                            inner[0]
                                        );
                                        assert!(
                                            matches!(&inner[1], IrPat::Bind { name, .. } if name == "b"),
                                            "inner second elem must be Bind(b), got {:?}",
                                            inner[1]
                                        );
                                    }
                                    other => panic!("expected inner Tuple pat, got {other:?}"),
                                }
                                assert!(
                                    matches!(&elems[1], IrPat::Bind { name, .. } if name == "c"),
                                    "outer second elem must be Bind(c), got {:?}",
                                    elems[1]
                                );
                            }
                            other => panic!("expected Tuple pat, got {other:?}"),
                        }
                    }
                    other => panic!("expected Match body, got {other:?}"),
                }
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
