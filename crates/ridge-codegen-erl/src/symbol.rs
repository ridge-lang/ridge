//! §4.3 — Lower `IrExpr::Symbol` / `SymbolRef` to a `CErlExpr`.
//!
//! Each `SymbolRef` variant routes to a different Core Erlang shape.
//! Several variants require arity lookup or stdlib-bridge data that is
//! assembled in later tasks (T6/T7/T8); those arms return a deferred
//! `IrShapeMalformed` error rather than panicking.
//!
//! Fully implemented in T3:
//! - `Constructor { fields: [] used-as-value }` → `'<name>'` atom.
//! - `Prelude { name: "None" }` → `'none'` atom.
//! - `Handler { .. }` → defensive `IrShapeMalformed` (never a value).
//! - `ActorType { .. }` → defensive `IrShapeMalformed` (never a value).
//!
//! Implemented in T7:
//! - `Stdlib { .. }` → bridge map lookup → 0-arg `Call` to BEAM target.
//!
//! Deferred to T8:
//! - `Local`, `External`, `Constructor` (fn-value form), `Prelude`
//!   (non-None used-as-value).

// T3 helpers are consumed by lower_expr (expr.rs) and wired into the
// module-level entry points in T8.  Until T8 ships they are only exercised
// from within this module's test suite and from expr.rs.
#![allow(dead_code)]
// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it anyway for explicitness per plan §2.2 — suppress the lint here.
#![allow(clippy::redundant_pub_crate)]

use crate::core_ast::{CErlAtom, CErlExpr, CErlLit, CErlVar};
use crate::error::CodegenError;
use crate::stdlib_map::{self, BridgeTarget};
use ridge_ast::Span;
use ridge_ir::SymbolRef;
use ridge_resolve::ModuleId;
use rustc_hash::FxHashMap;

/// B-D009 hotfix v3 Wave 2: emit a lambda that captures `arity` parameters
/// and forwards them to the BEAM target, so the stdlib symbol used as a
/// value behaves as a true function reference rather than a 0-arg call.
///
/// Pre-fix, `lower_symbol` for a `SymbolRef::Stdlib` used as a value emitted
/// `call 'M':'F' ()` — a zero-argument call to the target.  At runtime that
/// invoked the BEAM function with no arguments, which is `undef` for every
/// arity-1+ stdlib fn.  `List.map Text.byteSize lns` crashed with
/// `erlang:byte_size/0 undef`.
///
/// The fix emits `fun (V_X0, ..., V_XN) -> call 'M':'F' (V_X0, ..., V_XN)`,
/// which is a regular Erlang fun reference of the correct arity that callers
/// can invoke as `Fun(X1, ..., XN)`.
fn stdlib_value_fn_ref(module: CErlAtom, fn_name: CErlAtom, arity: u32) -> CErlExpr {
    let params: Vec<CErlVar> = (0..arity).map(|i| CErlVar(format!("V_X{i}"))).collect();
    let args: Vec<CErlExpr> = params.iter().map(|p| CErlExpr::Var(p.clone())).collect();
    CErlExpr::Fun {
        params,
        body: Box::new(CErlExpr::Call {
            module,
            fn_name,
            args,
        }),
    }
}

