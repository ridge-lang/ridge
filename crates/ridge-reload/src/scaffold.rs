//! `migrate` hook scaffolds for shape changes.

use crate::snapshot::{FieldSnap, StateSnap};

/// How one field of the NEW shape is filled from the OLD shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldAction {
    /// Field kept as-is: `name: old.name`.
    Keep { name: String },
    /// Field renamed: `to: old.from`.
    Rename { from: String, to: String },
    /// Added or retyped field the compiler cannot fill: renders as `???`
    /// unless the caller supplies an auto-fill expression.
    Hole { name: String, ty: String },
}

/// Maps old record fields to new record fields.
///
/// A rename is detected when exactly one field disappears and exactly one
/// appears with the same rendered type at the same declared position;
/// anything else treats additions (and retypes) as holes.
#[must_use]
pub fn field_plan(old: &[FieldSnap], new: &[FieldSnap]) -> Vec<FieldAction> {
    let old_pairs: Vec<(&str, &str)> = old
        .iter()
        .map(|f| (f.name.as_str(), f.ty.as_str()))
        .collect();
    let new_pairs: Vec<(&str, &str)> = new
        .iter()
        .map(|f| (f.name.as_str(), f.ty.as_str()))
        .collect();
    plan_impl(&old_pairs, &new_pairs)
}

/// State-field variant of [`field_plan`].
///
/// Purely additive changes whose new fields all have defaults never reach
/// this function (the caller classifies those as auto-migrations), so every
/// addition here becomes a hole.
#[must_use]
pub fn state_plan(old: &[StateSnap], new: &[StateSnap]) -> Vec<FieldAction> {
    let old_pairs: Vec<(&str, &str)> = old
        .iter()
        .map(|f| (f.name.as_str(), f.ty.as_str()))
        .collect();
    let new_pairs: Vec<(&str, &str)> = new
        .iter()
        .map(|f| (f.name.as_str(), f.ty.as_str()))
        .collect();
    plan_impl(&old_pairs, &new_pairs)
}

/// Shared plan over `(name, rendered-type)` pairs, in NEW declaration order.
fn plan_impl(old: &[(&str, &str)], new: &[(&str, &str)]) -> Vec<FieldAction> {
    let removed: Vec<(usize, &str, &str)> = old
        .iter()
        .enumerate()
        .filter(|(_, (n, _))| !new.iter().any(|(nn, _)| nn == n))
        .map(|(i, (n, t))| (i, *n, *t))
        .collect();
    let added: Vec<(usize, &str, &str)> = new
        .iter()
        .enumerate()
        .filter(|(_, (n, _))| !old.iter().any(|(on, _)| on == n))
        .map(|(i, (n, t))| (i, *n, *t))
        .collect();

    // Pure rename heuristic: exactly one removal and one addition, same
    // rendered type, same declared position.
    let rename = match (removed.as_slice(), added.as_slice()) {
        ([(oi, on, ot)], [(ni, nn, nt)]) if oi == ni && ot == nt => Some((*on, *nn)),
        _ => None,
    };

    new.iter()
        .map(|(n, t)| {
            if let Some((from, to)) = rename {
                if *n == to {
                    return FieldAction::Rename {
                        from: from.to_string(),
                        to: to.to_string(),
                    };
                }
            }
            match old.iter().find(|(on, _)| on == n) {
                Some((_, ot)) if ot == t => FieldAction::Keep {
                    name: (*n).to_string(),
                },
                // Added or retyped: the compiler cannot invent the value.
                _ => FieldAction::Hole {
                    name: (*n).to_string(),
                    ty: (*t).to_string(),
                },
            }
        })
        .collect()
}

/// Full record scaffold, including the `@version` line and `do ... end`.
///
/// `auto_fills` maps field name → source expression for compiler-derived
/// values; those fields render their expression instead of `???`. Field
/// order follows the NEW declaration order.
#[must_use]
pub fn record_migrate(
    module_fqn: &str,
    name: &str,
    old_version: u32,
    new_fields: &[FieldSnap],
    plan: &[FieldAction],
    auto_fills: &[(String, String)],
) -> String {
    let fields: Vec<String> = new_fields
        .iter()
        .map(|f| format!("{}: {}", f.name, f.ty))
        .collect();
    let new_version = old_version.saturating_add(1);
    format!(
        "// module: {module_fqn}\ntype {name} @version({new_version}) = {} do\n{}\nend\n",
        brace_list(&fields),
        migrate_block(name, old_version, plan, auto_fills, 4),
    )
}

/// Actor migrate hook body (goes at the level of `init`/`terminate`).
#[must_use]
pub fn actor_migrate(
    name: &str,
    old_version: u32,
    plan: &[FieldAction],
    auto_fills: &[(String, String)],
) -> String {
    migrate_block(name, old_version, plan, auto_fills, 0)
}

