//! Naming and recognition helpers for the data-layer `deriving` directives.
//!
//! `deriving (Table)` and `deriving (Schema)` are code-generation directives,
//! not typeclasses. For a record entity they generate user-visible top-level
//! declarations:
//!
//! ```text
//! pub type User = { id: Int, email: Text } deriving (Table, Schema)
//! -- Table generates:
//! type UserCols  = { id: Column User Int, email: Column User Text }
//! let  userCols  : UserCols  = { id = .., email = .. }
//! let  userTable : Table User = { name = "users", columns = ["id", "email"] }
//! -- Schema generates:
//! let  userSchema : Schema = { name = "User", table = "users", fields = [..] }
//! ```
//!
//! Three compiler phases each generate part of this: name resolution registers
//! the names, type checking synthesizes the type and the value schemes, and
//! lowering emits the values. They run in different crates and must agree on the
//! exact generated names, so these helpers are the single source of truth.

use crate::ident::Ident;
use crate::ty::Type;
use crate::PrimitiveType;

/// The name used inside a `deriving (...)` clause to request column codegen.
pub const TABLE_DERIVE: &str = "Table";

/// Whether a `deriving` clause requests column codegen (`deriving (Table)`).
#[must_use]
pub fn has_table_derive(deriving: &[Ident]) -> bool {
    deriving.iter().any(|d| d.text == TABLE_DERIVE)
}

/// Name of the generated column-mirror type for an entity: `User` → `UserCols`.
#[must_use]
pub fn mirror_type_name(entity: &str) -> String {
    format!("{entity}Cols")
}

/// Name of the generated column-mirror value for an entity: `User` → `userCols`.
#[must_use]
pub fn mirror_value_name(entity: &str) -> String {
    format!("{}Cols", lower_first(entity))
}

/// Name of the generated table-metadata value for an entity: `User` → `userTable`.
#[must_use]
pub fn table_value_name(entity: &str) -> String {
    format!("{}Table", lower_first(entity))
}

/// The name used inside a `deriving (...)` clause to request a schema descriptor.
pub const SCHEMA_DERIVE: &str = "Schema";

/// Whether a `deriving` clause requests a schema descriptor (`deriving (Schema)`).
#[must_use]
pub fn has_schema_derive(deriving: &[Ident]) -> bool {
    deriving.iter().any(|d| d.text == SCHEMA_DERIVE)
}

/// Name of the generated schema-descriptor value for an entity: `User` →
/// `userSchema`.
#[must_use]
pub fn schema_value_name(entity: &str) -> String {
    format!("{}Schema", lower_first(entity))
}

/// Render a field's declared type to its `FieldSchema` type tag.
///
/// The tag is a readable, machine-parseable spelling of the type that the
/// introspection layer maps to an `OpenAPI`/JSON type or a SQL column type:
/// `Int` → `"Int"`, `Id User` → `"Id User"`, `Option Text` → `"Option Text"`,
/// `Map Text (Id User)` keeps the nested application parenthesised. Function and
/// inline-record fields are rare in entities and collapse to `"Fn"` / `"Record"`.
#[must_use]
pub fn render_type_tag(ty: &Type) -> String {
    match ty {
        Type::Primitive { name, .. } => primitive_tag(*name).to_owned(),
        Type::Named { name, .. } | Type::Var { name, .. } => name.text.clone(),
        Type::App { head, args, .. } => {
            let mut out = head.text.clone();
            for a in args {
                out.push(' ');
                out.push_str(&render_type_atom(a));
            }
            out
        }
        Type::List { elem, .. } => format!("List {}", render_type_atom(elem)),
        Type::Paren { inner, .. } => render_type_tag(inner),
        Type::Tuple { elems, .. } => {
            let parts: Vec<String> = elems.iter().map(render_type_tag).collect();
            format!("({})", parts.join(", "))
        }
        Type::Fn { .. } => "Fn".to_owned(),
        Type::Record { .. } => "Record".to_owned(),
    }
}

