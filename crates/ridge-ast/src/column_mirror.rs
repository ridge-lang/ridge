//! Naming and recognition helpers for the `deriving (Table)` column mirror.
//!
//! `deriving (Table)` is a code-generation directive, not a typeclass. For a
//! record entity it generates a column-mirror type, a column-mirror value, and
//! a table-metadata value:
//!
//! ```text
//! pub type User = { id: Int, email: Text } deriving (Table)
//! -- generates:
//! type UserCols  = { id: Column User Int, email: Column User Text }
//! let  userCols  : UserCols  = { id = .., email = .. }
//! let  userTable : Table User = { name = "users", columns = ["id", "email"] }
//! ```
//!
//! Three compiler phases each generate part of this: name resolution registers
//! the names, type checking synthesizes the type and the value schemes, and
//! lowering emits the values. They run in different crates and must agree on the
//! exact generated names, so these helpers are the single source of truth.

use crate::ident::Ident;

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
}
