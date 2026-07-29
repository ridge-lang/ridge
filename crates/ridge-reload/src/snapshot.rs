//! Serializable per-workspace metadata: the input and output of reload diffs.

use std::collections::BTreeMap;

use ridge_ast::{ActorMember, Item};
use ridge_resolve::{NodeId, ResolvedVisibility, ResolvedWorkspace, SymbolKind};
use ridge_typecheck::caps_check::caps_from_ast_slice;
use ridge_typecheck::TypedWorkspace;
use ridge_types::tycon::{TyConKind, VariantPayload};

use crate::render::{render_ast_type, render_scheme, render_type, render_type_vars, RenderCtx};

/// Bump when the on-disk layout changes; old snapshots are rejected.
pub const SNAPSHOT_FORMAT: u32 = 2;

/// The public surface of one compiled workspace.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceSnapshot {
    /// On-disk layout version; must match [`SNAPSHOT_FORMAT`].
    pub format: u32,
    /// Keyed by fully-qualified module name.
    pub modules: BTreeMap<String, ModuleSnapshot>,
}

/// The public symbols of one module.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModuleSnapshot {
    /// Keyed by symbol name. Public symbols only.
    pub symbols: BTreeMap<String, SymbolSnapshot>,
    /// Hash of the module's parsed AST: changes on ANY source edit (including
    /// body-only ones the symbol surface cannot see), so the dev-loop loader
    /// knows which modules to reload. Not used by the compatibility checker.
    #[serde(default)]
    pub content_hash: u64,
}

/// One public symbol's reload-relevant shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SymbolSnapshot {
    /// A function: rendered signature plus inferred capability bits.
    Fn { signature: String, caps_bits: u16 },
    /// A constant: rendered type.
    Const { signature: String },
    /// A record type with its layout version and declared fields.
    Record {
        version: u32,
        fields: Vec<FieldSnap>,
    },
    /// A union type with its layout version and declared variants.
    Union {
        version: u32,
        variants: Vec<VariantSnap>,
    },
    /// A type alias: rendered expansion.
    Alias { target: String },
    /// An actor: state fields and message-handler capability bits.
    Actor {
        state: Vec<StateSnap>,
        handlers: BTreeMap<String, u16>,
    },
}

/// A record field: name and rendered type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FieldSnap {
    /// Field name.
    pub name: String,
    /// Rendered field type.
    pub ty: String,
}

/// An actor state field: name, rendered type, and whether a default exists.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StateSnap {
    /// State field name.
    pub name: String,
    /// Rendered state field type.
    pub ty: String,
    /// Whether the declaration provides a default expression.
    pub has_default: bool,
}

/// A union variant: name and rendered payload (`""` for nullary).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VariantSnap {
    /// Variant name.
    pub name: String,
    /// Rendered payload; empty for nullary variants.
    pub payload: String,
}

