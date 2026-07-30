//! `migrate` hook checking.
//!
//! A `migrate (old: User@1) -> User = …` hook is a pure fn `Old -> New`:
//! the `old` parameter's type is the anonymous record reconstructed from the
//! snapshot-history entry for that ordinal; the body is inferred and unified
//! against the current shape (a record's `Type::Con`, or an actor's state
//! record). The versioned reference never touches `Type` resolution — it is
//! interpreted here, against the injected [`VersionHistory`].
//!
//! Rendered field types in the history (`"Int"`, `"app.user.Role"`,
//! `"List<Int>"`, …) are re-resolved by a small recursive-descent reader.
//! Names that no longer resolve (a field type whose module is not imported
//! anymore) degrade to a fresh unification variable: the hash, not the
//! re-resolution, carries identity — a degraded field type can only weaken
//! the body's checking, never reject a valid program.

use std::sync::Arc;

use ridge_ast::{ActorMember, Item, MigrateDecl};
use rustc_hash::FxHashMap;

use ridge_types::history::VersionHistory;
use ridge_types::ty::{CapRow, RowTail};
use ridge_types::tycon::{TyConArena, TyConKind};
use ridge_types::{BuiltinTyCons, CapabilitySet, TyConId, Type};

use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::infer::infer_expr;

/// History + name-resolution context for one module's migrate pass.
pub struct MigrateHistoryCtx<'a> {
    /// The injected previous-build history (empty on a fresh build).
    pub history: &'a VersionHistory,
    /// Fully-qualified module names, indexed by `ModuleId.0`.
    pub module_fqns: &'a [String],
    /// Per-module actual type-name → `TyConId` tables of the modules checked
    /// so far (producers), indexed by `ModuleId.0`.
    pub checked_tycon_names: &'a [FxHashMap<String, TyConId>],
}

