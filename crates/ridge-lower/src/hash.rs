//! Typed primitive hashing operations; runtime representation belongs to codegen.

use ridge_ast::Span;
use ridge_ir::{CtorKind, IrExpr, IrParam, StdlibKind, SymbolRef};
use ridge_types::{BuiltinTyCons, CapabilitySet, ClassId, TyConId, Type, HASH_CLASS};

use crate::ctx::LowerCtx;

fn operation(b: &BuiltinTyCons, ty: TyConId) -> Option<&'static str> {
    [
        (b.int, "hashInt"),
        (b.bool, "hashBool"),
        (b.text, "hashText"),
        (b.unit, "hashUnit"),
        (b.ordering, "hashOrdering"),
        (b.bytes, "hashBytes"),
        (b.uuid, "hashUuid"),
        (b.timestamp, "hashTimestamp"),
        (b.date, "hashDate"),
        (b.time, "hashTime"),
        (b.decimal, "hashDecimal"),
        (b.handle, "hashHandle"),
        (b.supervisor, "hashSupervisor"),
        (b.monitor, "hashMonitor"),
    ]
    .into_iter()
    .find_map(|(id, name)| (id == ty).then_some(name))
}

pub fn dictionary(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    tycon: TyConId,
    span: Span,
) -> Option<IrExpr> {
    if class != HASH_CLASS {
        return None;
    }
    let name = operation(ctx.builtins, tycon)?;
    Some(primitive_dictionary(ctx, name, span))
}

pub fn is_primitive(builtins: &BuiltinTyCons, class: ClassId, tycon: TyConId) -> bool {
    class == HASH_CLASS && operation(builtins, tycon).is_some()
}

/// Evidence for compiler-generated maps whose keys are statically Text.
fn primitive_dictionary(ctx: &mut LowerCtx<'_>, name: &str, span: Span) -> IrExpr {
    let value_name = ctx.fresh_local("__hash_value");
    let state_name = ctx.fresh_local("__hash_state");
    let value = local(ctx, &value_name, span);
    let state = local(ctx, &state_name, span);
    let target = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.op".into(),
            name: name.into(),
            kind: StdlibKind::Function,
        },
        span,
    };
    let body = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(target),
        args: vec![value, state],
        span,
    };
    let method = IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params: vec![
            IrParam {
                name: value_name,
                // Dictionary value parameters are erased, as in prelude Eq.
                ty: Type::Error,
                span,
            },
            IrParam {
                name: state_name,
                ty: Type::Con(ctx.builtins.hasher, vec![]),
                span,
            },
        ],
        body: Box::new(body),
        caps: CapabilitySet::PURE,
        span,
    };
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: None,
            name: "$primitive_hash".into(),
            variant: 0,
        },
        fields: vec![("hash".into(), method)],
        span,
    }
}

fn local(ctx: &mut LowerCtx<'_>, name: &str, span: Span) -> IrExpr {
    IrExpr::Local {
        id: ctx.fresh_id(None),
        name: name.into(),
        span,
    }
}
