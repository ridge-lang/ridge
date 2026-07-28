//! Cross-compilation-stable rendering of types.
//!
//! Snapshot comparison needs two independent compilations of "the same type"
//! to render to the same string, so nominal types render by fully-qualified
//! name (never by `TyConId`, which is allocation-order dependent) and scheme
//! variables render positionally (`a`, `b`, …).

use ridge_ast::Type as AstType;
use ridge_types::ty::RowTail;
use ridge_types::tycon::TyConId;
use ridge_types::{Scheme, TyConDecl, TyVid, Type};

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

/// Renders a scheme, mapping quantified vars positionally to `a`, `b`, …
#[must_use]
pub fn render_scheme(ctx: &RenderCtx<'_>, scheme: &Scheme) -> String {
    render_type_vars(ctx, &scheme.ty, &scheme.vars)
}

/// Single recursive walk behind [`render_type`] and [`render_scheme`]: a
/// `Type::Var` found in `vars` renders as its positional letter, anything
/// else as `_`.
pub(crate) fn render_type_vars(ctx: &RenderCtx<'_>, ty: &Type, vars: &[TyVid]) -> String {
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
    use ridge_types::{Scheme, TyVid, Type};

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
            ridge_types::ty::RowTail::Closed,
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
