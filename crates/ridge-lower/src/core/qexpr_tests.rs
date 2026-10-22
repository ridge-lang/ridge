//! Quotation constructors must follow the schema, including after reordering.

use super::*;

fn workspace() -> ridge_typecheck::TypedWorkspace {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname = \"quote-schema\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("apps/demo/src")).unwrap();
    std::fs::write(
        dir.path().join("apps/demo/ridge.toml"),
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("apps/demo/src/Main.ridge"),
        "pub fn main () -> Int = 1\n",
    )
    .unwrap();
    let graph = ridge_resolve::discover_workspace(dir.path()).graph.unwrap();
    let resolved = ridge_resolve::resolve_workspace(graph);
    ridge_typecheck::typecheck_workspace(&resolved).typed
}

fn schema(ws: &mut ridge_typecheck::TypedWorkspace) -> &mut ridge_types::UnionSchema {
    let TyConKind::Union(schema) = &mut ws.tycons[ws.builtins.q_expr.0 as usize].kind else {
        panic!("QExpr must be a union")
    };
    schema
}

#[test]
fn quotation_literal_follows_reordered_schema() {
    let mut ws = workspace();
    let variants = &mut schema(&mut ws).variants;
    let index = variants.iter().position(|v| v.name == "QLitInt").unwrap();
    variants.swap(0, index);
    let mut ctx = LowerCtx::new(ModuleId(0), &[], &ws.builtins);
    ctx.attach_workspace(&ws);
    let span = Span::point(0);
    let expr = Expr::Literal(Literal::IntDec {
        raw: "42".into(),
        span,
    });
    let ir = reify_node(&mut ctx, &expr, &[]);
    assert!(ctx.errors.is_empty(), "{:?}", ctx.errors);
    let IrExpr::Construct { ctor, fields, .. } = ir else {
        panic!("expected constructor")
    };
    assert!(
        matches!(ctor, SymbolRef::Constructor { owner_type: Some(owner), variant: 0, ref name, .. }
        if owner == ws.builtins.q_expr && name == "QLitInt")
    );
    assert!(
        matches!(&fields[..], [(field, IrExpr::Lit { value: IrLit::Int(42), .. })] if field == "$0")
    );
}

#[test]
fn every_qexpr_constructor_follows_its_schema_after_reordering() {
    let mut ws = workspace();
    schema(&mut ws).variants.reverse();
    let TyConKind::Union(union) = &ws.tycons[ws.builtins.q_expr.0 as usize].kind else {
        unreachable!()
    };
    let mut ctx = LowerCtx::new(ModuleId(0), &[], &ws.builtins);
    ctx.attach_workspace(&ws);
    let span = Span::new(3, 9);
    for (index, variant) in union.variants.iter().enumerate() {
        let arity = match &variant.kind {
            ridge_types::VariantPayload::Nullary => 0,
            ridge_types::VariantPayload::Positional(fields) => fields.len(),
            ridge_types::VariantPayload::Record(_) => panic!("QExpr has positional payloads"),
        };
        let args = (0..arity).map(|_| unit_lit(&mut ctx, span)).collect();
        let node = qexpr_node(&mut ctx, &variant.name, args, span);
        let IrExpr::Construct {
            ctor,
            fields,
            span: actual_span,
            ..
        } = node
        else {
            panic!("expected constructor")
        };
        assert!(
            matches!(ctor, SymbolRef::Constructor { ctor_kind: ridge_ir::CtorKind::UnionVariant, owner_type: Some(owner), variant: actual, ref name }
            if owner == ws.builtins.q_expr && actual == u32::try_from(index).unwrap() && name == &variant.name)
        );
        assert_eq!(actual_span, span);
        assert_eq!(fields.len(), arity);
        for (index, (name, value)) in fields.iter().enumerate() {
            assert_eq!(name, &format!("${index}"));
            assert!(matches!(
                value,
                IrExpr::Lit {
                    value: IrLit::Unit,
                    ..
                }
            ));
        }
    }
    assert!(ctx.errors.is_empty(), "{:?}", ctx.errors);
}

#[test]
fn missing_qexpr_variant_reports_lowering_error_without_another_owner_fallback() {
    let mut ws = workspace();
    schema(&mut ws).variants.retain(|v| v.name != "QLitInt");
    let TyConKind::Union(other) = &mut ws.tycons[ws.builtins.ordering.0 as usize].kind else {
        unreachable!()
    };
    other.variants[0].name = "QLitInt".into();
    let mut ctx = LowerCtx::new(ModuleId(0), &[], &ws.builtins);
    ctx.attach_workspace(&ws);
    let span = Span::new(3, 9);
    let node = qexpr_node(&mut ctx, "QLitInt", vec![], span);
    assert!(matches!(
        node,
        IrExpr::Lit {
            value: IrLit::Unit,
            ..
        }
    ));
    assert!(
        matches!(&ctx.errors[..], [LowerError::InternalLoweringError { span: actual, message }]
        if *actual == span && message.contains("QLitInt"))
    );
}

#[test]
fn missing_qexpr_workspace_reports_lowering_error() {
    let mut ctx = LowerCtx::new(ModuleId(0), &[], crate::test_support::builtins());
    let span = Span::point(0);
    let node = qexpr_node(&mut ctx, "QLitInt", vec![], span);
    assert!(matches!(
        node,
        IrExpr::Lit {
            value: IrLit::Unit,
            ..
        }
    ));
    assert!(matches!(
        &ctx.errors[..],
        [LowerError::InternalLoweringError { .. }]
    ));
}
