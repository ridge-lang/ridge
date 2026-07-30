//! Serializable per-workspace metadata: the input and output of reload diffs.

use std::collections::BTreeMap;

use ridge_ast::{ActorMember, Item};
use ridge_resolve::{NodeId, ResolvedVisibility, ResolvedWorkspace, SymbolKind};
use ridge_typecheck::caps_check::caps_from_ast_slice;
use ridge_typecheck::TypedWorkspace;
use ridge_types::tycon::{TyConKind, VariantPayload};

use crate::render::{render_ast_type, render_scheme, render_type, render_type_vars, RenderCtx};
use ridge_types::history::{VersionEntry, VersionHistory};

/// Bump when the on-disk layout changes.
///
/// Older formats are still READ — missing history deserializes as empty —
/// but only [`SNAPSHOT_FORMAT`] is written. Formats NEWER than this are
/// rejected by the driver.
pub const SNAPSHOT_FORMAT: u32 = 3;

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
        /// Current ordinal (1 for a type with no recorded history).
        version: u32,
        /// Current shape hash (shared shape hash over `fields`).
        #[serde(default)]
        hash: u64,
        fields: Vec<FieldSnap>,
        /// Previous versions, oldest first. Empty for a first-version type
        /// and for snapshots written before version history existed.
        #[serde(default)]
        history: Vec<VersionSnap>,
        /// Ordinals the source's `migrate` blocks cover.
        #[serde(default)]
        migrate_edges: Vec<u32>,
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
        /// Current state-shape ordinal.
        #[serde(default)]
        version: u32,
        /// Current state-shape hash.
        #[serde(default)]
        hash: u64,
        /// Previous state shapes, oldest first.
        #[serde(default)]
        history: Vec<VersionSnap>,
        /// Ordinals the actor's `migrate` members cover.
        #[serde(default)]
        migrate_edges: Vec<u32>,
    },
}

