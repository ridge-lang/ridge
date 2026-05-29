//! Per-module symbol table built by the T6 top-level collector.
//!
//! [`collect_symbols`] walks a parsed [`Module`] with `TopLevelCollector`
//! (private), populating a [`SymbolTable`] and emitting
//! [`ResolveError::DuplicateDeclaration`] (`R005`) whenever two top-level
//! names collide.
//!
//! # Index vs. entries split
//!
//! `index` maps a source name → `SymbolId` for name-lookup-able bindings only.
//! Auto-constructors and field-accessors synthesised from record types are added
//! to `entries` but NOT to `index`:
//! - **Record auto-constructor**: shares the type's name; the type's `SymbolEntry`
//!   is already in `index`, so the constructor is looked up via `SymbolKind::Type`.
//! - **`FieldAccessors`**: looked up via the owning type, not by bare name.
//!
//! Union constructors with distinct names ARE added to `index` (and can trigger
//! R005 if they collide with another top-level binding).
//!
//! # Export cross-reference (DR-08)
//!
//! `[project.exports].public` cross-reference populates the
//! [`SymbolEntry::exported_externally`] flag via [`apply_external_exports`],
//! called from [`crate::resolve_workspace`] after T6 collection.

use rustc_hash::FxHashMap;

use ridge_ast::{visit::Visit, Capability, Item, Module, Span};

use crate::{
    error::{ManifestError, ResolveError},
    globs::GlobPattern,
    visibility::{resolve_visibility, ResolvedVisibility},
    ModuleId, SymbolId,
};

// ── Public data types ─────────────────────────────────────────────────────────

/// All top-level symbols of one module.
#[derive(Debug, Clone)]
pub struct SymbolTable {
    /// Which module this table belongs to.
    pub module: ModuleId,
    /// All symbol entries in insertion order.
    pub entries: Vec<SymbolEntry>,
    /// Fast name-lookup index: source name → `SymbolId`.
    ///
    /// Contains only name-lookup-able entries (functions, consts, types, actors,
    /// union constructors with distinct names). Record auto-constructors and
    /// field-accessors are in `entries` only.
    pub(crate) index: FxHashMap<String, SymbolId>,
}

/// A single top-level symbol.
#[derive(Debug, Clone)]
pub struct SymbolEntry {
    /// Unique identifier within this table.
    pub id: SymbolId,
    /// The source name of this symbol.
    pub name: String,
    /// What kind of symbol this is.
    pub kind: SymbolKind,
    /// Resolved visibility (post underscore-prefix and pub(internal) rules).
    pub visibility: ResolvedVisibility,
    /// Span of the declaration that introduces this symbol.
    pub def_span: Span,
    /// Whether this symbol is exported externally from the project.
    ///
    /// Set to `true` when the symbol's `visibility` is
    /// [`ResolvedVisibility::Pub`] **and** the symbol name is matched by at
    /// least one pattern in `[project.exports].public`.
    ///
    /// Populated by `apply_external_exports` (DR-08 post-pass) in
    /// [`crate::resolve_workspace`] after T6 symbol collection.  Defaults to
    /// `false` for symbols that are project-internal or omitted from the export
    /// surface.
    ///
    /// Phase 4 (type checker) reads this to gate signature export tables and IR
    /// codegen boundaries.
    pub exported_externally: bool,
}

