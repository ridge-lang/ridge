//! Record version metadata for runtime values (`__ridge_v` tags).
//!
//! Every non-anonymous record value carries a `__ridge_v` entry in its
//! runtime map: `{ModuleFQN, RecordName, LayoutVersion}`. The version is a
//! hash of the record's field layout (names + types, in declared order), so
//! two builds emit the same version iff the shape is unchanged. The runtime
//! code loader reads the tag to decide when a live value needs migration;
//! anonymous inline records stay untagged (they have no stable identity).

use ridge_types::{TyConDecl, TyConId, TyConKind};
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::Hasher;

/// Runtime identity of a record type: where it was declared, its name, and a
/// version derived from its field layout. Same layout ⇒ same version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordMeta {
    /// Beam name of the declaring module (e.g. `"ridge_blog_engine_models"`);
    /// unique and stable, it serves as the module identity in the tag.
    pub fqn: String,
    /// Source-level record name (e.g. `"User"`).
    pub name: String,
    /// Layout hash; changes whenever a field is added, removed, renamed,
    /// retyped, or reordered.
    pub version: u32,
}

/// Build the `TyConId → RecordMeta` table for a workspace.
///
/// `beam_names` is indexed by `ModuleId.0` and carries each module's stable
/// beam name, which doubles as the module identity in the tag (it derives
/// 1:1 from the FQN). Records without a declaring module (built-ins) and
/// anonymous inline records are skipped.
pub(crate) fn build_record_meta(
    tycons: &[TyConDecl],
    beam_names: &[String],
) -> FxHashMap<TyConId, RecordMeta> {
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
        let mut h = FxHasher::default();
        for f in schema.record_fields() {
            h.write(f.name.as_bytes());
            h.write(b":");
            // Debug formatting of `Type` is structural (includes constructor
            // ids and argument types), which is what the version must capture.
            h.write(format!("{:?}", f.ty).as_bytes());
            h.write(b";");
        }
        #[allow(clippy::cast_possible_truncation)]
        out.insert(
            decl.id,
            RecordMeta {
                fqn,
                name: decl.name.clone(),
                version: h.finish() as u32,
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

    fn text() -> Type {
        Type::Con(TyConId(100), vec![])
    }

    fn int() -> Type {
        Type::Con(TyConId(101), vec![])
    }

    fn beams() -> Vec<String> {
        vec!["ridge_app_models".to_owned()]
    }

    #[test]
    fn version_is_stable_for_same_layout() {
        let decls = vec![record_decl("User", 0, 0, &[("name", text())])];
        let m1 = build_record_meta(&decls, &beams());
        let m2 = build_record_meta(&decls, &beams());
        assert_eq!(m1[&TyConId(0)], m2[&TyConId(0)]);
        assert_eq!(m1[&TyConId(0)].fqn, "ridge_app_models");
        assert_eq!(m1[&TyConId(0)].name, "User");
    }

    #[test]
    fn version_changes_when_layout_changes() {
        let a = build_record_meta(&[record_decl("User", 0, 0, &[("name", text())])], &beams());
        let b = build_record_meta(
            &[record_decl(
                "User",
                0,
                0,
                &[("name", text()), ("role", text())],
            )],
            &beams(),
        );
        assert_ne!(a[&TyConId(0)].version, b[&TyConId(0)].version);
    }

    #[test]
    fn version_changes_when_field_retyped() {
        let a = build_record_meta(&[record_decl("User", 0, 0, &[("age", text())])], &beams());
        let b = build_record_meta(&[record_decl("User", 0, 0, &[("age", int())])], &beams());
        assert_ne!(a[&TyConId(0)].version, b[&TyConId(0)].version);
    }

    #[test]
    fn anon_records_get_no_meta() {
        let mut d = record_decl("{x}", 0, 0, &[("x", int())]);
        d.is_anon = true;
        assert!(build_record_meta(&[d], &beams()).is_empty());
    }

    #[test]
    fn builtin_records_without_module_get_no_meta() {
        let mut d = record_decl("Built", 0, 0, &[("x", int())]);
        d.def_module_raw = None;
        assert!(build_record_meta(&[d], &beams()).is_empty());
    }

    #[test]
    fn records_without_beam_name_get_no_meta() {
        // No beam name ⇒ no stable module identity ⇒ untagged.
        let d = record_decl("User", 0, 0, &[("x", int())]);
        assert!(build_record_meta(&[d], &[]).is_empty());
    }

    #[test]
    fn unions_get_no_meta() {
        let mut d = record_decl("Maybe", 0, 0, &[]);
        d.kind = TyConKind::Primitive;
        assert!(build_record_meta(&[d], &beams()).is_empty());
    }
}