/// The `migrate (old: Name@N) -> Name = { ... }` block, indented `indent`
/// spaces (the literal body sits one level deeper).
fn migrate_block(
    name: &str,
    old_version: u32,
    plan: &[FieldAction],
    auto_fills: &[(String, String)],
    indent: usize,
) -> String {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 4);
    let entries: Vec<String> = plan
        .iter()
        .map(|a| match a {
            FieldAction::Keep { name } => format!("{name}: old.{name}"),
            FieldAction::Rename { from, to } => format!("{to}: old.{from}"),
            FieldAction::Hole { name, .. } => {
                auto_fills.iter().find(|(n, _)| n == name).map_or_else(
                    || format!("{name}: ???"),
                    |(_, expr)| format!("{name}: {expr}"),
                )
            }
        })
        .collect();
    format!(
        "{pad}migrate (old: {name}@{old_version}) -> {name} =\n{inner}{}",
        brace_list(&entries)
    )
}

/// `{ a, b, c }` — or `{}` when empty.
fn brace_list(items: &[String]) -> String {
    if items.is_empty() {
        "{}".to_string()
    } else {
        format!("{{ {} }}", items.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Vec<FieldSnap> {
        pairs
            .iter()
            .map(|(n, t)| FieldSnap {
                name: (*n).to_string(),
                ty: (*t).to_string(),
            })
            .collect()
    }

    fn states(pairs: &[(&str, &str, bool)]) -> Vec<StateSnap> {
        pairs
            .iter()
            .map(|(n, t, d)| StateSnap {
                name: (*n).to_string(),
                ty: (*t).to_string(),
                has_default: *d,
            })
            .collect()
    }

    #[test]
    fn plan_keeps_unchanged_fields() {
        let old = fields(&[("name", "Text"), ("age", "Int")]);
        let plan = field_plan(&old, &old);
        assert_eq!(
            plan,
            vec![
                FieldAction::Keep {
                    name: "name".to_string()
                },
                FieldAction::Keep {
                    name: "age".to_string()
                },
            ]
        );
    }

    #[test]
    fn plan_detects_rename_by_type_and_position() {
        let old = fields(&[("name", "Text"), ("age", "Int")]);
        let new = fields(&[("full_name", "Text"), ("age", "Int")]);
        let plan = field_plan(&old, &new);
        assert_eq!(
            plan,
            vec![
                FieldAction::Rename {
                    from: "name".to_string(),
                    to: "full_name".to_string()
                },
                FieldAction::Keep {
                    name: "age".to_string()
                },
            ]
        );
    }

    #[test]
    fn plan_marks_new_fields_as_holes() {
        let old = fields(&[("name", "Text")]);
        let new = fields(&[("name", "Text"), ("email", "Text")]);
        let plan = field_plan(&old, &new);
        assert_eq!(
            plan,
            vec![
                FieldAction::Keep {
                    name: "name".to_string()
                },
                FieldAction::Hole {
                    name: "email".to_string(),
                    ty: "Text".to_string()
                },
            ]
        );
        // A retype is a hole too: the old value no longer fits.
        let retyped = fields(&[("name", "Int")]);
        assert_eq!(
            field_plan(&old, &retyped),
            vec![FieldAction::Hole {
                name: "name".to_string(),
                ty: "Int".to_string()
            }]
        );
    }

    #[test]
    fn state_plan_mirrors_field_plan() {
        let old = states(&[("count", "Int", true)]);
        let new = states(&[("total", "Int", true)]);
        assert_eq!(
            state_plan(&old, &new),
            vec![FieldAction::Rename {
                from: "count".to_string(),
                to: "total".to_string()
            }]
        );
    }

    #[test]
    fn record_scaffold_text_with_hole() {
        let old = fields(&[("name", "Text"), ("email", "Text")]);
        let new = fields(&[("name", "Text"), ("email", "Text"), ("role", "Role")]);
        let plan = field_plan(&old, &new);
        let text = record_migrate("app.user", "User", 1, &new, &plan, &[]);
        assert_eq!(
            text,
            "// module: app.user\ntype User @version(2) = { name: Text, email: Text, role: Role } do\n    migrate (old: User@1) -> User =\n        { name: old.name, email: old.email, role: ??? }\nend\n"
        );
    }

    #[test]
    fn record_scaffold_text_rename_complete() {
        let old = fields(&[("name", "Text"), ("age", "Int")]);
        let new = fields(&[("full_name", "Text"), ("age", "Int")]);
        let plan = field_plan(&old, &new);
        let text = record_migrate("app.user", "User", 1, &new, &plan, &[]);
        assert!(!text.contains("???"), "rename scaffold is complete: {text}");
        assert!(text.contains("full_name: old.name"));
    }

    #[test]
    fn actor_scaffold_text() {
        let plan = vec![
            FieldAction::Keep {
                name: "count".to_string(),
            },
            FieldAction::Hole {
                name: "step".to_string(),
                ty: "Int".to_string(),
            },
        ];
        let fills = vec![("step".to_string(), "1".to_string())];
        let text = actor_migrate("Counter", 1, &plan, &fills);
        assert_eq!(
            text,
            "migrate (old: Counter@1) -> Counter =\n    { count: old.count, step: 1 }"
        );
    }
}