/// The kind of a top-level symbol.
///
/// TODO(T8): may need `decl_node: NodeId` fields on each variant once use-site
/// lookups are implemented. Deferred so T6 stays minimal.
///
/// # Stability
///
/// Marked `#[non_exhaustive]` — new symbol kinds may be added in Phase 4
/// (type aliases, trait items) or later.  Match arms outside this crate must
/// include a wildcard (`_`) arm.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum SymbolKind {
    /// A `fn` declaration.
    Fn {
        /// Capability annotations declared on the function.
        caps: Vec<Capability>,
    },
    /// A `const` declaration.
    Const,
    /// A `type` declaration.
    Type {
        /// Number of type parameters (0 for monomorphic types).
        arity: u32,
    },
    /// An `actor` declaration.
    Actor {
        /// State fields declared inside the actor body.
        state: Vec<StateField>,
        /// `on` message handlers (excludes `init`).
        handlers: Vec<HandlerSig>,
    },
    /// A synthesised constructor symbol (record auto-ctor or union variant).
    Constructor {
        /// The `SymbolId` of the owning `Type` entry.
        owner_type: SymbolId,
        /// Variant index.  Always 0 for record auto-constructors.  For union
        /// variants this is the source-order index — so the FIRST union variant
        /// is also 0, indistinguishable from a record on `variant` alone.  Use
        /// `is_record` to discriminate.  A prior bug motivated adding `is_record`:
        /// pre-fix, the lower used `variant == 0` as a
        /// record-vs-union test, which silently miscompiled the first variant
        /// of every union type to an empty map.
        variant: u32,
        /// Number of constructor arguments (fields for records, args for positional).
        arity: u32,
        /// True if this constructor is the auto-synthesised constructor for a
        /// `type T = { ... }` record declaration; false if it is a variant of
        /// a `type T = A | B | C` union declaration.
        is_record: bool,
    },
    /// A synthesised field-accessor symbol.
    FieldAccessor {
        /// The `SymbolId` of the owning `Type` entry.
        owner_type: SymbolId,
        /// Field name.
        field: String,
    },
}

/// A state-field descriptor captured from an actor declaration.
#[derive(Debug, Clone)]
pub struct StateField {
    /// Field name.
    pub name: String,
    /// Whether a default expression was provided.
    pub defaulted: bool,
    /// Span of the `state` declaration.
    pub def_span: Span,
}

/// Descriptor for an `on` message-handler captured from an actor declaration.
#[derive(Debug, Clone)]
pub struct HandlerSig {
    /// Handler name.
    pub name: String,
    /// Capability annotations declared on the handler.
    pub caps: Vec<Capability>,
    /// Span of the `on` declaration.
    pub def_span: Span,
}

// ── SymbolTable impl ──────────────────────────────────────────────────────────

impl SymbolTable {
    /// Construct an empty symbol table for the given module.
    ///
    /// Used as a placeholder in [`crate::resolve_module`] and error paths where
    /// symbol collection did not run.
    #[must_use]
    pub fn empty(module: ModuleId) -> Self {
        Self {
            module,
            entries: Vec::new(),
            index: FxHashMap::default(),
        }
    }

    /// Look up a symbol by its source name.
    ///
    /// Returns the first (and, in a correct module, only) entry with this name
    /// that is reachable by name lookup. Synthesised entries not in `index`
    /// (record auto-constructors, field-accessors) are not returned here.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&SymbolEntry> {
        let id = self.index.get(name)?;
        self.entries.get(id.0 as usize)
    }
}

// ── collect_symbols ───────────────────────────────────────────────────────────

/// Run T6's top-level collection over a parsed module.
///
/// Returns the module's [`SymbolTable`] plus any `R005 DuplicateDeclaration`
/// errors encountered during collection.
///
/// # Notes
///
/// - Imports are skipped — they do NOT add to the symbol table.
/// - Constructor/accessor synthesis for record and union types happens inline.
/// - `[project.exports].public` cross-reference is performed post-collection by
///   [`apply_external_exports`] (DR-08), called from [`crate::resolve_workspace`]
///   after this function returns.
#[must_use]
pub fn collect_symbols(module_id: ModuleId, ast: &Module) -> (SymbolTable, Vec<ResolveError>) {
    let mut collector = TopLevelCollector {
        table: SymbolTable {
            module: module_id,
            entries: Vec::new(),
            index: FxHashMap::default(),
        },
        errors: Vec::new(),
    };
    collector.visit_module(ast);
    (collector.table, collector.errors)
}