/// Extracts the public surface of a resolved + typed workspace.
///
/// Symbol identity is `(module FQN, symbol name)`; only non-file-private
/// symbols are captured. Synthesised constructors and field accessors are
/// covered by their owner type's snapshot and skipped.
#[must_use]
pub fn extract_snapshot(resolved: &ResolvedWorkspace, typed: &TypedWorkspace) -> WorkspaceSnapshot {
    let module_fqns: Vec<String> = resolved
        .graph
        .modules
        .iter()
        .map(|m| m.fully_qualified_name.clone())
        .collect();
    let ctx = RenderCtx {
        tycons: &typed.tycons,
        module_fqns: &module_fqns,
    };

    let mut modules = BTreeMap::new();
    for rmod in &resolved.modules {
        let fqn = ctx
            .module_fqns
            .get(rmod.id.0 as usize)
            .cloned()
            .unwrap_or_default();
        let Some(tmod) = typed.modules.get(rmod.id.0 as usize) else {
            continue;
        };
        let Some(ast) = resolved.module_asts.get(rmod.id.0 as usize) else {
            continue;
        };
        let empty_schemes = rustc_hash::FxHashMap::default();
        let schemes = typed
            .module_schemes
            .get(rmod.id.0 as usize)
            .unwrap_or(&empty_schemes);

        let mut symbols = BTreeMap::new();
        for entry in &rmod.symbols.entries {
            if entry.visibility == ResolvedVisibility::FilePrivate {
                continue;
            }
            let snap = match &entry.kind {
                SymbolKind::Fn { caps } => {
                    let signature = schemes
                        .get(&entry.name)
                        .map_or_else(|| "?".to_string(), |s| render_scheme(&ctx, s));
                    let caps_bits = fn_caps_bits(ast, &entry.name, caps, tmod);
                    SymbolSnapshot::Fn {
                        signature,
                        caps_bits,
                    }
                }
                SymbolKind::Const => {
                    let signature = schemes
                        .get(&entry.name)
                        .map_or_else(|| "?".to_string(), |s| render_scheme(&ctx, s));
                    SymbolSnapshot::Const { signature }
                }
                SymbolKind::Type { .. } => {
                    let Some(snap) = type_snapshot(&ctx, tmod, typed, &entry.name) else {
                        continue;
                    };
                    snap
                }
                SymbolKind::Actor { handlers, .. } => {
                    let state = actor_state_snaps(ast, &entry.name);
                    let handlers = handlers
                        .iter()
                        .map(|h| (h.name.clone(), caps_from_ast_slice(&h.caps).bits()))
                        .collect();
                    SymbolSnapshot::Actor { state, handlers }
                }
                // Constructors and field accessors are covered by their owner
                // type's snapshot; unknown future kinds are skipped too.
                _ => continue,
            };
            symbols.insert(entry.name.clone(), snap);
        }
        // Content hash over the module AST: identical source parses to an
        // identical tree (spans included), so any edit — even one invisible
        // to the symbol surface, like a handler-body rewrite — flips it.
        let content_hash = {
            use std::hash::Hasher;
            let mut h = rustc_hash::FxHasher::default();
            h.write(format!("{ast:?}").as_bytes());
            h.finish()
        };
        modules.insert(
            fqn,
            ModuleSnapshot {
                symbols,
                content_hash,
            },
        );
    }

    WorkspaceSnapshot {
        format: SNAPSHOT_FORMAT,
        modules,
    }
}

/// Inferred capability bits for a top-level fn, read through the proxy
/// `NodeId(fn.span.start)` entry in `inferred_caps`. Falls back to the
/// declared capabilities when no inferred entry exists (e.g. FFI bodies).
fn fn_caps_bits(
    ast: &ridge_ast::Module,
    name: &str,
    declared: &[ridge_ast::Capability],
    tmod: &ridge_typecheck::TypedModule,
) -> u16 {
    for item in &ast.items {
        if let Item::Fn(f) = item {
            if f.name.text == name {
                return tmod
                    .inferred_caps
                    .get(&NodeId(f.span.start))
                    .map_or_else(|| caps_from_ast_slice(&f.caps), |c| *c)
                    .bits();
            }
        }
    }
    caps_from_ast_slice(declared).bits()
}

/// Snapshot for a `type` symbol, looked up through the module's own
/// name → `TyConId` table so synthesised types elsewhere in the arena do not
/// confuse the lookup. `None` for kinds that carry no public shape here
/// (actor `TyCons` are covered by [`SymbolKind::Actor`] entries).
fn type_snapshot(
    ctx: &RenderCtx<'_>,
    tmod: &ridge_typecheck::TypedModule,
    typed: &TypedWorkspace,
    name: &str,
) -> Option<SymbolSnapshot> {
    let names = typed.module_tycon_names.get(tmod.id.0 as usize)?;
    let id = names.get(name)?;
    let decl = typed.tycons.get(id.0 as usize)?;
    match &decl.kind {
        TyConKind::Record(schema) => Some(SymbolSnapshot::Record {
            version: 1,
            fields: schema
                .record_fields()
                .iter()
                .map(|f| FieldSnap {
                    name: f.name.clone(),
                    ty: render_type(ctx, &f.ty),
                })
                .collect(),
        }),
        TyConKind::Union(schema) => Some(SymbolSnapshot::Union {
            version: 1,
            variants: schema
                .variants
                .iter()
                .map(|v| {
                    let payload = match &v.kind {
                        VariantPayload::Nullary => String::new(),
                        VariantPayload::Positional(tys) => tys
                            .iter()
                            .map(|t| render_type(ctx, t))
                            .collect::<Vec<_>>()
                            .join(", "),
                        VariantPayload::Record(rs) => {
                            let fs: Vec<String> = rs
                                .record_fields()
                                .iter()
                                .map(|f| format!("{}: {}", f.name, render_type(ctx, &f.ty)))
                                .collect();
                            format!("{{{}}}", fs.join(", "))
                        }
                    };
                    VariantSnap {
                        name: v.name.clone(),
                        payload,
                    }
                })
                .collect(),
        }),
        TyConKind::Alias { params, body } => Some(SymbolSnapshot::Alias {
            target: render_type_vars(ctx, body, params),
        }),
        _ => None,
    }
}

