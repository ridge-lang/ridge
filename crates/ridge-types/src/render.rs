//! Cross-compilation-stable rendering of types.
//!
//! Snapshot comparison and hot-reload version hashes need two independent
//! compilations of "the same type" to render to the same string, so nominal
//! types render by fully-qualified name (never by `TyConId`, which is
//! allocation-order dependent) and scheme variables render positionally
//! (`a`, `b`, …).

use crate::ty::RowTail;
use crate::tycon::TyConId;
use crate::{TyConDecl, TyVid, Type};

/// Lookup context shared by all renderers.
pub struct RenderCtx<'a> {
    /// The workspace `TyCon` table, indexed by `TyConId.0`.
    pub tycons: &'a [TyConDecl],
    /// Fully-qualified module names, indexed by `ModuleId.0`.
    pub module_fqns: &'a [String],
}

/// Renders a semantic type with stable, names-only output.
///
/// Named types use their defining module's FQN (`app.user.User`); primitives
/// and built-ins (no defining module) render bare (`Int`, `List<Int>`). Free
/// unification variables render as `_`.
#[must_use]
pub fn render_type(ctx: &RenderCtx<'_>, ty: &Type) -> String {
    render_type_vars(ctx, ty, &[])
}

/// Single recursive walk behind [`render_type`]: a `Type::Var` found in
/// `vars` renders as its positional letter, anything else as `_`.
#[must_use]
pub fn render_type_vars(ctx: &RenderCtx<'_>, ty: &Type, vars: &[TyVid]) -> String {
    match ty {
        Type::Var(v) => vars
            .iter()
            .position(|b| b == v)
            .map_or_else(|| "_".to_string(), var_letter),
        Type::Con(id, args) => {
            let name = tycon_name(ctx, *id);
            if args.is_empty() {
                name
            } else {
                let inner: Vec<String> = args
                    .iter()
                    .map(|a| render_type_vars(ctx, a, vars))
                    .collect();
                format!("{name}<{}>", inner.join(", "))
            }
        }
        // Capability rows are intentionally not rendered: a top-level fn's
        // concrete caps travel in the snapshot's `caps_bits`, and variable
        // rows have no stable name.
        Type::Fn { params, ret, .. } => {
            let ps: Vec<String> = params
                .iter()
                .map(|p| render_type_vars(ctx, p, vars))
                .collect();
            format!(
                "fn({}) -> {}",
                ps.join(", "),
                render_type_vars(ctx, ret, vars)
            )
        }
        Type::Tuple(elems) => {
            let es: Vec<String> = elems
                .iter()
                .map(|e| render_type_vars(ctx, e, vars))
                .collect();
            format!("({})", es.join(", "))
        }
        Type::Record { fields, tail } => {
            let mut fs: Vec<String> = fields
                .iter()
                .map(|(n, t)| format!("{n}: {}", render_type_vars(ctx, t, vars)))
                .collect();
            if matches!(tail, RowTail::Open(_)) {
                fs.push("| _".to_string());
            }
            format!("{{{}}}", fs.join(", "))
        }
        Type::Alias { name, .. } => tycon_name(ctx, *name),
        // `Type::Error` — and any future variant (`Type` is non-exhaustive) —
        // renders as unknown.
        _ => "?".to_string(),
    }
}

/// Positional scheme-variable name: `a`…`z`, then `t26`, `t27`, …
fn var_letter(index: usize) -> String {
    if let Ok(i) = u32::try_from(index) {
        if i < 26 {
            if let Some(c) = char::from_u32(u32::from(b'a') + i) {
                return c.to_string();
            }
        }
    }
    format!("t{index}")
}

fn tycon_name(ctx: &RenderCtx<'_>, id: TyConId) -> String {
    let Some(decl) = ctx.tycons.get(id.0 as usize) else {
        return "?".to_string();
    };
    decl.def_module_raw.map_or_else(
        || decl.name.clone(),
        |m| {
            ctx.module_fqns
                .get(m as usize)
                .map_or_else(|| decl.name.clone(), |fqn| format!("{fqn}.{}", decl.name))
        },
    )
}

#[cfg(test)]
mod tests {
    use crate::capability_set::CapabilitySet;
    use crate::ty::CapRow;
    use crate::tycon::{RecordSchema, TyConId, TyConKind};
    use crate::{TyVid, Type};

