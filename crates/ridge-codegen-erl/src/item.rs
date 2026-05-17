//! §4.26–§4.27 — Lower `IrItem::Fn` and `IrItem::Const` to `CErlFn`.
//!
//! Top-level item lowering is the final piece before module assembly in T8.
//! Each `IrFn` becomes a quoted Core Erlang function with span/caps annotations;
//! each `IrConst` becomes a 0-arity function whose body returns the constant
//! value (Core Erlang has no top-level `const` form).

// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
#![allow(clippy::redundant_pub_crate)]
// lower_fn is the public API used by unit tests; lower_fn_with_module_name is
// the production path used by module.rs.  Suppress dead_code for lower_fn.
#![allow(dead_code)]

use crate::core_ast::{CErlAnn, CErlAtom, CErlExpr, CErlFn, CErlVar};
use crate::error::CodegenError;
use crate::expr::{lower_expr_in_scope, name_to_erl_var};
use crate::return_::{elide_tail_returns, has_non_tail_return, wrap_with_return_catch};
use crate::scope::LocalScope;
use ridge_ir::{IrConst, IrFn, LoweredWorkspace};
use rustc_hash::FxHashMap;

// ── §4.26  IrItem::Fn ─────────────────────────────────────────────────────────

/// Lower an [`IrFn`] to a [`CErlFn`].
///
/// ## What is emitted
/// - `name` → quoted atom, e.g. `'my_fn'`.
/// - `arity` = `params.len()`.
/// - `anns` → `%% File: <span.start>, Caps: <caps>` annotation.
/// - `body` → `fun (P1, ..., PN) -> lower_expr(fn_.body) end`.
///
/// ## What is dropped (type erasure, §4.26)
/// `ret_ty`, `params[i].ty`, `scheme` — Core Erlang is dynamically typed.
/// Future LLVM/WASM-GC backends will consume these from `LoweredModule.node_types`.
///
/// ## `is_main` arity check (§4.26)
/// A canonical `main` function must take exactly 1 argument (the CLI args list).
/// If `is_main` is true and `params.len() != 1`, a `%% Warning: main/N arity` annotation
/// is added and lowering continues — codegen is not failed.
///
/// ## Name quoting (§4.26)
/// Always quote function name in emission for safety.
///
/// ## Caps (§4.26 + §6)
/// Capabilities are emitted **only** as a `%% Caps: …` `CErlAnn` annotation —
/// never as runtime code (D018 Model B erasure).
pub(crate) fn lower_fn(
    fn_: &IrFn,
    ws: &LoweredWorkspace,
    fn_arity: &FxHashMap<String, u32>,
) -> Result<CErlFn, CodegenError> {
    lower_fn_with_module_name(fn_, ws, fn_arity, None)
}

