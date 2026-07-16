//! Declaration-level checks that cannot be expressed as pure scope-walk rules.
//!
//! Houses the crate-path gate for `@ffi` (§5.5 / T003) and the reserved-name
//! gate that stops a user type or constructor from silently shadowing a prelude
//! builtin (R028).

use std::collections::HashSet;
use std::sync::OnceLock;

use ridge_ast::{Body, Constructor, Item, Module, TypeBody};

use crate::error::ResolveError;
use crate::imports::{prelude_resolutions, Binding};

// ── Crate-path gate (T003 FfiOutsideStdlib) ───────────────────────────────────

/// Emit `R022 FfiOutsideStdlib` for every `@ffi`-decorated `fn` found in
/// `module`, unless the module belongs to the standard library.
///
/// `@ffi` is a standard-library-only privilege. Whether a module is part of
/// the standard library is decided by the driver that builds it — the stdlib
/// build paths set `is_stdlib`, every user build leaves it `false` — rather
/// than inferred from the source path, which is unreliable: the stdlib is
/// compiled from sources copied into a throwaway workspace whose path carries
/// no stable marker, and a user directory could just as easily be named
/// `ridge-stdlib`.
///
/// # Errors emitted
///
/// - [`ResolveError::FfiOutsideStdlib`] (`R022`) — for each `@ffi` decl in a
///   module that is not part of the standard library.
#[must_use]
pub fn check_ffi_outside_stdlib(module: &Module, is_stdlib: bool) -> Vec<ResolveError> {
    if is_stdlib {
        return Vec::new();
    }

    let mut errors = Vec::new();
    for item in &module.items {
        if let Item::Fn(d) = item {
            if matches!(d.body, Body::Ffi { .. }) {
                errors.push(ResolveError::FfiOutsideStdlib { span: d.name.span });
            }
        }
    }
    errors
}

// ── Reserved prelude names (R028 ReservedName) ─────────────────────────────────

/// The type and constructor names the prelude keeps in scope in every module.
///
/// Derived once from [`prelude_resolutions`] so the set can never drift from the
/// bindings that actually cause the collision. Only the `StdlibSymbol` bindings
/// are reserved — those are the prelude's type names (`Option`, `Result`,
/// `Quote`, `Ordering`, `JsonValue`, `QExpr`) and constructors (`Ok`, `Some`,
/// `Less`, the `J*`/`Q*` families). The `ModuleAlias` bindings (`Int`, `Text`,
/// `List`, `Set`, …) are deliberately excluded: they name modules, not
/// constructors, so a user union variant called `Set` does not collide with them
/// and must keep compiling.
fn reserved_prelude_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        prelude_resolutions()
            .into_iter()
            .flat_map(|res| res.effective_bindings)
            .filter(|eb| matches!(eb.binding, Binding::StdlibSymbol { .. }))
            .map(|eb| eb.local_name)
            .collect()
    })
}