/// Lower a [`SymbolRef`] used as a value expression to a [`CErlExpr`].
///
/// Variants whose lowering requires arity lookup, stdlib mapping, or other
/// data assembled by later tasks return a deferred `IrShapeMalformed` with a
/// `"T3: <variant> routing pending T6/T7/T8"` detail message.
///
/// The `fn_arity` table maps local fn/const names to their arities; it is
/// built by the module-level lowering pass (T8) and threaded via `LocalScope`.
///
/// `actor_parent` — when `Some((parent_id, parent_beam_name))` — identifies the
/// enclosing actor's parent module.  A `SymbolRef::Local { module: parent_id }`
/// used as a **value** must be emitted as a 0-arg qualified `call` rather than a
/// `LocalFnRef`, because the actor is a separate BEAM module and `'fnName'/0`
/// would be `undefined` there (B-6 fix, Phase 6 pass 3).
#[allow(clippy::too_many_lines)]
pub(crate) fn lower_symbol(
    sym: &SymbolRef,
    span: Span,
    fn_arity: &FxHashMap<String, u32>,
    actor_parent: Option<(ModuleId, &str)>,
) -> Result<CErlExpr, CodegenError> {
    match sym {
        // Local fn or const used as a value: look up arity from the module
        // function table assembled in T8.
        //
        // B-6 (Phase 6 pass 3): when `actor_parent` is set and this symbol's
        // `module` matches the parent's id, emit a qualified 0-arg `call` so that
        // the value expression works in the actor's separate BEAM module.
        SymbolRef::Local { name, module } => match fn_arity.get(name.as_str()) {
            Some(&arity) => {
                // Check for cross-module reference from an actor body.
                if let Some((parent_id, parent_beam)) = actor_parent {
                    if *module == parent_id {
                        // Emit call 'parent':'fn' () — a 0-arg qualified call that
                        // evaluates the parent-module fn/const as a value.
                        return Ok(CErlExpr::Call {
                            module: CErlAtom(parent_beam.to_owned()),
                            fn_name: CErlAtom(name.clone()),
                            args: vec![],
                        });
                    }
                }
                if arity == 0 {
                    // Zero-arity constants must be *called* (not referenced) when
                    // used as a value.  In Core Erlang `'name'/0` is a function
                    // reference; `apply 'name'/0 ()` is the actual value.
                    // Ridge constants (`const foo: Int = 30`) are always evaluated
                    // at the use-site — they are never passed as thunks.
                    Ok(CErlExpr::Apply {
                        callee: Box::new(CErlExpr::LocalFnRef {
                            name: CErlAtom(name.clone()),
                            arity: 0,
                        }),
                        args: vec![],
                    })
                } else {
                    Ok(CErlExpr::LocalFnRef {
                        name: CErlAtom(name.clone()),
                        arity,
                    })
                }
            }
            None => Err(CodegenError::IrShapeMalformed {
                variant: "SymbolRef::Local",
                span,
                detail: format!("Local symbol '{name}' not found in fn-arity table (T8)"),
            }),
        },

        // Stdlib symbols used as a *value* (rare — usually appear as Call callees).
        // §4.3: emit a 0-arg Call to the BEAM target so the value is a fun reference.
        // The `Some(_)` arm below is a defensive catch for future `#[non_exhaustive]`
        // BridgeTarget variants; suppress the unreachable-pattern warning inside this crate.
        #[allow(unreachable_patterns)]
        SymbolRef::Stdlib { module, name } => {
            match stdlib_map::lookup(module, name) {
                None => Err(CodegenError::StdlibBridgeMissing {
                    module: module.clone(),
                    name: name.clone(),
                    span,
                }),
                // B-D009 hotfix v3 Wave 2: emit a fun reference that captures
                // the bridge target's arity, not a 0-arg call.  The previous
                // 0-arg call produced `erlang:byte_size/0 undef` whenever a
                // stdlib fn was passed as a HOF argument.
                Some(
                    BridgeTarget::BeamStdlib {
                        module: m,
                        fn_name,
                        arity,
                    }
                    | BridgeTarget::BeamStdlibPerm {
                        module: m,
                        fn_name,
                        arity,
                        ..
                    },
                ) => Ok(stdlib_value_fn_ref(
                    CErlAtom((*m).into()),
                    CErlAtom((*fn_name).into()),
                    *arity,
                )),
                Some(BridgeTarget::RidgeRuntime { fn_name, arity, .. }) => Ok(stdlib_value_fn_ref(
                    CErlAtom("ridge_rt".into()),
                    CErlAtom((*fn_name).into()),
                    *arity,
                )),
                // Phase 7: RidgeStdlibLocal — emit a fun reference to the BEAM
                // target with the recorded arity (same B-D009 fix as above).
                Some(BridgeTarget::RidgeStdlibLocal {
                    beam_module,
                    fn_name,
                    arity,
                    ..
                }) => Ok(stdlib_value_fn_ref(
                    CErlAtom(beam_module.clone()),
                    CErlAtom(fn_name.clone()),
                    *arity,
                )),
                // #[non_exhaustive] catch.
                Some(_) => Err(CodegenError::IrShapeMalformed {
                    variant: "SymbolRef::Stdlib",
                    span,
                    detail: "unrecognised BridgeTarget variant".into(),
                }),
            }
        }

        // External symbols: require arity lookup and external module name
        // mangling, assembled in T7/T8.
        SymbolRef::External { name, .. } => Err(CodegenError::IrShapeMalformed {
            variant: "SymbolRef::External",
            span,
            detail: format!(
                "T3: External symbol '{name}' routing pending T7/T8 (arity lookup not yet wired)"
            ),
        }),

        // Handler is never a value — Phase 5 ensures handlers only appear
        // inside Send/Ask.  Any occurrence here is a Phase-5 invariant violation.
        SymbolRef::Handler { actor, handler, .. } => Err(CodegenError::IrShapeMalformed {
            variant: "SymbolRef::Handler",
            span,
            detail: format!(
                "Handler '{actor}/{handler}' appeared as a value expression; \
                 handlers are never values (Phase 5 invariant violated)"
            ),
        }),

        // ActorType is never a value — Phase 5 ensures actor types only appear
        // inside Spawn.  Any occurrence here is a Phase-5 invariant violation.
        SymbolRef::ActorType { name, .. } => Err(CodegenError::IrShapeMalformed {
            variant: "SymbolRef::ActorType",
            span,
            detail: format!(
                "ActorType '{name}' appeared as a value expression; \
                 actor types are never values (Phase 5 invariant violated)"
            ),
        }),

        // Constructor used as a zero-arg value: emit the bare atom `'<name>'`.
        // (Phase 5 wraps non-zero-arg constructors in IrExpr::Construct, so the
        // zero-arg path is the one meaningful case for T3.)
        // The fn-value form (e.g. `[Some, Some]` mapped over a list) is deferred to T6.
        //
        // §4.3: "If used as a value with no arg: emit the bare atom '<name>'."
        SymbolRef::Constructor { name, .. } => {
            Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(name.clone()))))
        }

        // Prelude symbols used as values:
        //   "None" → atom `'none'` (zero-arg, fully implementable in T3).
        //   All others: deferred — Phase 5 wraps Ok/Err/Some in Construct.
        SymbolRef::Prelude { name } => match name.as_str() {
            "None" => Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom("none".into())))),
            other => Err(CodegenError::IrShapeMalformed {
                variant: "SymbolRef::Prelude",
                span,
                detail: format!("T3: Prelude '{other}' used-as-value routing pending T6/T7/T8"),
            }),
        },

        // SymbolRef is #[non_exhaustive]; catch future variants defensively.
        _ => Err(CodegenError::IrShapeMalformed {
            variant: "SymbolRef",
            span,
            detail: "T3: unrecognised SymbolRef variant — pending future lowering task".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::CErlAtom;
    use ridge_ast::Span;
    use ridge_ir::{CtorKind, SymbolRef};
    use ridge_resolve::ModuleId;
    use ridge_types::TyConId;

    fn sp() -> Span {
        Span::point(0)
    }

    fn empty_arity() -> FxHashMap<String, u32> {
        FxHashMap::default()
    }

    #[test]
    fn symbol_local_unknown_in_table_is_error() {
        // With an empty arity table, an unknown local → IrShapeMalformed.
        let sym = SymbolRef::Local {
            name: "myFn".into(),
            module: ModuleId(0),
        };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        assert!(matches!(
            result,
            Err(CodegenError::IrShapeMalformed {
                variant: "SymbolRef::Local",
                ..
            })
        ));
    }

    #[test]
    fn symbol_local_known_emits_local_fn_ref() {
        // With a populated arity table, a known local → LocalFnRef (arity > 0).
        let sym = SymbolRef::Local {
            name: "myFn".into(),
            module: ModuleId(0),
        };
        let mut table = FxHashMap::default();
        table.insert("myFn".to_owned(), 2u32);
        let result = lower_symbol(&sym, sp(), &table, None);
        assert!(
            matches!(result, Ok(CErlExpr::LocalFnRef { ref name, arity: 2 }) if name.0 == "myFn"),
            "expected LocalFnRef(myFn/2), got {result:?}"
        );
    }

    #[test]
    fn symbol_local_zero_arity_emits_apply() {
        // Zero-arity local (constant) used as a value must be *called* so that
        // the Erlang value (not the function reference) is produced.
        // `apply 'myConst'/0 ()` — not the bare `'myConst'/0` fun reference.
        let sym = SymbolRef::Local {
            name: "myConst".into(),
            module: ModuleId(0),
        };
        let mut table = FxHashMap::default();
        table.insert("myConst".to_owned(), 0u32);
        let result = lower_symbol(&sym, sp(), &table, None);
        match result {
            Ok(CErlExpr::Apply {
                ref callee,
                ref args,
            }) => {
                assert!(
                    matches!(**callee, CErlExpr::LocalFnRef { ref name, arity: 0 } if name.0 == "myConst"),
                    "expected LocalFnRef(myConst/0), got {callee:?}"
                );
                assert!(args.is_empty(), "zero-arity apply must have no args");
            }
            other => panic!("expected Apply(LocalFnRef(myConst/0), []), got {other:?}"),
        }
    }

    #[test]
    fn symbol_stdlib_known_emits_fun_ref() {
        // B-D009 hotfix v3 Wave 2: a known stdlib symbol used as a value
        // emits a `fun (params) -> call 'M':'F' (params)` wrapper of the
        // correct arity.  The earlier behaviour (0-arg Call) was broken —
        // it crashed at runtime with `undef` for every arity-1+ stdlib fn
        // passed as a HOF argument.
        let sym = SymbolRef::Stdlib {
            module: "std.list".into(),
            name: "map".into(),
        };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        match result {
            Ok(CErlExpr::Fun { params, body }) => {
                assert_eq!(params.len(), 2, "lists:map/2 fun should have 2 params");
                let CErlExpr::Call {
                    module,
                    fn_name,
                    args,
                } = *body
                else {
                    panic!("Fun body must be a Call");
                };
                assert_eq!(module.0, "lists", "expected BEAM module 'lists'");
                assert_eq!(fn_name.0, "map", "expected BEAM fn 'map'");
                assert_eq!(args.len(), 2, "Call must forward both fun params");
            }
            other => panic!("expected Ok(CErlExpr::Fun{{..}}), got {other:?}"),
        }
    }

    #[test]
    fn symbol_stdlib_unknown_emits_e002() {
        // Unknown stdlib symbol → E002 StdlibBridgeMissing.
        let sym = SymbolRef::Stdlib {
            module: "std.unknown".into(),
            name: "bogus".into(),
        };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        assert!(
            matches!(result, Err(CodegenError::StdlibBridgeMissing { .. })),
            "expected StdlibBridgeMissing, got {result:?}"
        );
    }

    #[test]
    fn symbol_external_is_deferred() {
        let sym = SymbolRef::External {
            module: ModuleId(1),
            name: "helper".into(),
        };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        assert!(matches!(
            result,
            Err(CodegenError::IrShapeMalformed {
                variant: "SymbolRef::External",
                ..
            })
        ));
    }

    #[test]
    fn symbol_handler_is_error() {
        let sym = SymbolRef::Handler {
            actor_module: ModuleId(0),
            actor: "Counter".into(),
            handler: "increment".into(),
        };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        assert!(matches!(
            result,
            Err(CodegenError::IrShapeMalformed {
                variant: "SymbolRef::Handler",
                ..
            })
        ));
    }

    #[test]
    fn symbol_actor_type_is_error() {
        let sym = SymbolRef::ActorType {
            module: ModuleId(0),
            name: "Counter".into(),
        };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        assert!(matches!(
            result,
            Err(CodegenError::IrShapeMalformed {
                variant: "SymbolRef::ActorType",
                ..
            })
        ));
    }

    #[test]
    fn symbol_constructor_zero_arg_emits_atom() {
        // §4.3: Constructor used as a zero-arg value → bare atom.
        let sym = SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            owner_type: TyConId(0),
            name: "None".into(),
            variant: 0,
        };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        assert!(matches!(result, Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s)))) if s == "None"));
    }

    #[test]
    fn symbol_prelude_none_emits_atom() {
        // §4.3 Prelude row: "None" → atom `'none'`.
        let sym = SymbolRef::Prelude {
            name: "None".into(),
        };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        assert!(matches!(result, Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s)))) if s == "none"));
    }

    #[test]
    fn symbol_prelude_some_is_deferred() {
        let sym = SymbolRef::Prelude {
            name: "Some".into(),
        };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        assert!(matches!(
            result,
            Err(CodegenError::IrShapeMalformed {
                variant: "SymbolRef::Prelude",
                ..
            })
        ));
    }

    #[test]
    fn symbol_prelude_ok_is_deferred() {
        let sym = SymbolRef::Prelude { name: "Ok".into() };
        let result = lower_symbol(&sym, sp(), &empty_arity(), None);
        assert!(matches!(
            result,
            Err(CodegenError::IrShapeMalformed {
                variant: "SymbolRef::Prelude",
                ..
            })
        ));
    }
}
