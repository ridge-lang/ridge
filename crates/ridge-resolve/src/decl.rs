//! Declaration-level checks that cannot be expressed as pure scope-walk rules.
//!
//! Currently houses the crate-path gate for `@ffi` (§5.5 / T003).

use ridge_ast::{Body, Item, Module};

use crate::error::ResolveError;

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
}