    use super::*;

    fn tycon(
        id: u32,
        name: &str,
        arity: u32,
        kind: TyConKind,
        def_module: Option<u32>,
    ) -> TyConDecl {
        TyConDecl {
            id: TyConId(id),
            name: name.to_string(),
            arity,
            kind,
            def_span: None,
            def_module_raw: def_module,
            opaque: false,
            is_anon: false,
        }
    }

    /// Table: 0=Int, 1=Text, 2=Bool, 3=List, 4=app.user.User, 5=app.user.MyInt (alias).
    fn fixture() -> (Vec<TyConDecl>, Vec<String>) {
        let tycons = vec![
            tycon(0, "Int", 0, TyConKind::Primitive, None),
            tycon(1, "Text", 0, TyConKind::Primitive, None),
            tycon(2, "Bool", 0, TyConKind::Primitive, None),
            tycon(3, "List", 1, TyConKind::Builtin, None),
            tycon(
                4,
                "User",
                0,
                TyConKind::Record(RecordSchema::new(vec![], vec![])),
                Some(0),
            ),
            tycon(
                5,
                "MyInt",
                0,
                TyConKind::Alias {
                    params: vec![],
                    body: Type::Con(TyConId(0), vec![]),
                },
                Some(0),
            ),
        ];
        (tycons, vec!["app.user".to_string()])
    }

    fn con(id: u32, args: Vec<Type>) -> Type {
        Type::Con(TyConId(id), args)
    }

    #[test]
    fn renders_primitive_and_app() {
        let (tycons, fqns) = fixture();
        let ctx = RenderCtx {
            tycons: &tycons,
            module_fqns: &fqns,
        };
        assert_eq!(
            render_type(&ctx, &con(3, vec![con(0, vec![])])),
            "List<Int>"
        );
        assert_eq!(render_type(&ctx, &con(4, vec![])), "app.user.User");
    }

    #[test]
    fn renders_fn_tuple_record_alias_error() {
        let (tycons, fqns) = fixture();
        let ctx = RenderCtx {
            tycons: &tycons,
            module_fqns: &fqns,
        };
        let fn_ty = Type::Fn {
            params: vec![con(0, vec![]), con(1, vec![])],
            ret: Box::new(con(2, vec![])),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        assert_eq!(render_type(&ctx, &fn_ty), "fn(Int, Text) -> Bool");
        assert_eq!(
            render_type(&ctx, &Type::Tuple(vec![con(0, vec![]), con(1, vec![])])),
            "(Int, Text)"
        );
        let rec = Type::record(
            vec![
                ("a".to_string(), con(0, vec![])),
                ("b".to_string(), con(1, vec![])),
            ],
            crate::ty::RowTail::Closed,
        );
        assert_eq!(render_type(&ctx, &rec), "{a: Int, b: Text}");
        let alias = Type::Alias {
            name: TyConId(5),
            body: Box::new(con(0, vec![])),
        };
        assert_eq!(render_type(&ctx, &alias), "app.user.MyInt");
        assert_eq!(render_type(&ctx, &Type::Error), "?");
    }

    #[test]
    fn renders_quantified_vars_as_letters() {
        let (tycons, fqns) = fixture();
        let ctx = RenderCtx {
            tycons: &tycons,
            module_fqns: &fqns,
        };
        let fn_ty = Type::Fn {
            params: vec![Type::Var(TyVid(0))],
            ret: Box::new(Type::Var(TyVid(1))),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let vars = vec![TyVid(0), TyVid(1)];
        assert_eq!(render_type_vars(&ctx, &fn_ty, &vars), "fn(a) -> b");
        // A var outside `vars` renders as an opaque unknown.
        assert_eq!(render_type_vars(&ctx, &Type::Var(TyVid(9)), &vars), "_");
    }

    #[test]
    fn renders_record_schema_fields_via_record_con() {
        // A nominal record renders by name even though its schema has fields.
        let (tycons, fqns) = fixture();
        let ctx = RenderCtx {
            tycons: &tycons,
            module_fqns: &fqns,
        };
        let user = con(4, vec![]);
        assert_eq!(render_type(&ctx, &user), "app.user.User");
    }
}
