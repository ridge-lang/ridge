//! Naming and recognition helpers for the data-layer `deriving` directives.
//!
//! For a record entity, `deriving (Table)` and `deriving (Schema)` generate the
//! data-layer scaffolding:
//!
//! ```text
//! pub type User = { id: Int, email: Text } deriving (Table, Schema)
//! -- Table generates a user-visible column mirror:
//! type UserCols  = { id: Column User Int, email: Column User Text }
//! let  userCols  : UserCols  = { id = .., email = .. }
//! let  userTable : Table User = { name = "users", columns = ["id", "email"] }
//! -- Schema synthesizes a HasSchema instance (like Row), reached by type:
//! instance HasSchema User =
//!     schemaOf _ = schema "User" "users" |> withColumn (mkColumn "id" "id" ..) |> ..
//! ```
//!
//! `Table` is a pure codegen directive: name resolution registers its names,
//! type checking synthesizes the mirror type and value schemes, and lowering
//! emits the values — three phases in different crates that must agree on the
//! exact generated names, so these helpers are that shared source of truth.
//! `Schema` is a typeclass derive: it synthesizes a `HasSchema` instance the way
//! `Row` does, so it registers no user-visible value name; the column-naming and
//! `DbType`-convention helpers below are what it shares with the `Table` path.

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

/// Name of the synthesized insert-companion type for an entity: `User` →
/// `UserInsert`.
///
/// `deriving (Schema)` synthesizes this record — the entity minus its
/// database-generated columns — for an entity that has any such column, so the
/// typed `insert`/`insertMany` accept a value that simply has no field for a
/// serial/identity or `DEFAULT` column. It is what the `InsertShape e`
/// projection reduces to. An entity with no generated columns gets no companion
/// (its insert shape is the entity itself).
#[must_use]
pub fn insert_companion_type_name(entity: &str) -> String {
    format!("{entity}Insert")
}

/// The name used inside a `deriving (...)` clause to request a schema descriptor.
pub const SCHEMA_DERIVE: &str = "Schema";

/// The class `deriving (Schema)` synthesizes an instance of: `HasSchema`.
///
/// The derive name (`Schema`) and the class name (`HasSchema`, from
/// `std.schema`) differ, so the derive resolves the class by this name rather
/// than by the clause spelling.
pub const SCHEMA_CLASS: &str = "HasSchema";

/// Whether a `deriving` clause requests a schema descriptor (`deriving (Schema)`).
#[must_use]
pub fn has_schema_derive(deriving: &[Ident]) -> bool {
    deriving.iter().any(|d| d.text == SCHEMA_DERIVE)
}

/// The name used inside a `deriving (...)` clause to request a row decoder.
pub const ROW_DERIVE: &str = "Row";

/// Whether a `deriving` clause requests a row decoder (`deriving (Row)`).
///
/// Unlike `Table`/`Schema`, `Row` is a real typeclass (declared in `std.sql`):
/// `deriving (Row)` synthesises a `Row` instance whose `fromRow` method reads a
/// database row — a `Map Text SqlValue` keyed by snake-cased column name — back
/// into the record. The column names are the same `column_sql_name` mapping the
/// `Table` derive uses, so a row and its table agree on column spelling.
#[must_use]
pub fn has_row_derive(deriving: &[Ident]) -> bool {
    deriving.iter().any(|d| d.text == ROW_DERIVE)
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
        PrimitiveType::Decimal => "Decimal",
        PrimitiveType::Uuid => "Uuid",
        PrimitiveType::Bytes => "Bytes",
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

/// Whether a field's declared type is `Int`, seen through parentheses and an
/// `Option` wrapper.
///
/// The schema convention's serial primary key is a non-null integer `id`, so the
/// generated-column and identity checks ask this directly. The column *type* is no
/// longer inferred here: `deriving (Schema)` reads it from the field's
/// `SqlType.dbType`, the one source shared by the value codec and the column type.
fn is_int_type(ty: &Type) -> bool {
    match ty {
        Type::Paren { inner, .. } => is_int_type(inner),
        Type::App { head, args, .. } if head.text == "Option" && !args.is_empty() => {
            is_int_type(&args[0])
        }
        Type::Primitive { name, .. } => matches!(name, PrimitiveType::Int),
        _ => false,
    }
}

/// Whether a record field is database-generated by the schema convention.
///
/// A generated column is one a typed insert drops because the database (or the
/// in-memory store) fills it, so it has no field in the entity's insert companion.
/// By the convention `deriving (Schema)` seeds, that is the conventional serial
/// primary key: a non-null integer field named `id` (an `Int`). A nullable or
/// non-integer `id` is still the
/// key but is supplied by the caller, and so is not generated. A hand-written
/// `HasSchema` instance states any further generated columns (`DEFAULT`s, other
/// identity columns); this is the single source of truth the schema derive, the
/// name reservation, and the companion synthesis all read.
#[must_use]
pub fn is_generated_field(field_name: &str, ty: &Type) -> bool {
    field_name == "id" && !is_optional_type(ty) && is_int_type(ty)
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
    fn detects_row_derive() {
        assert!(has_row_derive(&idents(&["Row"])));
        assert!(has_row_derive(&idents(&["Eq", "Row", "Table"])));
        assert!(!has_row_derive(&idents(&["Table", "Schema"])));
        assert!(!has_row_derive(&[]));
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