/// State-field snapshots for an actor, read from its AST declaration (state
/// field types never get a semantic `Type` outside inference).
fn actor_state_snaps(ast: &ridge_ast::Module, name: &str) -> Vec<StateSnap> {
    for item in &ast.items {
        if let Item::Actor(a) = item {
            if a.name.text == name {
                return a
                    .members
                    .iter()
                    .filter_map(|m| match m {
                        ActorMember::State(s) => Some(StateSnap {
                            name: s.name.text.clone(),
                            ty: render_ast_type(&s.ty),
                            has_default: s.default.is_some(),
                        }),
                        _ => None,
                    })
                    .collect();
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn write_file(dir: &std::path::Path, rel: &str, content: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create dirs");
        }
        fs::write(full, content).expect("write file");
    }

    /// Compiles a one-project workspace from `src` and extracts its snapshot.
    fn snapshot_of(src: &str) -> WorkspaceSnapshot {
        let td = TempDir::new().expect("tempdir");
        write_file(
            td.path(),
            "ridge.toml",
            "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
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
        let checked = ridge_typecheck::typecheck_workspace(&resolved);
        extract_snapshot(&resolved, &checked.typed)
    }

    const DEMO_SRC: &str = "\
pub fn answer () -> Int = 42
fn _helper () -> Int = 1
pub type User = { name: Text, age: Int }
pub actor Counter =
    state count: Int = 0
    state step: Int
    on bump = count <- count + 1
";

    #[test]
    fn extracts_public_symbols_only() {
        let snap = snapshot_of(DEMO_SRC);
        assert_eq!(snap.modules.len(), 1);
        let module = snap.modules.values().next().expect("one module");
        assert!(module.symbols.contains_key("answer"), "pub fn captured");
        assert!(module.symbols.contains_key("User"), "pub record captured");
        assert!(module.symbols.contains_key("Counter"), "pub actor captured");
        assert!(
            !module.symbols.contains_key("_helper"),
            "file-private fn skipped"
        );
        match &module.symbols["answer"] {
            SymbolSnapshot::Fn { signature, .. } => assert_eq!(signature, "fn() -> Int"),
            other => panic!("answer should be a fn, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_json_roundtrip() {
        let mut symbols = BTreeMap::new();
        symbols.insert(
            "answer".to_string(),
            SymbolSnapshot::Fn {
                signature: "fn() -> Int".to_string(),
                caps_bits: 0,
            },
        );
        symbols.insert(
            "User".to_string(),
            SymbolSnapshot::Record {
                version: 1,
                fields: vec![
                    FieldSnap {
                        name: "name".to_string(),
                        ty: "Text".to_string(),
                    },
                    FieldSnap {
                        name: "age".to_string(),
                        ty: "Int".to_string(),
                    },
                ],
            },
        );
        let mut modules = BTreeMap::new();
        modules.insert(
            "demo.main".to_string(),
            ModuleSnapshot {
                symbols,
                content_hash: 0,
            },
        );
        let snap = WorkspaceSnapshot {
            format: SNAPSHOT_FORMAT,
            modules,
        };

        let json = serde_json::to_string_pretty(&snap).expect("serialize");
        let back: WorkspaceSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, snap);
        assert_eq!(back.format, SNAPSHOT_FORMAT);
    }

    #[test]
    fn record_fields_and_actor_state_captured() {
        let snap = snapshot_of(DEMO_SRC);
        let module = snap.modules.values().next().expect("one module");
        match &module.symbols["User"] {
            SymbolSnapshot::Record { version, fields } => {
                assert_eq!(*version, 1);
                let got: Vec<(&str, &str)> = fields
                    .iter()
                    .map(|f| (f.name.as_str(), f.ty.as_str()))
                    .collect();
                assert_eq!(got, [("name", "Text"), ("age", "Int")]);
            }
            other => panic!("User should be a record, got {other:?}"),
        }
        match &module.symbols["Counter"] {
            SymbolSnapshot::Actor { state, handlers } => {
                assert_eq!(state.len(), 2);
                assert_eq!(state[0].name, "count");
                assert!(state[0].has_default, "count has a default");
                assert_eq!(state[1].name, "step");
                assert!(!state[1].has_default, "step has no default");
                assert!(handlers.contains_key("bump"), "handler captured");
            }
            other => panic!("Counter should be an actor, got {other:?}"),
        }
    }
}
