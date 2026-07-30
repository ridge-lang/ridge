//! Record version metadata for runtime values (`__ridge_v` tags).
//!
//! Every non-anonymous record value carries a `__ridge_v` entry in its
//! runtime map: `{ModuleFQN, RecordName, LayoutVersion}`. The version is a
//! hash of the record's field layout (names + types, in declared order), so
//! two builds emit the same version iff the shape is unchanged. The runtime
//! code loader reads the tag to decide when a live value needs migration;
//! anonymous inline records stay untagged (they have no stable identity).

// pub(crate) on items in a pub(crate) module is redundant per clippy; kept
// for explicitness, matching the convention in `module.rs`.
#![allow(clippy::redundant_pub_crate)]

use ridge_types::{TyConDecl, TyConId, TyConKind};
use rustc_hash::FxHashMap;

/// Runtime identity of a record type: where it was declared, its name, and a
/// version derived from its field layout. Same layout ⇒ same version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordMeta {
    /// Beam name of the declaring module (e.g. `"ridge_blog_engine_models"`);
    /// unique and stable, it serves as the module identity in the tag.
    pub fqn: String,
    /// Source-level record name (e.g. `"User"`).
    pub name: String,
    /// The shared 64-bit shape hash; changes whenever a field is added,
    /// removed, renamed, retyped, or reordered.
    pub version: u64,
}