/// DR-08 post-pass: cross-reference `[project.exports].public` globs against
/// the collected symbol table and set [`SymbolEntry::exported_externally`].
///
/// # Algorithm
///
/// For each entry in `exports_public`:
/// - Check whether any `pub` symbol's name matches the pattern.
/// - If matched: set `exported_externally = true` on every matching `pub` entry.
/// - If NO symbol at all (pub or private) matched the pattern: emit
///   [`ManifestError::ExportNotFound`] (`M020`) — the pattern likely contains
///   a typo or references a removed symbol.
///
/// Wildcard patterns (e.g. `"**"`) typically match everything and do not
/// trigger M020.  Only a pattern that matches zero symbols fires the error.
///
/// Synthesised entries (constructors, field-accessors) inherit the flag from
/// their owning type entry when the owning type is exported externally.
///
/// # Errors
///
/// Returns `M020 ExportNotFound` for every export pattern that matched no
/// symbols in this module's table.
pub fn apply_external_exports(
    table: &mut SymbolTable,
    exports_public: &[GlobPattern],
    manifest_path: &std::path::Path,
) -> Vec<ManifestError> {
    if exports_public.is_empty() {
        return Vec::new();
    }

    let mut errors: Vec<ManifestError> = Vec::new();
    let mut exported_type_ids: std::collections::HashSet<SymbolId> =
        std::collections::HashSet::new();

    // Per-pattern pass: check each export pattern against the symbol table.
    for pat in exports_public {
        let mut any_matched = false;
        for entry in &mut table.entries {
            if pat.matches(&entry.name) {
                any_matched = true;
                if entry.visibility == ResolvedVisibility::Pub {
                    entry.exported_externally = true;
                    if let SymbolKind::Type { .. } | SymbolKind::Actor { .. } = entry.kind {
                        exported_type_ids.insert(entry.id);
                    }
                }
            }
        }
        if !any_matched {
            // Pattern matched nothing at all — likely a typo in the manifest.
            errors.push(ManifestError::ExportNotFound {
                name: pat.raw.clone(),
                manifest_path: manifest_path.to_path_buf(),
            });
        }
    }

    // Propagation pass: synthesised constructors / accessors inherit the flag
    // from their owning type if the owning type was exported.
    for entry in &mut table.entries {
        match &entry.kind {
            SymbolKind::Constructor { owner_type, .. }
            | SymbolKind::FieldAccessor { owner_type, .. } => {
                if exported_type_ids.contains(owner_type) {
                    entry.exported_externally = true;
                }
            }
            _ => {}
        }
    }

    errors
}

// ── TopLevelCollector ─────────────────────────────────────────────────────────

/// Internal visitor that populates a [`SymbolTable`] while detecting R005.
struct TopLevelCollector {
    table: SymbolTable,
    errors: Vec<ResolveError>,
}

impl TopLevelCollector {
    /// Allocate the next `SymbolId` (one past the last entry).
    fn next_id(&self) -> SymbolId {
        SymbolId(u32::try_from(self.table.entries.len()).unwrap_or(u32::MAX))
    }

    /// Push an entry into `entries`.
    ///
    /// If `register_in_index` is `true`, also insert into `index`. If the name
    /// already exists there, emit R005 and skip the new entry entirely
    /// (first-declaration-wins).
    ///
    /// Returns the assigned `SymbolId` on success, or `None` on R005.
    fn push(
        &mut self,
        name: String,
        kind: SymbolKind,
        visibility: ResolvedVisibility,
        def_span: Span,
        register_in_index: bool,
    ) -> Option<SymbolId> {
        if register_in_index {
            if let Some(&existing_id) = self.table.index.get(&name) {
                let first_span = self
                    .table
                    .entries
                    .get(existing_id.0 as usize)
                    .map_or(Span::point(0), |e| e.def_span);
                self.errors.push(ResolveError::DuplicateDeclaration {
                    name,
                    first_span,
                    second_span: def_span,
                });
                return None;
            }
        }

        let id = self.next_id();
        if register_in_index {
            self.table.index.insert(name.clone(), id);
        }
        self.table.entries.push(SymbolEntry {
            id,
            name,
            kind,
            visibility,
            def_span,
            exported_externally: false, // populated by apply_external_exports (DR-08)
        });
        Some(id)
    }
}