/// Render a type as an *argument* of an application: a multi-word application or
/// function type is parenthesised so the parent tag stays unambiguous.
fn render_type_atom(ty: &Type) -> String {
    match ty {
        Type::App { args, .. } if !args.is_empty() => format!("({})", render_type_tag(ty)),
        Type::Fn { .. } => format!("({})", render_type_tag(ty)),
        other => render_type_tag(other),
    }
}

/// The `UPPER_IDENT` spelling of a primitive type, for the descriptor type tag.
const fn primitive_tag(p: PrimitiveType) -> &'static str {
    match p {
        PrimitiveType::Int => "Int",
        PrimitiveType::Float => "Float",
        PrimitiveType::Bool => "Bool",
        PrimitiveType::Text => "Text",
        PrimitiveType::Unit => "Unit",
        PrimitiveType::Timestamp => "Timestamp",
    }
}

/// Whether a field's declared type is optional (`Option a`), which the
/// descriptor records as a nullable / not-required column.
#[must_use]
pub fn is_optional_type(ty: &Type) -> bool {
    match ty {
        Type::Paren { inner, .. } => is_optional_type(inner),
        Type::Named { name, .. } => name.text == "Option",
        Type::App { head, .. } => head.text == "Option",
        _ => false,
    }
}

/// The SQL table name for an entity: `User` → `users`, `BlogPost` → `blog_posts`.
///
/// The default is the snake-cased plural of the type name. A future `@table`
/// attribute will override this per the data-layer naming convention.
#[must_use]
pub fn table_sql_name(entity: &str) -> String {
    pluralize(&to_snake_case(entity))
}

/// The SQL column name for a record field: `createdAt` → `created_at`.
///
/// A future `@column` attribute will override this per field.
#[must_use]
pub fn column_sql_name(field: &str) -> String {
    to_snake_case(field)
}

// ── String helpers ─────────────────────────────────────────────────────────

/// Lowercase only the first character, leaving the rest untouched
/// (`BlogPost` → `blogPost`).
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_lowercase().to_string() + chars.as_str()
    })
}

/// Convert a `camelCase` or `PascalCase` identifier to `snake_case` by inserting
/// an underscore before every uppercase letter that follows a lowercase letter
/// or digit, then lowercasing. Runs of uppercase (acronyms) are left joined;
/// a dedicated `@column` override exists for the rare cases that need control.
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev_lower_or_digit = false;
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower_or_digit {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower_or_digit = false;
        } else {
            out.push(c);
            prev_lower_or_digit = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    out
}

/// Naive English pluralization good enough for default table names:
/// `y`→`ies` (after a consonant), `s/x/z/ch/sh`→`es`, otherwise `+s`.
fn pluralize(s: &str) -> String {
    if let Some(stem) = s.strip_suffix('y') {
        if stem.chars().last().is_some_and(|c| !is_vowel(c)) {
            return format!("{stem}ies");
        }
    }
    let needs_es = s.ends_with('s')
        || s.ends_with('x')
        || s.ends_with('z')
        || s.ends_with("ch")
        || s.ends_with("sh");
    if needs_es {
        format!("{s}es")
    } else {
        format!("{s}s")
    }
}

const fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    fn idents(names: &[&str]) -> Vec<Ident> {
        names
            .iter()
            .map(|n| Ident::new(*n, Span::point(0)))
            .collect()
    }

    #[test]
    fn detects_table_derive() {
        assert!(has_table_derive(&idents(&["Eq", "Table", "Ord"])));
        assert!(!has_table_derive(&idents(&["Eq", "ToText"])));
        assert!(!has_table_derive(&[]));
    }

    #[test]
    fn generated_names() {
        assert_eq!(mirror_type_name("User"), "UserCols");
        assert_eq!(mirror_value_name("User"), "userCols");
        assert_eq!(table_value_name("User"), "userTable");

        assert_eq!(mirror_type_name("BlogPost"), "BlogPostCols");
        assert_eq!(mirror_value_name("BlogPost"), "blogPostCols");
        assert_eq!(table_value_name("BlogPost"), "blogPostTable");
    }

    #[test]
    fn table_names_pluralize_and_snake() {
        assert_eq!(table_sql_name("User"), "users");
        assert_eq!(table_sql_name("Post"), "posts");
        assert_eq!(table_sql_name("BlogPost"), "blog_posts");
        assert_eq!(table_sql_name("Category"), "categories");
        assert_eq!(table_sql_name("Day"), "days");
        assert_eq!(table_sql_name("Box"), "boxes");
        assert_eq!(table_sql_name("Dish"), "dishes");
        assert_eq!(table_sql_name("Church"), "churches");
    }

    #[test]
    fn column_names_snake_case() {
        assert_eq!(column_sql_name("id"), "id");
        assert_eq!(column_sql_name("email"), "email");
        assert_eq!(column_sql_name("createdAt"), "created_at");
        assert_eq!(column_sql_name("authorId"), "author_id");
        assert_eq!(column_sql_name("isPublished2"), "is_published2");
    }

    // ── Schema descriptor helpers ────────────────────────────────────────────

    fn ident(name: &str) -> Ident {
        Ident::new(name, Span::point(0))
    }
    fn prim(p: PrimitiveType) -> Type {
        Type::Primitive {
            name: p,
            span: Span::point(0),
        }
    }
    fn named(name: &str) -> Type {
        Type::Named {
            name: ident(name),
            span: Span::point(0),
        }
    }
    fn app(head: &str, args: Vec<Type>) -> Type {
        Type::App {
            head: ident(head),
            args,
            span: Span::point(0),
        }
    }

    #[test]
    fn detects_schema_derive() {
        assert!(has_schema_derive(&idents(&["Schema"])));
        assert!(has_schema_derive(&idents(&["Table", "Schema", "Eq"])));
        assert!(!has_schema_derive(&idents(&["Table", "Eq"])));
        assert!(!has_schema_derive(&[]));
    }

    #[test]
    fn schema_value_names() {
        assert_eq!(schema_value_name("User"), "userSchema");
        assert_eq!(schema_value_name("BlogPost"), "blogPostSchema");
    }

    #[test]
    fn type_tags_render() {
        assert_eq!(render_type_tag(&prim(PrimitiveType::Int)), "Int");
        assert_eq!(render_type_tag(&prim(PrimitiveType::Text)), "Text");
        assert_eq!(render_type_tag(&named("Email")), "Email");
        assert_eq!(render_type_tag(&app("Id", vec![named("User")])), "Id User");
        assert_eq!(
            render_type_tag(&app("Option", vec![prim(PrimitiveType::Text)])),
            "Option Text"
        );
        assert_eq!(
            render_type_tag(&Type::List {
                elem: Box::new(named("Post")),
                span: Span::point(0),
            }),
            "List Post"
        );
        // Nested applications parenthesise the argument.
        assert_eq!(
            render_type_tag(&app("Option", vec![app("Id", vec![named("User")])])),
            "Option (Id User)"
        );
        assert_eq!(
            render_type_tag(&Type::Tuple {
                elems: vec![prim(PrimitiveType::Int), prim(PrimitiveType::Text)],
                span: Span::point(0),
            }),
            "(Int, Text)"
        );
    }

    #[test]
    fn optional_detection() {
        assert!(is_optional_type(&app(
            "Option",
            vec![prim(PrimitiveType::Text)]
        )));
        assert!(is_optional_type(&Type::Paren {
            inner: Box::new(app("Option", vec![named("User")])),
            span: Span::point(0),
        }));
        assert!(!is_optional_type(&prim(PrimitiveType::Int)));
        assert!(!is_optional_type(&app("List", vec![named("Post")])));
    }
}
