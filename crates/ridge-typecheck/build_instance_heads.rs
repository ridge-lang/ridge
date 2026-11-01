//! Retain source instance heads without parsing library bodies during checking.

use std::path::{Path, PathBuf};

use ridge_ast::Type;

fn files(root: &Path, result: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.is_dir() {
            files(&path, result)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "ridge")
            && !path.to_string_lossy().ends_with(".test.ridge")
        {
            result.push(path);
        }
    }
    Ok(())
}

fn ident(name: &str) -> String {
    format!("ident({name:?})")
}

fn types(values: &[Type]) -> String {
    format!(
        "vec![{}]",
        values.iter().map(ty).collect::<Vec<_>>().join(",")
    )
}

fn ty(value: &Type) -> String {
    let body = match value {
        Type::Primitive { name, .. } => {
            format!("Primitive {{name: ridge_ast::PrimitiveType::{name:?}, span: S}}")
        }
        Type::Named { name, .. } => format!("Named {{name: {}, span: S}}", ident(&name.text)),
        Type::Var { name, .. } => format!("Var {{name: {}, span: S}}", ident(&name.text)),
        Type::App { head, args, .. } => {
            format!(
                "App {{head: {}, args: {}, span: S}}",
                ident(&head.text),
                types(args)
            )
        }
        Type::Paren { inner, .. } => return ty(inner),
        Type::Tuple { elems, .. } => format!("Tuple {{elems: {}, span: S}}", types(elems)),
        Type::List { elem, .. } => format!("List {{elem: Box::new({}), span: S}}", ty(elem)),
        Type::Fn { fn_ty, .. } => {
            let caps = fn_ty
                .caps
                .iter()
                .map(|cap| format!("ridge_ast::Capability::{cap:?}"))
                .collect::<Vec<_>>()
                .join(",");
            format!("Fn {{fn_ty: ridge_ast::FnType {{caps: vec![{caps}], params: {}, ret: Box::new({}), span: S}}, span: S}}",
                types(&fn_ty.params), ty(&fn_ty.ret))
        }
        Type::Record { fields, tail, .. } => {
            let fields = fields
                .iter()
                .map(|field| {
                    format!(
                        "ridge_ast::RecordTypeField {{name: {}, ty: {}, span: S}}",
                        ident(&field.name.text),
                        ty(&field.ty)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let tail = tail.as_ref().map_or_else(
                || "None".into(),
                |tail| format!("Some({})", ident(&tail.text)),
            );
            format!("Record {{fields: vec![{fields}], tail: {tail}, span: S}}")
        }
    };
    format!("ridge_ast::Type::{body}")
}

pub fn generate(root: &Path, output: &Path) -> Result<(), String> {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut sources = Vec::new();
    files(root, &mut sources)?;
    sources.sort();
    let mut rows = Vec::new();
    for path in sources {
        println!("cargo:rerun-if-changed={}", path.display());
        let source = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let parsed = ridge_parser::parse_source(&source);
        if !parsed.errors.is_empty() || !parsed.lex_errors.is_empty() {
            return Err(format!(
                "Cannot generate instance heads from {}: {:?} {:?}",
                path.display(),
                parsed.errors,
                parsed.lex_errors
            ));
        }
        for item in parsed.module.items {
            if let ridge_ast::Item::InstanceDecl(instance) = item {
                let constraints = instance.constraints.iter().map(|constraint| {
                    let variables = constraint.ty_vars.iter().map(|name| ident(&name.text)).collect::<Vec<_>>().join(",");
                    format!("ridge_ast::ClassConstraint {{class: {}, ty_vars: vec![{variables}], span: S}}", ident(&constraint.class.text))
                }).collect::<Vec<_>>().join(",");
                rows.push(format!(
                    "({:?}, {}, vec![{constraints}])",
                    instance.class.text,
                    types(&instance.head)
                ));
            }
        }
    }
    std::fs::write(
        output,
        format!(
            "// Generated from standard-library instance declarations.\nvec![{}]\n",
            rows.join(",\n")
        ),
    )
    .map_err(|error| error.to_string())
}