/// Emit `R028 ReservedName` for every user `type` or union constructor whose
/// name collides with an always-in-scope prelude builtin.
///
/// Without this gate a `type Quote = { … }` (or a union variant named `Ok`,
/// `Less`, …) silently shadows the builtin and later fails to unify with itself
/// — `expected Quote, got Quote` — pointing at a use site far from the cause.
/// Reporting the clash at the declaration turns that into one actionable error.
///
/// Stdlib modules legitimately declare these names (`Option`, `Ordering`, the
/// `Q*` family, …), so the check is skipped when `is_stdlib`.
///
/// # Errors emitted
///
/// - [`ResolveError::ReservedName`] (`R028`) — once per colliding type name and
///   once per colliding union constructor name.
#[must_use]
pub fn check_reserved_prelude_names(module: &Module, is_stdlib: bool) -> Vec<ResolveError> {
    if is_stdlib {
        return Vec::new();
    }

    let reserved = reserved_prelude_names();
    let mut errors = Vec::new();
    for item in &module.items {
        let Item::Type(d) = item else { continue };

        if reserved.contains(&d.name.text) {
            errors.push(ResolveError::ReservedName {
                name: d.name.text.clone(),
                kind: "type",
                span: d.name.span,
            });
        }

        if let TypeBody::Union(body) = &d.body {
            for ctor in &body.alternatives {
                let (Constructor::Positional { name, .. } | Constructor::Record { name, .. }) =
                    ctor;
                if reserved.contains(&name.text) {
                    errors.push(ResolveError::ReservedName {
                        name: name.text.clone(),
                        kind: "constructor",
                        span: name.span,
                    });
                }
            }
        }
    }
    errors
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_errors_when_module_is_stdlib() {
        // An empty stdlib module produces no errors.
        let module = ridge_ast::Module {
            items: vec![],
            doc: vec![],
            span: ridge_ast::Span::point(0),
        };
        let errs = check_ffi_outside_stdlib(&module, true);
        assert!(errs.is_empty());
    }

    /// Build a module containing a single `@ffi`-decorated `pub fn`.
    fn module_with_ffi() -> Module {
        use ridge_ast::{FnDecl, Ident, Span, Visibility};

        let span = Span::point(0);
        let decl = FnDecl {
            attrs: vec![],
            vis: Visibility::Pub,
            caps: vec![],
            name: Ident {
                text: "length".to_owned(),
                span,
            },
            params: vec![],
            ret: None,
            constraints: vec![],
            body: Body::Ffi {
                module: "erlang".to_owned(),
                name: "length".to_owned(),
                arity: 1,
            },
            span,
            doc: None,
        };
        Module {
            items: vec![Item::Fn(decl)],
            doc: vec![],
            span,
        }
    }

    #[test]
    fn r022_fires_for_ffi_in_user_module() {
        let module = module_with_ffi();
        let errs = check_ffi_outside_stdlib(&module, false);
        assert_eq!(errs.len(), 1);
        assert!(matches!(errs[0], ResolveError::FfiOutsideStdlib { .. }));
        assert_eq!(errs[0].code(), "R022");
    }

    #[test]
    fn no_r022_for_ffi_in_stdlib_module() {
        let module = module_with_ffi();
        let errs = check_ffi_outside_stdlib(&module, true);
        assert!(errs.is_empty(), "stdlib `@ffi` must be allowed: {errs:?}");
    }

    // ── R028 ReservedName ──────────────────────────────────────────────────────

    fn ident(text: &str) -> ridge_ast::Ident {
        ridge_ast::Ident {
            text: text.to_owned(),
            span: ridge_ast::Span::point(0),
        }
    }

    /// A module with one union `type <type_name> = <v0> | <v1> | …`, each variant
    /// nullary so no field types need constructing.
    fn module_with_union(type_name: &str, variants: &[&str]) -> Module {
        use ridge_ast::{Constructor, Span, TypeBody, TypeDecl, UnionTypeBody, Visibility};

        let span = Span::point(0);
        let alternatives = variants
            .iter()
            .map(|v| Constructor::Positional {
                name: ident(v),
                args: vec![],
                span,
            })
            .collect();
        let decl = TypeDecl {
            vis: Visibility::Pub,
            opaque: false,
            name: ident(type_name),
            params: vec![],
            body: TypeBody::Union(UnionTypeBody { alternatives, span }),
            deriving: vec![],
            span,
            doc: None,
        };
        Module {
            items: vec![Item::Type(decl)],
            doc: vec![],
            span,
        }
    }

    #[test]
    fn r028_fires_for_reserved_type_name() {
        // `Quote` is a prelude type name (backs the query DSL).
        let module = module_with_union("Quote", &["Placeholder"]);
        let errs = check_reserved_prelude_names(&module, false);
        assert_eq!(errs.len(), 1, "expected exactly one R028: {errs:?}");
        assert_eq!(errs[0].code(), "R028");
        assert!(matches!(
            &errs[0],
            ResolveError::ReservedName { name, kind, .. } if name == "Quote" && *kind == "type"
        ));
    }

    #[test]
    fn r028_fires_for_reserved_constructor_name() {
        // `Ok` is a prelude constructor (of `Result`); the type name is free.
        let module = module_with_union("Outcome", &["Ok", "Nope"]);
        let errs = check_reserved_prelude_names(&module, false);
        assert_eq!(errs.len(), 1, "expected exactly one R028: {errs:?}");
        assert_eq!(errs[0].code(), "R028");
        assert!(matches!(
            &errs[0],
            ResolveError::ReservedName { name, kind, .. } if name == "Ok" && *kind == "constructor"
        ));
    }

    #[test]
    fn no_r028_in_stdlib_module() {
        // The stdlib legitimately declares `Ordering = Less | Equal | Greater`.
        let module = module_with_union("Ordering", &["Less", "Equal", "Greater"]);
        let errs = check_reserved_prelude_names(&module, true);
        assert!(
            errs.is_empty(),
            "stdlib prelude decls must be allowed: {errs:?}"
        );
    }

    #[test]
    fn no_r028_for_import_gated_name() {
        // `Query` is a builtin the resolver leaves import-gated (not in the
        // prelude), so a user may shadow it; neither variant is reserved.
        let module = module_with_union("Query", &["Foo", "Bar"]);
        let errs = check_reserved_prelude_names(&module, false);
        assert!(
            errs.is_empty(),
            "import-gated names must not fire R028: {errs:?}"
        );
    }

    #[test]
    fn no_r028_for_module_alias_name_as_variant() {
        // `Set` is a prelude *module alias*, not a constructor, so a user union
        // variant called `Set` does not collide with it and must keep compiling.
        let module = module_with_union("Command", &["Get", "Set", "Del"]);
        let errs = check_reserved_prelude_names(&module, false);
        assert!(
            errs.is_empty(),
            "module-alias names must not be reserved as constructors: {errs:?}"
        );
    }
}