/// Build the `TyConId → RecordMeta` table for a workspace.
///
/// `beam_names` and `module_fqns` are both indexed by `ModuleId.0`. The
/// version is the shared 64-bit shape hash over `(name, rendered-type)`
/// pairs — the same function and the same rendering `ridge-reload` uses for
/// snapshot history, so a beam tag and a snapshot hash can never diverge.
/// Records without a declaring module (built-ins) and anonymous inline
/// records are skipped.
pub(crate) fn build_record_meta(
    tycons: &[TyConDecl],
    beam_names: &[String],
    module_fqns: &[String],
) -> FxHashMap<TyConId, RecordMeta> {
    let ctx = ridge_types::render::RenderCtx {
        tycons,
        module_fqns,
    };
    let mut out = FxHashMap::default();
    for decl in tycons {
        if decl.is_anon {
            continue;
        }
        let TyConKind::Record(schema) = &decl.kind else {
            continue;
        };
        let Some(raw) = decl.def_module_raw else {
            continue;
        };
        // No beam name ⇒ no stable module identity (built-ins, hand-built
        // workspaces in unit tests) ⇒ untagged, mirroring the built-in skip.
        let Some(fqn) = beam_names.get(raw as usize).cloned() else {
            continue;
        };
        let shape: Vec<(String, String)> = schema
            .record_fields()
            .iter()
            .map(|f| {
                (
                    f.name.clone(),
                    ridge_types::render::render_type(&ctx, &f.ty),
                )
            })
            .collect();
        out.insert(
            decl.id,
            RecordMeta {
                fqn,
                name: decl.name.clone(),
                version: ridge_types::shape::shape_hash(&shape),
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_types::{RecordField, RecordSchema, TyConDecl, TyConKind, Type};

    fn record_decl(name: &str, id: u32, module: u32, fields: &[(&str, Type)]) -> TyConDecl {
        TyConDecl {
            id: TyConId(id),
            name: name.to_owned(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                fields
                    .iter()
                    .map(|(n, t)| RecordField {
                        name: (*n).to_owned(),
                        ty: t.clone(),
                    })
                    .collect(),
            )),
            def_span: None,
            def_module_raw: Some(module),
            opaque: false,
            is_anon: false,
        }
    }

    fn prim_decl(id: u32, name: &str) -> TyConDecl {
        TyConDecl {
            id: TyConId(id),
            name: name.to_owned(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        }
    }

    fn text() -> Type {
        Type::Con(TyConId(100), vec![])
    }

    fn int() -> Type {
        Type::Con(TyConId(101), vec![])
    }

    /// The record decl plus the primitives its field types reference, laid out
    /// so `tycons[id.0]` resolves — rendered field types embed the tycon
    /// names, so `Text` and `Int` must render differently for the retype test.
    fn tycons_with(record: TyConDecl) -> Vec<TyConDecl> {
        let mut v = vec![record];
        while v.len() < 100 {
            #[allow(clippy::cast_possible_truncation)]
            v.push(prim_decl(v.len() as u32, "_pad"));
        }
        v.push(prim_decl(100, "Text"));
        v.push(prim_decl(101, "Int"));
        v
    }

    fn beams() -> Vec<String> {
        vec!["ridge_app_models".to_owned()]
    }

    fn fqns() -> Vec<String> {
        vec!["app.models".to_owned()]
    }

    #[test]
    fn version_is_stable_for_same_layout() {
        let m1 = build_record_meta(
            &tycons_with(record_decl("User", 0, 0, &[("name", text())])),
            &beams(),
            &fqns(),
        );
        let m2 = build_record_meta(
            &tycons_with(record_decl("User", 0, 0, &[("name", text())])),
            &beams(),
            &fqns(),
        );
        assert_eq!(m1[&TyConId(0)], m2[&TyConId(0)]);
        assert_eq!(m1[&TyConId(0)].fqn, "ridge_app_models");
        assert_eq!(m1[&TyConId(0)].name, "User");
    }

    #[test]
    fn version_changes_when_layout_changes() {
        let a = build_record_meta(
            &tycons_with(record_decl("User", 0, 0, &[("name", text())])),
            &beams(),
            &fqns(),
        );
        let b = build_record_meta(
            &tycons_with(record_decl(
                "User",
                0,
                0,
                &[("name", text()), ("role", text())],
            )),
            &beams(),
            &fqns(),
        );
        assert_ne!(a[&TyConId(0)].version, b[&TyConId(0)].version);
    }

    #[test]
    fn version_changes_when_field_retyped() {
        let a = build_record_meta(
            &tycons_with(record_decl("User", 0, 0, &[("age", text())])),
            &beams(),
            &fqns(),
        );
        let b = build_record_meta(
            &tycons_with(record_decl("User", 0, 0, &[("age", int())])),
            &beams(),
            &fqns(),
        );
        assert_ne!(a[&TyConId(0)].version, b[&TyConId(0)].version);
    }

    #[test]
    fn rendered_names_make_versions_cross_module_stable() {
        // Two records with the same name and layout in different modules get
        // the same hash — but a field whose type is another record renders
        // with that record's module FQN, so a layout change in a referenced
        // module shifts the version even when this module is untouched.
        let field_ty = Type::Con(TyConId(0), vec![]);
        let a = build_record_meta(
            &tycons_with(record_decl("Outer", 0, 0, &[("inner", field_ty.clone())])),
            &beams(),
            &fqns(),
        );
        let other_fqns = vec!["app.other".to_owned()];
        let b = build_record_meta(
            &tycons_with(record_decl("Outer", 0, 0, &[("inner", field_ty)])),
            &beams(),
            &other_fqns,
        );
        assert_ne!(
            a[&TyConId(0)].version,
            b[&TyConId(0)].version,
            "field type renders as app.models.Outer vs app.other.Outer"
        );
    }

    #[test]
    fn anon_records_get_no_meta() {
        let mut d = record_decl("{x}", 0, 0, &[("x", int())]);
        d.is_anon = true;
        assert!(build_record_meta(&tycons_with(d), &beams(), &fqns()).is_empty());
    }

    #[test]
    fn builtin_records_without_module_get_no_meta() {
        let mut d = record_decl("Built", 0, 0, &[("x", int())]);
        d.def_module_raw = None;
        assert!(build_record_meta(&tycons_with(d), &beams(), &fqns()).is_empty());
    }

    #[test]
    fn records_without_beam_name_get_no_meta() {
        // No beam name ⇒ no stable module identity ⇒ untagged.
        let d = record_decl("User", 0, 0, &[("x", int())]);
        assert!(build_record_meta(&tycons_with(d), &[], &[]).is_empty());
    }

    #[test]
    fn unions_get_no_meta() {
        let mut d = record_decl("Maybe", 0, 0, &[]);
        d.kind = TyConKind::Primitive;
        assert!(build_record_meta(&tycons_with(d), &beams(), &fqns()).is_empty());
    }
}