/// One recorded version of a record's or actor state's shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VersionSnap {
    /// Source-level ordinal (`@version(N)` or the assigned sequence).
    pub ordinal: u32,
    /// Runtime identity: the shared 64-bit shape hash.
    pub hash: u64,
    /// The shape at this version (ordered fields).
    pub shape: Vec<FieldSnap>,
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
/// covered by their owner type's snapshot and skipped. `prev` is the snapshot
/// of the previous build, when one exists; it seeds version ordinals and
/// history.
#[must_use]
pub fn extract_snapshot(
    resolved: &ResolvedWorkspace,
    typed: &TypedWorkspace,
    prev: Option<&WorkspaceSnapshot>,
) -> WorkspaceSnapshot {
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
                    let Some(snap) = type_snapshot(&ctx, tmod, typed, &entry.name, prev, &fqn, ast)
                    else {
                        continue;
                    };
                    snap
                }
                SymbolKind::Actor { handlers, .. } => {
                    actor_snapshot(ast, &entry.name, handlers, prev, &fqn)
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
/// (actor `TyCons` are covered by [`SymbolKind::Actor`] entries). `prev`
/// seeds the version identity; `fqn` and `ast` locate the symbol in the
/// previous snapshot and read its declared `migrate` edges from source.
fn type_snapshot(
    ctx: &RenderCtx<'_>,
    tmod: &ridge_typecheck::TypedModule,
    typed: &TypedWorkspace,
    name: &str,
    prev: Option<&WorkspaceSnapshot>,
    fqn: &str,
    ast: &ridge_ast::Module,
) -> Option<SymbolSnapshot> {
    let names = typed.module_tycon_names.get(tmod.id.0 as usize)?;
    let id = names.get(name)?;
    let decl = typed.tycons.get(id.0 as usize)?;
    match &decl.kind {
        TyConKind::Record(schema) => {
            let fields: Vec<FieldSnap> = schema
                .record_fields()
                .iter()
                .map(|f| FieldSnap {
                    name: f.name.clone(),
                    ty: render_type(ctx, &f.ty),
                })
                .collect();
            let (version, hash, history) = versioned_identity(
                prev_versioned(prev, fqn, name),
                declared_version(ast, name),
                &fields,
            );
            Some(SymbolSnapshot::Record {
                version,
                hash,
                fields,
                history,
                migrate_edges: declared_migrate_edges(ast, name),
            })
        }
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

/// Snapshot for an `actor` symbol: state shape, version identity, handler
/// capability bits, and declared `migrate` edges.
fn actor_snapshot(
    ast: &ridge_ast::Module,
    name: &str,
    handlers: &[ridge_resolve::HandlerSig],
    prev: Option<&WorkspaceSnapshot>,
    fqn: &str,
) -> SymbolSnapshot {
    let state = actor_state_snaps(ast, name);
    let shape: Vec<FieldSnap> = state
        .iter()
        .map(|s| FieldSnap {
            name: s.name.clone(),
            ty: s.ty.clone(),
        })
        .collect();
    let (version, hash, history) =
        versioned_identity(prev_versioned(prev, fqn, name), None, &shape);
    let handlers = handlers
        .iter()
        .map(|h| (h.name.clone(), caps_from_ast_slice(&h.caps).bits()))
        .collect();
    SymbolSnapshot::Actor {
        state,
        handlers,
        version,
        hash,
        history,
        migrate_edges: actor_migrate_edges(ast, name),
    }
}

/// Compute `(version, hash, history)` for a shape-bearing symbol.
///
/// `prev_versioned` is the same symbol's `(version, hash, shape, history)`
/// from the previous snapshot, when it existed. `declared` is an optional
/// `@version(N)` override from source (records only; actors pass `None`).
/// Rules:
/// - same shape ⇒ same ordinal and hash, history carried over unchanged;
/// - changed shape ⇒ ordinal = `declared` or `prev.version + 1`, fresh hash,
///   and the previous CURRENT version is appended to the history;
/// - no previous version ⇒ ordinal = `declared` or 1, empty history.
///
/// The comparison is by shape, not by hash: a pre-history snapshot (hash 0)
/// with unchanged fields keeps its ordinal instead of spuriously bumping.
fn versioned_identity(
    prev_versioned: Option<(u32, u64, Vec<FieldSnap>, Vec<VersionSnap>)>,
    declared: Option<u32>,
    current_shape: &[FieldSnap],
) -> (u32, u64, Vec<VersionSnap>) {
    let pairs: Vec<(String, String)> = current_shape
        .iter()
        .map(|f| (f.name.clone(), f.ty.clone()))
        .collect();
    let hash = ridge_types::shape::shape_hash(&pairs);
    match prev_versioned {
        Some((pv, _, pshape, phist)) if pshape == current_shape => (pv, hash, phist),
        Some((pv, ph, pshape, phist)) => {
            let mut history = phist;
            history.push(VersionSnap {
                ordinal: pv,
                hash: ph,
                shape: pshape,
            });
            (declared.unwrap_or(pv + 1), hash, history)
        }
        None => (declared.unwrap_or(1), hash, Vec::new()),
    }
}

/// The previous snapshot's versioned view of one symbol, if it carried one.
/// Pre-history actor entries (hash 0) yield `None` — a safe v1 start.
fn prev_versioned(
    prev: Option<&WorkspaceSnapshot>,
    fqn: &str,
    name: &str,
) -> Option<(u32, u64, Vec<FieldSnap>, Vec<VersionSnap>)> {
    let sym = prev?.modules.get(fqn)?.symbols.get(name)?;
    match sym {
        SymbolSnapshot::Record {
            version,
            hash,
            fields,
            history,
            ..
        } => Some((*version, *hash, fields.clone(), history.clone())),
        SymbolSnapshot::Actor {
            state,
            version,
            hash,
            history,
            ..
        } if *hash != 0 => Some((
            *version,
            *hash,
            state
                .iter()
                .map(|s| FieldSnap {
                    name: s.name.clone(),
                    ty: s.ty.clone(),
                })
                .collect(),
            history.clone(),
        )),
        _ => None,
    }
}

/// The declared `@version(N)` override of one type declaration, if any.
fn declared_version(ast: &ridge_ast::Module, type_name: &str) -> Option<u32> {
    for item in &ast.items {
        if let Item::Type(t) = item {
            if t.name.text == type_name {
                return t.version;
            }
        }
    }
    None
}

/// Ordinals covered by `migrate` blocks in one type declaration's source.
fn declared_migrate_edges(ast: &ridge_ast::Module, type_name: &str) -> Vec<u32> {
    for item in &ast.items {
        if let Item::Type(t) = item {
            if t.name.text == type_name {
                return t.migrates.iter().map(|m| m.old_type.version).collect();
            }
        }
    }
    Vec::new()
}

/// Ordinals covered by `migrate` members in one actor declaration's source.
fn actor_migrate_edges(ast: &ridge_ast::Module, actor_name: &str) -> Vec<u32> {
    for item in &ast.items {
        if let Item::Actor(a) = item {
            if a.name.text == actor_name {
                return a
                    .members
                    .iter()
                    .filter_map(|m| match m {
                        ActorMember::Migrate(md) => Some(md.old_type.version),
                        _ => None,
                    })
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Build the compiler-facing version history from a snapshot.
///
/// Every list ends with the version the snapshot considers current (a
/// `migrate` hook always targets a shape the RUNNING build may still hold).
/// Symbols whose ordinal is 0 came from a pre-history snapshot format and
/// produce no entries — "no history" is a safe v1 start, never a crash.
#[must_use]
pub fn history_of(snapshot: &WorkspaceSnapshot) -> VersionHistory {
    let mut out = VersionHistory::default();
    for (fqn, m) in &snapshot.modules {
        for (name, sym) in &m.symbols {
            let (kind_is_record, version, hash, shape, history) = match sym {
                SymbolSnapshot::Record {
                    version,
                    hash,
                    fields,
                    history,
                    ..
                } => (true, *version, *hash, fields.clone(), history.clone()),
                SymbolSnapshot::Actor {
                    state,
                    version,
                    hash,
                    history,
                    ..
                } => (
                    false,
                    *version,
                    *hash,
                    state
                        .iter()
                        .map(|s| FieldSnap {
                            name: s.name.clone(),
                            ty: s.ty.clone(),
                        })
                        .collect(),
                    history.clone(),
                ),
                _ => continue,
            };
            if version == 0 {
                continue;
            }
            let mut entries: Vec<VersionEntry> = history
                .iter()
                .map(|v| VersionEntry {
                    ordinal: v.ordinal,
                    hash: v.hash,
                    shape: v
                        .shape
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect(),
                })
                .collect();
            entries.push(VersionEntry {
                ordinal: version,
                hash,
                shape: shape
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone()))
                    .collect(),
            });
            let key = (fqn.clone(), name.clone());
            if kind_is_record {
                out.records.insert(key, entries);
            } else {
                out.actors.insert(key, entries);
            }
        }
    }
    out
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
        extract_snapshot(&resolved, &checked.typed, None)
    }

    /// Recompile `src` against the previous snapshot and extract.
    /// (Same body as `snapshot_of` with `Some(prev)` threaded through — a
    /// deliberate duplicate, not a refactor of the existing helper.)
    fn snapshot_with_prev(src: &str, prev: &WorkspaceSnapshot) -> WorkspaceSnapshot {
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
        extract_snapshot(&resolved, &checked.typed, Some(prev))
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
                hash: 42,
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
                history: vec![],
                migrate_edges: vec![],
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
            SymbolSnapshot::Record {
                version,
                hash,
                fields,
                ..
            } => {
                assert_eq!(*version, 1);
                assert_ne!(*hash, 0, "fresh build computes a real shape hash");
                let got: Vec<(&str, &str)> = fields
                    .iter()
                    .map(|f| (f.name.as_str(), f.ty.as_str()))
                    .collect();
                assert_eq!(got, [("name", "Text"), ("age", "Int")]);
            }
            other => panic!("User should be a record, got {other:?}"),
        }
        match &module.symbols["Counter"] {
            SymbolSnapshot::Actor {
                state,
                handlers,
                hash,
                version,
                ..
            } => {
                assert_eq!(state.len(), 2);
                assert_eq!(state[0].name, "count");
                assert!(state[0].has_default, "count has a default");
                assert_eq!(state[1].name, "step");
                assert!(!state[1].has_default, "step has no default");
                assert!(handlers.contains_key("bump"), "handler captured");
                assert_eq!(*version, 1);
                assert_ne!(*hash, 0, "actor state carries a real shape hash");
            }
            other => panic!("Counter should be an actor, got {other:?}"),
        }
    }

    #[test]
    fn unchanged_shape_keeps_ordinal_and_hash() {
        let s1 = snapshot_of(DEMO_SRC);
        let s2 = snapshot_with_prev(DEMO_SRC, &s1);
        let (
            SymbolSnapshot::Record {
                version: v1,
                hash: h1,
                history: hist1,
                ..
            },
            SymbolSnapshot::Record {
                version: v2,
                hash: h2,
                history: hist2,
                ..
            },
        ) = (
            &s1.modules["demo.main"].symbols["User"],
            &s2.modules["demo.main"].symbols["User"],
        )
        else {
            panic!("records")
        };
        assert_eq!((v1, h1), (v2, h2), "unchanged shape keeps identity");
        assert!(hist1.is_empty() && hist2.is_empty(), "no version appended");
    }

    #[test]
    fn changed_shape_bumps_ordinal_and_records_history() {
        let s1 = snapshot_of(DEMO_SRC);
        let edited = DEMO_SRC.replace("age: Int", "age: Int, email: Text");
        let s2 = snapshot_with_prev(&edited, &s1);
        let SymbolSnapshot::Record {
            version,
            hash,
            history,
            ..
        } = &s2.modules["demo.main"].symbols["User"]
        else {
            panic!("record")
        };
        assert_eq!(*version, 2);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].ordinal, 1);
        assert_ne!(history[0].hash, *hash);
        assert_eq!(history[0].shape.len(), 2, "old shape had 2 fields");
    }

    #[test]
    fn version_attr_overrides_ordinal() {
        let s1 = snapshot_of(DEMO_SRC);
        let edited = DEMO_SRC
            .replace("pub type User =", "pub type User @version(7) =")
            .replace("age: Int", "age: Int, email: Text");
        let s2 = snapshot_with_prev(&edited, &s1);
        let SymbolSnapshot::Record {
            version,
            migrate_edges,
            ..
        } = &s2.modules["demo.main"].symbols["User"]
        else {
            panic!("record")
        };
        assert_eq!(*version, 7);
        assert!(migrate_edges.is_empty(), "no migrate blocks in source");
    }

    #[test]
    fn migrate_edges_recorded_from_source() {
        let src = "pub type User = { name: Text, email: Text } do\n    migrate (old: User@1) -> User =\n        User { name = old.name, email = old.email }\nend\n";
        let snap = snapshot_of(src);
        let SymbolSnapshot::Record { migrate_edges, .. } =
            &snap.modules["demo.main"].symbols["User"]
        else {
            panic!("record")
        };
        assert_eq!(migrate_edges, &vec![1]);
    }

    #[test]
    fn actor_state_history_and_edges() {
        let v1 = "pub actor Counter =\n    state count: Int = 0\n    on bump =\n        count <- count + 1\n";
        let s1 = snapshot_of(v1);
        let v2 = "pub actor Counter =\n    state count: Int = 0\n    state step: Int = 1\n    migrate (old: Counter@1) -> Counter =\n        { count = old.count, step = 1 }\n    on bump =\n        count <- count + 1\n";
        let s2 = snapshot_with_prev(v2, &s1);
        let SymbolSnapshot::Actor {
            version,
            hash,
            history,
            migrate_edges,
            ..
        } = &s2.modules["demo.main"].symbols["Counter"]
        else {
            panic!("actor")
        };
        assert_eq!(*version, 2);
        assert_eq!(history.len(), 1);
        assert_eq!(migrate_edges, &vec![1]);
        let SymbolSnapshot::Actor { hash: h1, .. } = &s1.modules["demo.main"].symbols["Counter"]
        else {
            panic!("actor")
        };
        assert_eq!(
            history[0].hash, *h1,
            "history stores the previous current hash"
        );
        let _ = hash;
    }

    #[test]
    fn history_of_includes_current_versions() {
        let s1 = snapshot_of(DEMO_SRC);
        let h = history_of(&s1);
        let e = h.lookup_record("demo.main", "User", 1).expect("v1 known");
        assert_eq!(e.shape.len(), 2);
        let a = h
            .lookup_actor("demo.main", "Counter", 1)
            .expect("actor v1 known");
        assert_eq!(a.shape.len(), 2);
    }

    #[test]
    fn format2_snapshot_deserializes_with_empty_history() {
        // A minimal format-2 document (no history fields anywhere) must still
        // parse: "no history" — a safe v1 start, never a crash.
        let json = r#"{
        "format": 2,
        "modules": {
            "demo.main": {
                "symbols": {
                    "User": { "kind": "record", "version": 1, "fields": [{"name": "name", "ty": "Text"}] }
                },
                "content_hash": 0
            }
        }
    }"#;
        let snap: WorkspaceSnapshot = serde_json::from_str(json).expect("format 2 parses");
        let SymbolSnapshot::Record {
            history,
            migrate_edges,
            ..
        } = &snap.modules["demo.main"].symbols["User"]
        else {
            panic!("record")
        };
        assert!(history.is_empty() && migrate_edges.is_empty());
        let h = history_of(&snap);
        assert_eq!(
            h.lookup_record("demo.main", "User", 1).map(|e| e.ordinal),
            Some(1)
        );
    }
}