/// Check every `migrate` hook in the module: type-level `do … end` sections
/// and actor `migrate` members. Runs after actor-body checking so the arena
/// and schemes are final.
pub fn typecheck_migrate_hooks(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    ast: &Arc<ridge_ast::Module>,
    arena: &TyConArena,
    hctx: &MigrateHistoryCtx<'_>,
    module_fqn: &str,
) {
    for item in &ast.items {
        match item {
            Item::Type(decl) => {
                if decl.migrates.is_empty() {
                    continue;
                }
                let Some(&tycon) = ctx.user_tycon_names.get(&decl.name.text) else {
                    continue;
                };
                let TyConKind::Record(schema) = &arena.get(tycon).kind else {
                    continue;
                };
                let new_ty =
                    Type::Con(tycon, schema.params.iter().map(|v| Type::Var(*v)).collect());
                let mut seen_ordinals: Vec<u32> = Vec::new();
                for m in &decl.migrates {
                    check_one_hook(
                        ctx,
                        b,
                        hctx,
                        module_fqn,
                        &decl.name.text,
                        OwnerKind::Record,
                        m,
                        &new_ty,
                        &mut seen_ordinals,
                    );
                }
            }
            Item::Actor(decl) => {
                let migrates: Vec<&MigrateDecl> = decl
                    .members
                    .iter()
                    .filter_map(|m| match m {
                        ActorMember::Migrate(md) => Some(md),
                        _ => None,
                    })
                    .collect();
                if migrates.is_empty() {
                    continue;
                }
                let Some(&actor_id) = ctx.user_tycon_names.get(&decl.name.text) else {
                    continue;
                };
                let TyConKind::Actor(schema) = &arena.get(actor_id).kind else {
                    continue;
                };
                // The actor's "new" shape: the anonymous record of its state
                // fields — exactly what handlers read and `init` builds.
                let new_ty = Type::record(
                    schema
                        .state_fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect(),
                    RowTail::Closed,
                );
                let mut seen_ordinals: Vec<u32> = Vec::new();
                for m in migrates {
                    check_one_hook(
                        ctx,
                        b,
                        hctx,
                        module_fqn,
                        &decl.name.text,
                        OwnerKind::Actor,
                        m,
                        &new_ty,
                        &mut seen_ordinals,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Record types and actor states share the checking; only the history table
/// they resolve against differs.
#[derive(Clone, Copy)]
enum OwnerKind {
    Record,
    Actor,
}

/// Check one hook: duplicate-ordinal guard, ordinal resolution (T049),
/// `old` binding, body inference, unification with the new shape, purity.
#[expect(clippy::too_many_arguments, reason = "one hook needs all of these")]
fn check_one_hook(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    hctx: &MigrateHistoryCtx<'_>,
    module_fqn: &str,
    owner_name: &str,
    kind: OwnerKind,
    m: &MigrateDecl,
    new_ty: &Type,
    seen_ordinals: &mut Vec<u32>,
) {
    if seen_ordinals.contains(&m.old_type.version) {
        ctx.errors.push(TypeError::DuplicateMigration {
            name: owner_name.to_owned(),
            ordinal: m.old_type.version,
            span: m.span,
        });
        return;
    }
    seen_ordinals.push(m.old_type.version);

    let entry = match kind {
        OwnerKind::Record => hctx
            .history
            .lookup_record(module_fqn, owner_name, m.old_type.version),
        OwnerKind::Actor => hctx
            .history
            .lookup_actor(module_fqn, owner_name, m.old_type.version),
    };
    let Some(entry) = entry else {
        ctx.errors.push(TypeError::UnknownTypeVersion {
            name: m.old_type.name.text.clone(),
            ordinal: m.old_type.version,
            span: m.old_type.span,
        });
        return;
    };

    // Reconstruct `old`'s type: the anonymous record of the history shape.
    let old_ty = Type::record(
        entry
            .shape
            .iter()
            .map(|(n, t)| (n.clone(), resolve_rendered(ctx, b, hctx, t)))
            .collect(),
        RowTail::Closed,
    );

    ctx.env.push_frame();
    ctx.env
        .bind(m.param.text.clone(), ridge_types::Scheme::mono(old_ty));
    let body_ty = infer_expr(ctx, b, &m.body);
    ctx.env.pop_frame();
    // A body that does not fit the new shape is a normal type error: unify
    // through the same machinery every other expression uses.
    if let Err(e) = crate::unify::unify(ctx, &body_ty, new_ty) {
        ctx.errors.push(e);
    }

    // Hooks are pure: capability-check the body against the empty declared
    // set, the same rule a handler with no caps annotation gets.
    crate::caps_check::check_caps_decl_kind(
        ctx,
        b,
        &format!("migrate {owner_name}@{}", m.old_type.version),
        Some(CapabilitySet::PURE),
        &m.body,
        m.span,
        crate::error::CapDeclKind::Handler,
    );
}

// ── Rendered-type re-resolution ─────────────────────────────────────────────

/// Re-resolve a rendered field type from the snapshot history into a
/// semantic [`Type`]. Resolution order: primitives and built-ins via the
/// built-in table, bare names via the module's own/imported tycon names,
/// dotted `fqn.Name` via the producer modules checked so far. Anything that
/// fails degrades to a fresh unification variable.
fn resolve_rendered(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    hctx: &MigrateHistoryCtx<'_>,
    s: &str,
) -> Type {
    let mut p = RendP {
        s: s.as_bytes(),
        i: 0,
    };
    p.ty(ctx, b, hctx)
}

/// Minimal reader over the `render_type` output grammar:
/// `Int`, `app.m.Role`, `List<Int>`, `fn(A, B) -> R`, `(A, B)`,
/// `{ f: T, g: T }`, `{ f: T | _ }`, `_`.
struct RendP<'a> {
    s: &'a [u8],
    i: usize,
}

impl RendP<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i] == b' ' {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, lit: &str) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    /// An identifier, possibly dotted (`app.user.Role`).
    fn ident(&mut self) -> String {
        self.ws();
        let start = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric()
                || self.s[self.i] == b'_'
                || self.s[self.i] == b'.')
        {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
    }

    fn ty(&mut self, ctx: &mut InferCtx, b: &BuiltinTyCons, hctx: &MigrateHistoryCtx<'_>) -> Type {
        self.ws();
        match self.peek() {
            Some(b'_') => {
                self.i += 1;
                Type::Var(ctx.fresh_tyvid())
            }
            Some(b'(') => self.tuple(ctx, b, hctx),
            Some(b'{') => self.record(ctx, b, hctx),
            Some(_) => {
                let name = self.ident();
                if name == "fn" && self.eat(b'(') {
                    return self.fn_ty(ctx, b, hctx);
                }
                let mut args = Vec::new();
                if self.eat(b'<') {
                    loop {
                        args.push(self.ty(ctx, b, hctx));
                        if self.eat(b',') {
                            continue;
                        }
                        let _ = self.eat(b'>');
                        break;
                    }
                }
                Self::named(ctx, b, hctx, &name, args)
            }
            None => Type::Var(ctx.fresh_tyvid()),
        }
    }
    fn fn_ty(
        &mut self,
        ctx: &mut InferCtx,
        b: &BuiltinTyCons,
        hctx: &MigrateHistoryCtx<'_>,
    ) -> Type {
        // `fn(P1, P2) -> R` — already past `fn(`.
        let mut params = Vec::new();
        if !self.eat(b')') {
            loop {
                params.push(self.ty(ctx, b, hctx));
                if self.eat(b',') {
                    continue;
                }
                let _ = self.eat(b')');
                break;
            }
        }
        let _ = self.eat_str("->");
        let ret = self.ty(ctx, b, hctx);
        Type::Fn {
            params,
            ret: Box::new(ret),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        }
    }

    fn tuple(
        &mut self,
        ctx: &mut InferCtx,
        b: &BuiltinTyCons,
        hctx: &MigrateHistoryCtx<'_>,
    ) -> Type {
        let _ = self.eat(b'(');
        let mut elems = vec![self.ty(ctx, b, hctx)];
        while self.eat(b',') {
            elems.push(self.ty(ctx, b, hctx));
        }
        let _ = self.eat(b')');
        if elems.len() == 1 {
            elems.pop().unwrap_or_else(|| Type::Var(ctx.fresh_tyvid()))
        } else {
            Type::Tuple(elems)
        }
    }

    fn record(
        &mut self,
        ctx: &mut InferCtx,
        b: &BuiltinTyCons,
        hctx: &MigrateHistoryCtx<'_>,
    ) -> Type {
        let _ = self.eat(b'{');
        let mut fields = Vec::new();
        loop {
            if self.eat(b'}') {
                break;
            }
            if self.eat(b'|') {
                // Open tail (`| _`): ignore — degrade to closed.
                let _ = self.ident();
                let _ = self.eat(b'}');
                break;
            }
            let name = self.ident();
            let _ = self.eat(b':');
            let fty = self.ty(ctx, b, hctx);
            fields.push((name, fty));
            let _ = self.eat(b',');
        }
        Type::record(fields, RowTail::Closed)
    }

    fn named(
        ctx: &mut InferCtx,
        b: &BuiltinTyCons,
        hctx: &MigrateHistoryCtx<'_>,
        name: &str,
        args: Vec<Type>,
    ) -> Type {
        let fresh = |ctx: &mut InferCtx| Type::Var(ctx.fresh_tyvid());
        // 1. Primitives and built-in containers.
        let builtin: Option<TyConId> = match name {
            "Int" => Some(b.int),
            "Float" => Some(b.float),
            "Bool" => Some(b.bool),
            "Text" => Some(b.text),
            "Unit" => Some(b.unit),
            "Timestamp" => Some(b.timestamp),
            "Decimal" => Some(b.decimal),
            "Uuid" => Some(b.uuid),
            "Bytes" => Some(b.bytes),
            "Date" => Some(b.date),
            "Time" => Some(b.time),
            "List" => Some(b.list),
            "Map" => Some(b.map),
            "Set" => Some(b.set),
            "Option" => Some(b.option),
            "Result" => Some(b.result),
            "Handle" => Some(b.handle),
            _ => None,
        };
        if let Some(id) = builtin {
            return Type::Con(id, args);
        }
        // 2. Dotted `fqn.Name` — resolve through the producer's checked table.
        if let Some((fqn, bare)) = name.rsplit_once('.') {
            if let Some(mid) = hctx.module_fqns.iter().position(|f| f == fqn) {
                if let Some(id) = hctx.checked_tycon_names.get(mid).and_then(|t| t.get(bare)) {
                    return Type::Con(*id, args);
                }
            }
            return fresh(ctx);
        }
        // 3. Bare name — own module's or an imported type.
        if let Some(&id) = ctx.user_tycon_names.get(name) {
            return Type::Con(id, args);
        }
        fresh(ctx)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use ridge_types::history::{VersionEntry, VersionHistory};

    fn write_file(dir: &std::path::Path, rel: &str, content: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create dirs");
        }
        fs::write(full, content).expect("write file");
    }

    fn check(src: &str, history: &VersionHistory) -> Vec<String> {
        let td = TempDir::new().expect("tempdir");
        write_file(
            td.path(),
            "ridge.toml",
            "[workspace]\nname = \"t\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
        );
        write_file(
            td.path(),
            "apps/demo/ridge.toml",
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
        );
        write_file(td.path(), "apps/demo/src/main.ridge", src);
        let disc = ridge_resolve::discover_workspace(td.path());
        let ws = disc.graph.expect("graph");
        let resolved = ridge_resolve::resolve_workspace(ws);
        let result = crate::typecheck_workspace_with_history(&resolved, history);
        result
            .errors
            .iter()
            .map(|(_, e)| e.code().to_string())
            .collect()
    }

    fn user_history() -> VersionHistory {
        let mut h = VersionHistory::default();
        h.records.insert(
            ("demo.main".to_owned(), "User".to_owned()),
            vec![VersionEntry {
                ordinal: 1,
                hash: 111,
                shape: vec![
                    ("name".to_owned(), "Text".to_owned()),
                    ("age".to_owned(), "Int".to_owned()),
                ],
            }],
        );
        h
    }

    #[test]
    fn hook_with_known_history_typechecks() {
        let src = "pub type User = { name: Text, age: Int, email: Text } do\n    migrate (old: User@1) -> User =\n        User { name = old.name, age = old.age, email = \"?\" }\nend\n";
        assert_eq!(check(src, &user_history()), Vec::<String>::new());
    }

    #[test]
    fn hook_without_history_is_t049() {
        let src = "pub type User = { name: Text } do\n    migrate (old: User@1) -> User =\n        User { name = old.name }\nend\n";
        let codes = check(src, &VersionHistory::default());
        assert!(codes.iter().any(|c| c == "T049"), "{codes:?}");
    }

    #[test]
    fn duplicate_edge_is_t050() {
        let src = "pub type User = { name: Text, age: Int } do\n    migrate (old: User@1) -> User =\n        User { name = old.name, age = 0 }\n    migrate (old: User@1) -> User =\n        User { name = old.name, age = 1 }\nend\n";
        let codes = check(src, &user_history());
        assert!(codes.iter().any(|c| c == "T050"), "{codes:?}");
    }

    #[test]
    fn body_not_fitting_new_shape_is_normal_type_error() {
        // Missing the new `email` field: ordinary unification failure, no T049/T050.
        let src = "pub type User = { name: Text, age: Int, email: Text } do\n    migrate (old: User@1) -> User =\n        User { name = old.name, age = old.age }\nend\n";
        let codes = check(src, &user_history());
        assert!(!codes.is_empty(), "a shape mismatch must error");
        assert!(
            !codes.iter().any(|c| c == "T049" || c == "T050"),
            "{codes:?}"
        );
    }

    #[test]
    fn actor_hook_with_history_typechecks() {
        let mut h = VersionHistory::default();
        h.actors.insert(
            ("demo.main".to_owned(), "Counter".to_owned()),
            vec![VersionEntry {
                ordinal: 1,
                hash: 222,
                shape: vec![("count".to_owned(), "Int".to_owned())],
            }],
        );
        let src = "pub actor Counter =\n    state count: Int = 0\n    state step: Int = 1\n    migrate (old: Counter@1) -> Counter =\n        { count = old.count, step = 1 }\n    on bump =\n        count <- count + 1\n";
        assert_eq!(check(src, &h), Vec::<String>::new());
    }
}
