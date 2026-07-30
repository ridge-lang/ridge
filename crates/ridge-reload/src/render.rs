//! Snapshot-facing renderers.
//!
//! The cross-compilation-stable semantic type renderer lives in
//! [`ridge_types::render`] (shared with codegen's version-hash inputs); this
//! module keeps the scheme wrapper plus the AST-level renderer used for actor
//! state fields, which never get a semantic `Type` outside inference.

use ridge_ast::Type as AstType;
use ridge_types::Scheme;

pub use ridge_types::render::{render_type, render_type_vars, RenderCtx};

/// Renders a scheme, mapping quantified vars positionally to `a`, `b`, …
#[must_use]
pub fn render_scheme(ctx: &RenderCtx<'_>, scheme: &Scheme) -> String {
    render_type_vars(ctx, &scheme.ty, &scheme.vars)
}

/// Renders an AST-level type, structurally and span-free.
///
/// Used for actor state fields, which never get a semantic `Type` outside
/// inference. Mirrors the shape rules of the fingerprint renderer in the
/// driver's incremental checker, with human-readable separators.
#[must_use]
pub fn render_ast_type(ty: &AstType) -> String {
    match ty {
        AstType::Primitive { name, .. } => primitive_name(*name).to_string(),
        AstType::Named { name, .. } | AstType::Var { name, .. } => name.text.clone(),
        AstType::App { head, args, .. } => {
            let rendered: Vec<String> = args.iter().map(render_ast_type).collect();
            format!("{}<{}>", head.text, rendered.join(", "))
        }
        AstType::Tuple { elems, .. } => {
            let rendered: Vec<String> = elems.iter().map(render_ast_type).collect();
            format!("({})", rendered.join(", "))
        }
        AstType::List { elem, .. } => format!("[{}]", render_ast_type(elem)),
        AstType::Paren { inner, .. } => render_ast_type(inner),
        AstType::Fn { fn_ty, .. } => {
            let rendered: Vec<String> = fn_ty.params.iter().map(render_ast_type).collect();
            let caps = if fn_ty.caps.is_empty() {
                String::new()
            } else {
                let names: Vec<&str> = fn_ty.caps.iter().map(|c| capability_name(*c)).collect();
                format!("({})", names.join(", "))
            };
            format!(
                "fn{caps}({}) -> {}",
                rendered.join(", "),
                render_ast_type(&fn_ty.ret)
            )
        }
        AstType::Record { fields, tail, .. } => {
            let mut rendered: Vec<String> = fields
                .iter()
                .map(|f| format!("{}: {}", f.name.text, render_ast_type(&f.ty)))
                .collect();
            rendered.sort();
            if let Some(t) = tail {
                rendered.push(format!("| {}", t.text));
            }
            format!("{{{}}}", rendered.join(", "))
        }
    }
}

const fn primitive_name(p: ridge_ast::PrimitiveType) -> &'static str {
    use ridge_ast::PrimitiveType as P;
    match p {
        P::Int => "Int",
        P::Float => "Float",
        P::Bool => "Bool",
        P::Text => "Text",
        P::Unit => "Unit",
        P::Timestamp => "Timestamp",
        P::Decimal => "Decimal",
        P::Uuid => "Uuid",
        P::Bytes => "Bytes",
        P::Date => "Date",
        P::Time => "Time",
    }
}

pub(crate) const fn capability_name(c: ridge_ast::Capability) -> &'static str {
    use ridge_ast::Capability as C;
    match c {
        C::Io => "io",
        C::Fs => "fs",
        C::Net => "net",
        C::Time => "time",
        C::Random => "random",
        C::Env => "env",
        C::Proc => "proc",
        C::Spawn => "spawn",
        C::Ffi => "ffi",
        C::Db => "db",
    }
}

#[cfg(test)]
mod tests {
    use ridge_types::capability_set::CapabilitySet;
    use ridge_types::ty::CapRow;
    use ridge_types::tycon::{RecordSchema, TyConId, TyConKind};
    use ridge_types::{Scheme, TyConDecl, TyVid, Type};

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

    #[test]
    fn renders_scheme_vars_as_letters() {
        let (tycons, fqns) = fixture();
        let ctx = RenderCtx {
            tycons: &tycons,
            module_fqns: &fqns,
        };
        let scheme = Scheme {
            vars: vec![TyVid(0), TyVid(1)],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Var(TyVid(0))],
                ret: Box::new(Type::Var(TyVid(1))),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
        };
        assert_eq!(render_scheme(&ctx, &scheme), "fn(a) -> b");
        // A var outside scheme.vars renders as an opaque unknown.
        let free = Scheme::mono(Type::Var(TyVid(9)));
        assert_eq!(render_scheme(&ctx, &free), "_");
    }
}
