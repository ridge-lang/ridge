//! Declaration-level checks that cannot be expressed as pure scope-walk rules.
//!
//! Currently houses the crate-path gate for `@ffi` (§5.5 / T003).

use std::path::Path;

use ridge_ast::{Body, Item, Module};

use crate::error::ResolveError;

// ── Crate-path gate (T003 FfiOutsideStdlib) ───────────────────────────────────

/// Emit `R022 FfiOutsideStdlib` for every `@ffi`-decorated `pub fn` found in
/// `module` when `source_path` is not inside the `ridge-stdlib` crate.
///
/// The stdlib crate is identified by the presence of `"ridge-stdlib"` in the
/// canonical source path (separator-agnostic via [`Path::components`]).
///
/// # Errors emitted
///
/// - [`ResolveError::FfiOutsideStdlib`] (`R022`) — for each `@ffi` decl whose
///   file is outside `crates/ridge-stdlib/`.
#[must_use]
pub fn check_ffi_outside_stdlib(module: &Module, source_path: &Path) -> Vec<ResolveError> {
    if is_stdlib_path(source_path) {
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

/// Return `true` when `path` is inside the `ridge-stdlib` Rust crate.
///
/// Detection criterion: any path component equals `"ridge-stdlib"`.
fn is_stdlib_path(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().to_str().is_some_and(|s| s == "ridge-stdlib"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_stdlib_path_returns_true_for_stdlib() {
        assert!(is_stdlib_path(Path::new(
            "/workspace/crates/ridge-stdlib/src/io.ridge"
        )));
    }

    #[test]
    fn is_stdlib_path_returns_false_for_user_code() {
        assert!(!is_stdlib_path(Path::new(
            "/workspace/apps/myapp/src/main.ridge"
        )));
    }

    #[test]
    fn no_errors_when_source_is_stdlib() {
        // An empty module inside the stdlib path produces no errors.
        let module = ridge_ast::Module {
            items: vec![],
            doc: vec![],
            span: ridge_ast::Span::point(0),
        };
        let errs = check_ffi_outside_stdlib(
            &module,
            Path::new("/workspace/crates/ridge-stdlib/src/io.ridge"),
        );
        assert!(errs.is_empty());
    }
}
