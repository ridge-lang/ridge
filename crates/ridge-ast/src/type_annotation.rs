//! Source type spellings retained as metadata after syntax is lowered.

use std::fmt;

use crate::{Capability, Type};

/// A complete type annotation in Ridge syntax, independent of inference ids.
///
/// Constructed only from syntax: aliases and named types keep the author's
/// spelling, and nested applications retain their parentheses. This is metadata,
/// not a substitute for the checked semantic type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAnnotation(String);

impl From<&Type> for TypeAnnotation {
    fn from(ty: &Type) -> Self {
        Self(SourceType(ty).to_string())
    }
}

impl fmt::Display for TypeAnnotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

struct SourceType<'a>(&'a Type);

impl fmt::Display for SourceType<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Type::Primitive { name, .. } => f.write_str(name.name()),
            Type::Named { name, .. } | Type::Var { name, .. } => f.write_str(&name.text),
            Type::App { head, args, .. } => {
                f.write_str(&head.text)?;
                for arg in args {
                    f.write_str(" ")?;
                    atom(f, arg)?;
                }
                Ok(())
            }
            Type::List { elem, .. } => {
                f.write_str("List ")?;
                atom(f, elem)
            }
            Type::Paren { inner, .. } => write!(f, "({})", Self(inner)),
            Type::Tuple { elems, .. } => {
                f.write_str("(")?;
                separated(f, elems, ", ")?;
                f.write_str(")")
            }
            Type::Fn { fn_ty, .. } => {
                if fn_ty.caps.is_empty() && fn_ty.params.len() == 1 {
                    atom(f, &fn_ty.params[0])?;
                } else {
                    f.write_str("fn")?;
                    for cap in &fn_ty.caps {
                        write!(f, " {}", capability_name(*cap))?;
                    }
                    if fn_ty.params.is_empty() {
                        f.write_str(" ()")?;
                    }
                    for param in &fn_ty.params {
                        f.write_str(" ")?;
                        atom(f, param)?;
                    }
                }
                write!(f, " -> {}", Self(&fn_ty.ret))
            }
            Type::Record { fields, tail, .. } => {
                f.write_str("{ ")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {}", field.name.text, Self(&field.ty))?;
                }
                if let Some(tail) = tail {
                    if !fields.is_empty() {
                        f.write_str(" ")?;
                    }
                    write!(f, "| {}", tail.text)?;
                }
                f.write_str(" }")
            }
        }
    }
}

fn atom(f: &mut fmt::Formatter<'_>, ty: &Type) -> fmt::Result {
    match ty {
        Type::App { .. } | Type::List { .. } | Type::Fn { .. } => {
            write!(f, "({})", SourceType(ty))
        }
        other => write!(f, "{}", SourceType(other)),
    }
}

fn separated(f: &mut fmt::Formatter<'_>, types: &[Type], separator: &str) -> fmt::Result {
    for (i, ty) in types.iter().enumerate() {
        if i > 0 {
            f.write_str(separator)?;
        }
        write!(f, "{}", SourceType(ty))?;
    }
    Ok(())
}

const fn capability_name(cap: Capability) -> &'static str {
    match cap {
        Capability::Io => "io",
        Capability::Fs => "fs",
        Capability::Net => "net",
        Capability::Time => "time",
        Capability::Random => "random",
        Capability::Env => "env",
        Capability::Proc => "proc",
        Capability::Spawn => "spawn",
        Capability::Ffi => "ffi",
        Capability::Db => "db",
    }
}
