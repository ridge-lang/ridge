//! The intrinsic conversion dictionary must implement its advertised contract.

use ridge_ast::{InstanceDecl, Span};
use ridge_types::{BuiltinTyCons, CapRow, CapabilitySet, Constraint, RigidId, Subst, TyVid, Type};
use rustc_hash::FxHashMap;

use crate::class_env::ClassTable;
use crate::ctx::{InferCtx, RigidInfo};
use crate::error::{TypeDesc, TypeError};
use crate::tycon_collect::ast_type_to_ridge_type;

/// Reject a missing, duplicate or differently named conversion method.
pub fn check_method_set(ctx: &mut InferCtx, decl: &InstanceDecl) {
    if decl.methods.len() != 1 || decl.methods[0].name.text != "errorFrom" {
        mismatch(
            ctx,
            decl.span,
            TypeDesc::Text("one method named errorFrom".into()),
            TypeDesc::Text(
                decl.methods
                    .iter()
                    .map(|m| m.name.text.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        );
    }
}

/// Hold generic parameters constant while checking the method and its body.
pub fn prepare_method(
    ctx: &mut InferCtx,
    builtins: &BuiltinTyCons,
    decl: &InstanceDecl,
    classes: &ClassTable,
    vars: &FxHashMap<&str, TyVid>,
    params: &mut [Type],
    ret: &mut Type,
) {
    let givens: Vec<_> = decl
        .constraints
        .iter()
        .filter_map(|given| {
            let class = classes.id_by_name(&given.class.text)?;
            let tys: Option<smallvec::SmallVec<[TyVid; 1]>> = given
                .ty_vars
                .iter()
                .map(|v| vars.get(v.text.as_str()).copied())
                .collect();
            Some(Constraint::new(class, tys?))
        })
        .collect();
    let mut subst = Subst::empty();
    for (name, vid) in vars {
        let id = RigidId(vid.0);
        let promises: Vec<_> = givens
            .iter()
            .filter(|c| c.tys.contains(vid))
            .cloned()
            .collect();
        ctx.rigids.insert(
            id,
            RigidInfo {
                decl: "ErrorFrom instance".into(),
                span: decl.span,
                givens: promises.iter().map(|c| c.class).collect(),
                given_constraints: promises,
                complete: true,
            },
        );
        subst.ty.insert(
            *vid,
            Type::Rigid {
                id,
                name: (*name).into(),
            },
        );
    }
    for param in params.iter_mut() {
        *param = subst.apply_to_ty(param);
    }
    *ret = subst.apply_to_ty(ret);
    let [source, target] = decl.head.as_slice() else {
        return;
    };
    let names = ctx.user_tycon_names.clone();
    let source = subst.apply_to_ty(&ast_type_to_ridge_type(builtins, ctx, source, &names, vars));
    let target = subst.apply_to_ty(&ast_type_to_ridge_type(builtins, ctx, target, &names, vars));
    let expected = Type::Fn {
        params: vec![source],
        ret: Box::new(target),
        caps: CapRow::Concrete(CapabilitySet::PURE),
    };
    let found = Type::Fn {
        params: params.to_vec(),
        ret: Box::new(ret.clone()),
        caps: CapRow::Concrete(CapabilitySet::PURE),
    };
    if !crate::unify::try_unify(ctx, &expected, &found) {
        mismatch(ctx, decl.span, TypeDesc::ty(expected), TypeDesc::ty(found));
    }
}

fn mismatch(ctx: &mut InferCtx, span: Span, expected: TypeDesc, found: TypeDesc) {
    ctx.errors.push(TypeError::TypeMismatch {
        expected, found, span,
        hint: Some("an ErrorFrom instance must define pure errorFrom from its source type to its destination type".into()),
    });
}
