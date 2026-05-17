//! Resolved visibility kinds — the post-resolution form of `ridge_ast::Visibility`.
//!
//! The mapping rule (§4.3 step 2) interprets a leading-underscore name as
//! file-private **only when paired with `Visibility::Private`**. A `pub` or
//! `pub(internal)` declaration is never demoted by the underscore prefix —
//! the explicit keyword overrides the convention.
//!
//! See [`resolve_visibility`] for the full rule and the justification for the
//! `Pub + _` choice.

use ridge_ast::Visibility;

// ── ResolvedVisibility ────────────────────────────────────────────────────────

/// Visibility resolved against the underscore-prefix and `pub(internal)` rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolvedVisibility {
    /// `_foo` or `_Type` (with no explicit `pub` modifier) — file-private.
    FilePrivate,
    /// No modifier — project-private (default).
    ProjectPrivate,
    /// `pub(internal)` — namespace-private.
    NamespaceInternal,
    /// `pub` — exportable (subject to manifest `[project.exports].public`).
    Pub,
}

// ── resolve_visibility ────────────────────────────────────────────────────────

/// Map a parser `Visibility` + the declared name to a `ResolvedVisibility`.
///
/// # Rule (§4.3 step 2)
///
/// | `vis`            | `name` starts with `_` | result              |
/// |------------------|------------------------|---------------------|
/// | `Private`        | yes                    | `FilePrivate`       |
/// | `Private`        | no                     | `ProjectPrivate`    |
/// | `PubInternal`    | any                    | `NamespaceInternal` |
/// | `Pub`            | any                    | `Pub`               |
///
/// **`Pub` + `_` stays `Pub`**: the spec rule only mentions
/// `Visibility::Private + starts_with('_') → FilePrivate`.  An explicit `pub`
/// keyword takes priority; the underscore is a hint, not an override.
/// `PubInternal` similarly ignores the underscore prefix.
#[must_use]
pub fn resolve_visibility(vis: Visibility, name: &str) -> ResolvedVisibility {
    match vis {
        Visibility::Private if name.starts_with('_') => ResolvedVisibility::FilePrivate,
        Visibility::Private => ResolvedVisibility::ProjectPrivate,
        Visibility::PubInternal => ResolvedVisibility::NamespaceInternal,
        Visibility::Pub => ResolvedVisibility::Pub,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pub_plain_name_is_pub() {
        assert_eq!(
            resolve_visibility(Visibility::Pub, "foo"),
            ResolvedVisibility::Pub
        );
    }

    #[test]
    fn pub_underscore_name_stays_pub() {
        // Explicit `pub` wins over the underscore hint.
        assert_eq!(
            resolve_visibility(Visibility::Pub, "_foo"),
            ResolvedVisibility::Pub
        );
    }

    #[test]
    fn private_plain_name_is_project_private() {
        assert_eq!(
            resolve_visibility(Visibility::Private, "foo"),
            ResolvedVisibility::ProjectPrivate
        );
    }

    #[test]
    fn private_underscore_name_is_file_private() {
        assert_eq!(
            resolve_visibility(Visibility::Private, "_helper"),
            ResolvedVisibility::FilePrivate
        );
    }

    #[test]
    fn pub_internal_plain_name_is_namespace_internal() {
        assert_eq!(
            resolve_visibility(Visibility::PubInternal, "foo"),
            ResolvedVisibility::NamespaceInternal
        );
    }

    #[test]
    fn pub_internal_underscore_name_is_namespace_internal() {
        // Underscore does NOT demote pub(internal) to FilePrivate.
        assert_eq!(
            resolve_visibility(Visibility::PubInternal, "_foo"),
            ResolvedVisibility::NamespaceInternal
        );
    }
}