/// Like [`lower_fn`] but also passes the parent module's BEAM name into scope
/// so that `IrExpr::Spawn` can derive the correct actor BEAM module atom.
pub(crate) fn lower_fn_with_module_name(
    fn_: &IrFn,
    _ws: &LoweredWorkspace,
    fn_arity: &FxHashMap<String, u32>,
    module_beam_name: Option<&str>,
) -> Result<CErlFn, CodegenError> {
    // Build the parameter variable list.
    let params: Vec<CErlVar> = fn_
        .params
        .iter()
        .map(|p| CErlVar(name_to_erl_var(&p.name)))
        .collect();

    let arity = u32::try_from(params.len()).map_err(|_| CodegenError::IrShapeMalformed {
        variant: "IrFn",
        span: fn_.span,
        detail: format!(
            "function '{}' has {} parameters, exceeding the u32 limit",
            fn_.name,
            params.len()
        ),
    })?;

    // Lower the body expression using a scope seeded with the module arity table
    // so that SymbolRef::Local used as a value resolves to a LocalFnRef.
    // The module_beam_name (when present) enables `lower_spawn` to derive the
    // correct actor BEAM module atom via the same convention as `actor.rs`.
    let mut scope = module_beam_name.map_or_else(
        || LocalScope::with_arity(fn_arity.clone()),
        |name| LocalScope::with_arity_and_module(fn_arity.clone(), name),
    );
    // §4.9 — Route through elide/wrap based on whether non-tail Returns exist.
    //
    // - Non-tail Returns present: lower as-is (Return nodes emit throws) then
    //   wrap the whole body in a try/catch {ridge_return, V} frame.
    // - All Returns are tail-position (or none): elide them first (Replace
    //   Return { value } → value) so lower_expr never emits a throw — no
    //   try/catch wrapper needed.
    let lowered_body = if has_non_tail_return(&fn_.body) {
        let body = lower_expr_in_scope(&fn_.body, &mut scope)?;
        wrap_with_return_catch(body)
    } else {
        // Elide tail Returns before lowering — lower_expr emits throws for
        // *all* Return nodes, so we must strip them first when they are in
        // tail position (no throw+catch needed, value flows naturally).
        let elided = elide_tail_returns(&fn_.body);
        lower_expr_in_scope(&elided, &mut scope)?
    };

    // Build the annotations.
    // §4.26: `%% File: <span.file>, Line: <span.line>` at the head.
    // Span only carries byte offsets (no file/line in this IR stage).
    let file_ann = CErlAnn(format!("%% File: <source>, Offset: {}", fn_.span.start));

    // §4.26: `%% Caps: …` annotation — metadata only, no runtime code.
    let caps_ann = CErlAnn(format!("%% Caps: {}", fn_.caps));

    let mut anns = vec![file_ann, caps_ann];

    // §4.26: is_main arity check warning.
    // A canonical main function takes exactly 1 argument (the CLI args list).
    // We do not fail codegen — we annotate and continue.
    // NOTE: tracing is not a dependency of this crate; warning is emitted as
    //       a CErlAnn annotation (metadata-only, §4.26 contract).
    if fn_.is_main && arity != 1 {
        anns.push(CErlAnn(format!(
            "%% Warning: main/{arity} — canonical main should have arity 1 (§4.26)"
        )));
    }

    // The body is `fun (P1, ..., PN) -> lower_body end`.
    let fun_body = CErlExpr::Fun {
        params,
        body: Box::new(lowered_body),
    };

    Ok(CErlFn {
        name: CErlAtom(fn_.name.clone()),
        arity,
        anns,
        body: fun_body,
    })
}

// ── §4.27  IrItem::Const ─────────────────────────────────────────────────────