impl<'ast> Visit<'ast> for TopLevelCollector {
    /// Only process top-level items — do NOT recurse into expression bodies.
    #[allow(clippy::too_many_lines)] // exhaustive match over all Item variants with synthesis
    fn visit_item(&mut self, i: &'ast ridge_ast::Item) {
        match i {
            Item::Import(_) => {
                // Imports are skipped — they do not add to the symbol table.
            }
            Item::Fn(d) => {
                let vis = resolve_visibility(d.vis, &d.name.text);
                self.push(
                    d.name.text.clone(),
                    SymbolKind::Fn {
                        caps: d.caps.clone(),
                    },
                    vis,
                    d.span,
                    true,
                );
            }
            Item::Const(d) => {
                let vis = resolve_visibility(d.vis, &d.name.text);
                self.push(d.name.text.clone(), SymbolKind::Const, vis, d.span, true);
            }
            Item::Type(d) => {
                let vis = resolve_visibility(d.vis, &d.name.text);
                let arity = d.params.len().try_into().unwrap_or(u32::MAX);
                let type_id_opt = self.push(
                    d.name.text.clone(),
                    SymbolKind::Type { arity },
                    vis,
                    d.span,
                    true,
                );

                // Only synthesise constructors/accessors if the type entry was
                // successfully registered (no R005 on the type itself).
                let Some(type_id) = type_id_opt else { return };

                match &d.body {
                    ridge_ast::TypeBody::Record(rec) => {
                        // Record auto-constructor: same name as the type.
                        // NOT registered in `index` (type's name is already there).
                        let ctor_arity = rec.fields.len().try_into().unwrap_or(u32::MAX);
                        self.push(
                            d.name.text.clone(),
                            SymbolKind::Constructor {
                                owner_type: type_id,
                                variant: 0,
                                arity: ctor_arity,
                                is_record: true,
                            },
                            vis,
                            rec.span,
                            false, // NOT in index
                        );

                        // One FieldAccessor per field — NOT in index.
                        for field in &rec.fields {
                            self.push(
                                field.name.text.clone(),
                                SymbolKind::FieldAccessor {
                                    owner_type: type_id,
                                    field: field.name.text.clone(),
                                },
                                vis,
                                field.span,
                                false, // NOT in index
                            );
                        }
                    }
                    ridge_ast::TypeBody::Union(union_body) => {
                        for (idx, alt) in union_body.alternatives.iter().enumerate() {
                            let variant = idx.try_into().unwrap_or(u32::MAX);
                            match alt {
                                ridge_ast::Constructor::Positional { name, args, span } => {
                                    let ctor_arity = args.len().try_into().unwrap_or(u32::MAX);
                                    // Union constructors are registered in index (can collide).
                                    self.push(
                                        name.text.clone(),
                                        SymbolKind::Constructor {
                                            owner_type: type_id,
                                            variant,
                                            arity: ctor_arity,
                                            is_record: false,
                                        },
                                        vis,
                                        *span,
                                        true, // registered in index
                                    );
                                }
                                ridge_ast::Constructor::Record { name, body, span } => {
                                    let ctor_arity =
                                        body.fields.len().try_into().unwrap_or(u32::MAX);
                                    let ctor_id_opt = self.push(
                                        name.text.clone(),
                                        SymbolKind::Constructor {
                                            owner_type: type_id,
                                            variant,
                                            arity: ctor_arity,
                                            is_record: false,
                                        },
                                        vis,
                                        *span,
                                        true, // registered in index
                                    );

                                    // Field-accessors for union record constructors
                                    // use the constructor's SymbolId as owner.
                                    // If the ctor itself collided, skip its fields.
                                    let Some(ctor_id) = ctor_id_opt else { continue };

                                    for field in &body.fields {
                                        self.push(
                                            field.name.text.clone(),
                                            SymbolKind::FieldAccessor {
                                                owner_type: ctor_id,
                                                field: field.name.text.clone(),
                                            },
                                            vis,
                                            field.span,
                                            false, // NOT in index
                                        );
                                    }
                                }
                            }
                        }
                    }
                    ridge_ast::TypeBody::Alias(_) => {
                        // Alias: no constructor synthesis.
                    }
                }
            }
            Item::Actor(d) => {
                let vis = resolve_visibility(d.vis, &d.name.text);

                let mut state_fields: Vec<StateField> = Vec::new();
                let mut handlers: Vec<HandlerSig> = Vec::new();
                let mut has_init = false;

                for member in &d.members {
                    match member {
                        ridge_ast::ActorMember::State(s) => {
                            state_fields.push(StateField {
                                name: s.name.text.clone(),
                                defaulted: s.default.is_some(),
                                def_span: s.span,
                            });
                        }
                        ridge_ast::ActorMember::On(h) => {
                            handlers.push(HandlerSig {
                                name: h.name.text.clone(),
                                caps: h.caps.clone(),
                                def_span: h.span,
                            });
                        }
                        ridge_ast::ActorMember::Init(_) => {
                            has_init = true;
                        }
                        ridge_ast::ActorMember::Mailbox(_) => {
                            // Mailbox config contributes no symbols.
                        }
                    }
                }

                // R021: per §5.1, an actor with no `init` block must default
                // every state field — otherwise the actor is unconstructible.
                // One R021 per undefaulted state field, anchored at the
                // state declaration's span.
                if !has_init {
                    for s in &state_fields {
                        if !s.defaulted {
                            self.errors
                                .push(ResolveError::ActorStateMissingDefaultOrInit {
                                    name: d.name.text.clone(),
                                    span: s.def_span,
                                });
                        }
                    }
                }

                self.push(
                    d.name.text.clone(),
                    SymbolKind::Actor {
                        state: state_fields,
                        handlers,
                    },
                    vis,
                    d.span,
                    true,
                );
            }
        }
        // Do NOT call walk_item — we handle everything inline and do NOT want
        // to recurse into expression bodies (T8 owns that).
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{
        ActorDecl, ActorMember, Block, Body, ConstDecl, Constructor, Expr, FieldDecl, FnDecl,
        Ident, InitDecl, Item, Module, OnHandler, Param, RecordTypeBody, StateDecl, Type, TypeBody,
        TypeDecl, UnionTypeBody, Visibility,
    };

    // ── AST builder helpers ───────────────────────────────────────────────────

    fn sp() -> Span {
        Span::point(0)
    }

    fn id(text: &str) -> Ident {
        Ident {
            text: text.into(),
            span: sp(),
        }
    }

    fn unit_expr() -> Expr {
        Expr::Unit(sp())
    }

    fn empty_block() -> Block {
        Block {
            stmts: vec![],
            span: sp(),
        }
    }

    fn prim_type_int() -> Type {
        Type::Primitive {
            name: ridge_ast::PrimitiveType::Int,
            span: sp(),
        }
    }

    fn empty_module() -> Module {
        Module {
            items: vec![],
            doc: vec![],
            span: sp(),
        }
    }

    fn module_with(items: Vec<Item>) -> Module {
        Module {
            items,
            doc: vec![],
            span: sp(),
        }
    }

    fn fn_item(name: &str, vis: Visibility, caps: Vec<Capability>) -> Item {
        Item::Fn(FnDecl {
            attrs: vec![],
            vis,
            caps,
            name: id(name),
            params: vec![],
            ret: None,
            body: Body::Expr(unit_expr()),
            span: sp(),
            doc: None,
        })
    }

    fn const_item(name: &str, vis: Visibility) -> Item {
        Item::Const(ConstDecl {
            vis,
            name: id(name),
            ty: prim_type_int(),
            value: unit_expr(),
            span: sp(),
            doc: None,
        })
    }

    fn alias_type_item(name: &str, vis: Visibility) -> Item {
        Item::Type(TypeDecl {
            vis,
            name: id(name),
            params: vec![],
            body: TypeBody::Alias(prim_type_int()),
            span: sp(),
            doc: None,
        })
    }

    fn union_type_item(name: &str, vis: Visibility, ctors: Vec<(&str, usize)>) -> Item {
        let alternatives = ctors
            .into_iter()
            .map(|(n, arity)| Constructor::Positional {
                name: id(n),
                args: vec![prim_type_int(); arity],
                span: sp(),
            })
            .collect();
        Item::Type(TypeDecl {
            vis,
            name: id(name),
            params: vec![],
            body: TypeBody::Union(UnionTypeBody {
                alternatives,
                span: sp(),
            }),
            span: sp(),
            doc: None,
        })
    }

    fn record_type_item(name: &str, vis: Visibility, fields: Vec<&str>, params: Vec<&str>) -> Item {
        let field_decls = fields
            .into_iter()
            .map(|f| FieldDecl {
                name: id(f),
                ty: prim_type_int(),
                span: sp(),
            })
            .collect();
        Item::Type(TypeDecl {
            vis,
            name: id(name),
            params: params.into_iter().map(id).collect(),
            body: TypeBody::Record(RecordTypeBody {
                fields: field_decls,
                span: sp(),
            }),
            span: sp(),
            doc: None,
        })
    }

    fn actor_item(
        name: &str,
        vis: Visibility,
        state: Vec<(&str, bool)>,
        handlers: Vec<&str>,
    ) -> Item {
        let mut members: Vec<ActorMember> = Vec::new();
        for (s_name, has_default) in state {
            members.push(ActorMember::State(StateDecl {
                name: id(s_name),
                ty: prim_type_int(),
                default: if has_default { Some(unit_expr()) } else { None },
                span: sp(),
            }));
        }
        for h_name in handlers {
            members.push(ActorMember::On(OnHandler {
                caps: vec![],
                name: id(h_name),
                params: vec![],
                ret: None,
                body: unit_expr(),
                span: sp(),
                doc: None,
            }));
        }
        Item::Actor(ActorDecl {
            vis,
            name: id(name),
            members,
            span: sp(),
            doc: None,
        })
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    // Test 1: empty module → empty table, 0 errors
    #[test]
    fn t1_empty_module() {
        let m = empty_module();
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty());
        assert!(table.entries.is_empty());
        assert!(table.index.is_empty());
    }

    // Test 2: single `fn foo` → 1 entry, kind = Fn, vis = ProjectPrivate
    #[test]
    fn t2_single_fn_private() {
        let m = module_with(vec![fn_item("foo", Visibility::Private, vec![])]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty());
        assert_eq!(table.entries.len(), 1);
        let e = &table.entries[0];
        assert_eq!(e.name, "foo");
        assert!(matches!(e.kind, SymbolKind::Fn { .. }));
        assert_eq!(e.visibility, ResolvedVisibility::ProjectPrivate);
        assert!(table.lookup("foo").is_some());
    }

    // Test 3: `pub fn bar` → vis = Pub
    #[test]
    fn t3_pub_fn() {
        let m = module_with(vec![fn_item("bar", Visibility::Pub, vec![])]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty());
        assert_eq!(table.entries[0].visibility, ResolvedVisibility::Pub);
    }

    // Test 4: `fn _helper` → vis = FilePrivate
    #[test]
    fn t4_underscore_fn_is_file_private() {
        let m = module_with(vec![fn_item("_helper", Visibility::Private, vec![])]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty());
        assert_eq!(table.entries[0].visibility, ResolvedVisibility::FilePrivate);
    }

    // Test 5: single const → kind = Const
    #[test]
    fn t5_single_const() {
        let m = module_with(vec![const_item("PI", Visibility::Private)]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty());
        assert_eq!(table.entries.len(), 1);
        assert!(matches!(table.entries[0].kind, SymbolKind::Const));
    }

    // Test 6: union type `Color = Red | Green | Blue` → 4 entries, 4 index entries
    #[test]
    fn t6_union_type_entries_and_index() {
        let m = module_with(vec![union_type_item(
            "Color",
            Visibility::Private,
            vec![("Red", 0), ("Green", 0), ("Blue", 0)],
        )]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        // 1 Type + 3 Constructors = 4
        assert_eq!(table.entries.len(), 4);
        // All 4 names are in index
        assert!(table.lookup("Color").is_some());
        assert!(table.lookup("Red").is_some());
        assert!(table.lookup("Green").is_some());
        assert!(table.lookup("Blue").is_some());
        assert!(matches!(table.entries[0].kind, SymbolKind::Type { .. }));
        assert!(matches!(
            table.entries[1].kind,
            SymbolKind::Constructor { .. }
        ));
    }

    // Test 7: record type → 1 Type in index, 1 Constructor NOT in index, 2 FieldAccessors NOT in index
    #[test]
    fn t7_record_type_synthesis() {
        let m = module_with(vec![record_type_item(
            "User",
            Visibility::Private,
            vec!["name", "age"],
            vec![],
        )]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        // 1 Type + 1 Constructor + 2 FieldAccessors = 4
        assert_eq!(table.entries.len(), 4);
        // Only "User" in index; no "name"/"age" as top-level
        assert!(table.lookup("User").is_some());
        assert!(table.lookup("name").is_none());
        assert!(table.lookup("age").is_none());
        // Record auto-constructor NOT in index
        let ctor_count = table
            .entries
            .iter()
            .filter(|e| matches!(e.kind, SymbolKind::Constructor { .. }))
            .count();
        assert_eq!(ctor_count, 1);
        let fa_count = table
            .entries
            .iter()
            .filter(|e| matches!(e.kind, SymbolKind::FieldAccessor { .. }))
            .count();
        assert_eq!(fa_count, 2);
        // Type arity = 0
        assert!(matches!(
            table.entries[0].kind,
            SymbolKind::Type { arity: 0 }
        ));
    }

    // Test 8: generic record type `type List a = { items: List a, len: Int }` → arity = 1
    #[test]
    fn t8_generic_record_arity() {
        let m = module_with(vec![record_type_item(
            "List",
            Visibility::Private,
            vec!["items", "len"],
            vec!["a"],
        )]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert!(matches!(
            table.entries[0].kind,
            SymbolKind::Type { arity: 1 }
        ));
    }

    // Test 9: type alias → 1 entry, no constructors synthesised
    #[test]
    fn t9_type_alias_no_constructor() {
        let m = module_with(vec![alias_type_item("Name", Visibility::Private)]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty());
        assert_eq!(table.entries.len(), 1);
        assert!(matches!(
            table.entries[0].kind,
            SymbolKind::Type { arity: 0 }
        ));
    }

    // Test 10: actor with state (defaulted) + handler
    #[test]
    fn t10_actor_with_state_and_handler() {
        let m = module_with(vec![actor_item(
            "Counter",
            Visibility::Private,
            vec![("count", true)],
            vec!["inc"],
        )]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty());
        assert_eq!(table.entries.len(), 1);
        match &table.entries[0].kind {
            SymbolKind::Actor { state, handlers } => {
                assert_eq!(state.len(), 1);
                assert!(state[0].defaulted);
                assert_eq!(handlers.len(), 1);
                assert_eq!(handlers[0].name, "inc");
            }
            other => panic!("expected Actor, got {other:?}"),
        }
    }

    // Test 11: actor with no-default state field and no init block → R021.
    // Per §5.1, an undefaulted state field with no `init` to construct it
    // makes the actor unbuildable; one R021 fires per such field.
    #[test]
    fn t11_actor_state_no_default() {
        let m = module_with(vec![actor_item(
            "Foo",
            Visibility::Private,
            vec![("x", false)],
            vec![],
        )]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code(), "R021");
        match &table.entries[0].kind {
            SymbolKind::Actor { state, .. } => {
                assert_eq!(state.len(), 1);
                assert!(!state[0].defaulted);
            }
            other => panic!("expected Actor, got {other:?}"),
        }
    }

    // Test 12: R005 — duplicate fn names
    #[test]
    fn t12_r005_duplicate_fn() {
        let m = module_with(vec![
            fn_item("foo", Visibility::Private, vec![]),
            fn_item("foo", Visibility::Private, vec![]),
        ]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code(), "R005");
        // First declaration wins
        assert_eq!(table.entries.len(), 1);
    }

    // Test 13: R005 — fn collides with type name
    #[test]
    fn t13_r005_fn_after_type() {
        let m = module_with(vec![
            union_type_item("Color", Visibility::Private, vec![("Red", 0), ("Green", 0)]),
            fn_item("Color", Visibility::Private, vec![]),
        ]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code(), "R005");
        // "Color" type + "Red" + "Green" constructors = 3 entries
        assert_eq!(table.entries.len(), 3);
    }

    // Test 14: overlapping union constructor names
    #[test]
    fn t14_d051_overlapping_union_ctors() {
        let m = module_with(vec![
            union_type_item("A", Visibility::Private, vec![("X", 0), ("Y", 0)]),
            union_type_item("B", Visibility::Private, vec![("X", 0), ("Z", 0)]),
        ]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        // "X" appears in both unions → 1 R005
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code(), "R005");
        // A: 1 Type + 2 Ctors (X, Y) = 3
        // B: 1 Type + 0 (X collides, skipped) + 1 Ctor (Z) = 2
        // Total = 5
        let entry_names: Vec<_> = table.entries.iter().map(|e| &e.name).collect();
        assert_eq!(table.entries.len(), 5, "entries: {entry_names:?}");
    }

    // Test 15: actor with init member — init is NOT added as a handler
    #[test]
    fn t15_actor_init_not_a_handler() {
        let members = vec![
            ActorMember::State(StateDecl {
                name: id("x"),
                ty: prim_type_int(),
                default: None,
                span: sp(),
            }),
            ActorMember::Init(InitDecl {
                caps: vec![],
                params: vec![Param::Bare(id("v"))],
                body: empty_block(),
                span: sp(),
            }),
            ActorMember::On(OnHandler {
                caps: vec![],
                name: id("get"),
                params: vec![],
                ret: None,
                body: unit_expr(),
                span: sp(),
                doc: None,
            }),
        ];
        let m = module_with(vec![Item::Actor(ActorDecl {
            vis: Visibility::Private,
            name: id("Foo"),
            members,
            span: sp(),
            doc: None,
        })]);
        let (table, errors) = collect_symbols(ModuleId(0), &m);
        assert!(errors.is_empty());
        match &table.entries[0].kind {
            SymbolKind::Actor { state, handlers } => {
                assert_eq!(state.len(), 1);
                // init must not appear in handlers
                assert_eq!(handlers.len(), 1);
                assert_eq!(handlers[0].name, "get");
            }
            other => panic!("expected Actor, got {other:?}"),
        }
    }
}