/// Lower an [`IrConst`] to a zero-arity [`CErlFn`].
///
/// Core Erlang has no top-level `const` form.  We emit a 0-arity function
/// whose body is `lower_expr(c.value)`:
///
/// ```text
/// '<name>'/0 =
///     %% Const: <name>: <ty_metadata_only>
///     fun () -> <lower_expr(value)> end
/// ```
///
/// Type info is metadata-only: `ty` appears in the annotation but is **not**
/// emitted as runtime code (type erasure, §4.27).
///
/// Call sites for consts use `call '<own_module>':'<name>' ()` (0-arity call);
/// that lowering is T6's job (§4.27 note).
pub(crate) fn lower_const(
    c: &IrConst,
    _ws: &LoweredWorkspace,
    fn_arity: &FxHashMap<String, u32>,
) -> Result<CErlFn, CodegenError> {
    let mut scope = LocalScope::with_arity(fn_arity.clone());
    let lowered_value = lower_expr_in_scope(&c.value, &mut scope)?;

    // §4.27: `%% Const: <name>: <ty_metadata_only>` annotation.
    // The `ty` field is formatted for metadata purposes only — not emitted as code.
    let const_ann = CErlAnn(format!("%% Const: {}: {:?}", c.name, c.ty));

    let fun_body = CErlExpr::Fun {
        params: vec![],
        body: Box::new(lowered_value),
    };

    Ok(CErlFn {
        name: CErlAtom(c.name.clone()),
        arity: 0,
        anns: vec![const_ann],
        body: fun_body,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAnn, CErlExpr, CErlLit};
    use ridge_ast::Span;
    use ridge_ir::{
        CapabilitySet, IrConst, IrExpr, IrFn, IrLit, IrNodeId, IrParam, LoweredWorkspace, ModuleId,
        NodeId, Scheme, Type,
    };

    fn sp() -> Span {
        Span::point(0)
    }

    fn empty_arity() -> FxHashMap<String, u32> {
        FxHashMap::default()
    }

    fn lit_unit_expr() -> IrExpr {
        IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Unit,
            span: sp(),
        }
    }

    fn lit_int_expr(n: i64) -> IrExpr {
        IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Int(n),
            span: sp(),
        }
    }

    fn empty_ws() -> LoweredWorkspace {
        LoweredWorkspace::empty(1, 0)
    }

    // ── lower_fn tests ────────────────────────────────────────────────────────

    #[test]
    fn lower_fn_params_and_arity() {
        let fn_ = IrFn {
            name: "greet".into(),
            module: ModuleId(0),
            params: vec![
                IrParam {
                    name: "name".into(),
                    ty: Type::Error,
                    span: sp(),
                },
                IrParam {
                    name: "age".into(),
                    ty: Type::Error,
                    span: sp(),
                },
            ],
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: lit_unit_expr(),
            origin: NodeId(0),
            span: sp(),
            is_pub: false,
            is_main: false,
            doc: None,
        };
        let ws = empty_ws();
        let result = lower_fn(&fn_, &ws, &empty_arity()).unwrap();

        // Arity must match param count.
        assert_eq!(result.arity, 2);

        // The body must be a Fun with 2 param CErlVars.
        match &result.body {
            CErlExpr::Fun { params, .. } => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].0, "V_Name");
                assert_eq!(params[1].0, "V_Age");
            }
            other => panic!("expected CErlExpr::Fun, got {other:?}"),
        }
    }

    #[test]
    fn lower_fn_annotations_present() {
        let fn_ = IrFn {
            name: "run".into(),
            module: ModuleId(0),
            params: vec![],
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: lit_unit_expr(),
            origin: NodeId(0),
            span: Span::new(10, 40),
            is_pub: false,
            is_main: false,
            doc: None,
        };
        let ws = empty_ws();
        let result = lower_fn(&fn_, &ws, &empty_arity()).unwrap();

        // Must have at least 2 annotations: File and Caps.
        assert!(result.anns.len() >= 2, "expected File + Caps annotations");

        let has_file = result.anns.iter().any(|CErlAnn(s)| s.contains("File:"));
        let has_caps = result.anns.iter().any(|CErlAnn(s)| s.contains("Caps:"));
        assert!(has_file, "missing %% File: annotation");
        assert!(has_caps, "missing %% Caps: annotation");
    }

    #[test]
    fn lower_fn_body_lowered() {
        let fn_ = IrFn {
            name: "answer".into(),
            module: ModuleId(0),
            params: vec![],
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: lit_int_expr(42),
            origin: NodeId(0),
            span: sp(),
            is_pub: false,
            is_main: false,
            doc: None,
        };
        let ws = empty_ws();
        let result = lower_fn(&fn_, &ws, &empty_arity()).unwrap();

        // The fun body should lower the int literal.
        match &result.body {
            CErlExpr::Fun { body, .. } => {
                assert!(
                    matches!(body.as_ref(), CErlExpr::Lit(CErlLit::Int(42))),
                    "body did not lower to Int(42)"
                );
            }
            other => panic!("expected Fun, got {other:?}"),
        }
    }

    #[test]
    fn lower_fn_main_arity_warning_annotation() {
        // is_main with wrong arity → warning annotation, no error.
        let fn_ = IrFn {
            name: "main".into(),
            module: ModuleId(0),
            params: vec![], // arity 0, but is_main expects 1
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: lit_unit_expr(),
            origin: NodeId(0),
            span: sp(),
            is_pub: false,
            is_main: true,
            doc: None,
        };
        let ws = empty_ws();
        let result = lower_fn(&fn_, &ws, &empty_arity()).unwrap();

        // Codegen must NOT fail — it must succeed and add a warning annotation.
        let has_warning = result.anns.iter().any(|CErlAnn(s)| s.contains("Warning:"));
        assert!(
            has_warning,
            "expected Warning annotation for is_main with arity != 1"
        );
    }

    #[test]
    fn lower_fn_main_correct_arity_no_warning() {
        // is_main with correct arity (1) → no warning.
        let fn_ = IrFn {
            name: "main".into(),
            module: ModuleId(0),
            params: vec![IrParam {
                name: "args".into(),
                ty: Type::Error,
                span: sp(),
            }],
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: lit_unit_expr(),
            origin: NodeId(0),
            span: sp(),
            is_pub: false,
            is_main: true,
            doc: None,
        };
        let ws = empty_ws();
        let result = lower_fn(&fn_, &ws, &empty_arity()).unwrap();

        let has_warning = result.anns.iter().any(|CErlAnn(s)| s.contains("Warning:"));
        assert!(!has_warning, "unexpected Warning annotation for main/1");
    }

    // ── lower_const tests ─────────────────────────────────────────────────────

    #[test]
    fn lower_const_zero_arity() {
        let c = IrConst {
            name: "max_retries".into(),
            ty: Type::Error,
            value: lit_int_expr(3),
            origin: NodeId(0),
            span: sp(),
            is_pub: false,
        };
        let ws = empty_ws();
        let result = lower_const(&c, &ws, &empty_arity()).unwrap();

        // Must be arity 0.
        assert_eq!(result.arity, 0);
        assert_eq!(result.name.0, "max_retries");

        // Body must be a 0-param Fun.
        match &result.body {
            CErlExpr::Fun { params, body } => {
                assert!(params.is_empty(), "const fn must have 0 params");
                assert!(
                    matches!(body.as_ref(), CErlExpr::Lit(CErlLit::Int(3))),
                    "const value not lowered correctly"
                );
            }
            other => panic!("expected Fun, got {other:?}"),
        }
    }

    #[test]
    fn lower_const_has_annotation() {
        let c = IrConst {
            name: "timeout_ms".into(),
            ty: Type::Error,
            value: lit_int_expr(5000),
            origin: NodeId(0),
            span: sp(),
            is_pub: true,
        };
        let ws = empty_ws();
        let result = lower_const(&c, &ws, &empty_arity()).unwrap();

        let has_const_ann = result.anns.iter().any(|CErlAnn(s)| s.contains("Const:"));
        assert!(has_const_ann, "missing %% Const: annotation");
    }

    #[test]
    fn lower_fn_name_preserved() {
        let fn_ = IrFn {
            name: "parse_line".into(),
            module: ModuleId(0),
            params: vec![],
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: lit_unit_expr(),
            origin: NodeId(0),
            span: sp(),
            is_pub: true,
            is_main: false,
            doc: None,
        };
        let ws = empty_ws();
        let result = lower_fn(&fn_, &ws, &empty_arity()).unwrap();

        // Name is preserved verbatim (always quoted by the printer).
        assert_eq!(result.name.0, "parse_line", "fn name not preserved");
    }

    #[test]
    fn lower_fn_atom_name_matches_ir() {
        let fn_ = IrFn {
            name: "CamelCase".into(),
            module: ModuleId(0),
            params: vec![],
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: lit_unit_expr(),
            origin: NodeId(0),
            span: sp(),
            is_pub: false,
            is_main: false,
            doc: None,
        };
        let ws = empty_ws();
        let result = lower_fn(&fn_, &ws, &empty_arity()).unwrap();

        // §4.26: name is preserved verbatim; quoting is the printer's job.
        assert_eq!(result.name.0, "CamelCase");
    }
}
