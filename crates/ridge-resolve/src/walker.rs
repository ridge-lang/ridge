//! Intra-module scope walker (T8, plan §4.5).
//!
//! `ScopeWalker` (private) is a [`ridge_ast::visit::Visit`] implementation that:
//! 1. Maintains a [`ScopeStack`] while descending into every scope-introducing
//!    construct (fn bodies, lambdas, match arms, actor blocks, etc.).
//! 2. Stamps a [`Binding`] into a `Vec<Option<Binding>>` side-table (indexed by
//!    `NodeId.0`) for every identifier and qualified-name *use-site* it encounters.
//! 3. Emits `R010 UnresolvedIdent` when a name is not found.
//! 4. Emits `R011 DuplicateLocal` when the same name is bound twice in one scope.
//! 5. Emits `R017 StateFieldShadowedByLocal` (warn-level) when a let/var inside
//!    an actor handler introduces a name that matches a state field.
//!
//! # Design notes
//!
//! - The walker does NOT handle qualified-name resolution beyond the `ModuleAlias`
//!   case (T9 fills in Result.Ok, Option.Some, etc.).  All other unresolved
//!   qualified names are left as `Binding::Error` (T9 will emit R012/R014).
//! - T10 owns capability binding (`Binding::Capability`) and the capability
//!   list on fn/on/init declarations.  This walker ignores those positions.
//! - T13 owns Levenshtein suggestions; all `R010` errors emitted here carry
//!   `suggestions: vec![]`.

use ridge_ast::{
    decl::{
        ActorDecl, ActorMember, Constructor, FnDecl, InitDecl, OnHandler, Param, StateDecl,
        TypeBody, TypeDecl,
    },
    expr::{FieldInit, LambdaParam, MatchArm, QualifiedName, RecordCtor},
    typeclass::InstanceDecl,
    visit::{walk_block, walk_expr, walk_init_decl, walk_on_handler, walk_type, Visit},
    Block, Body, Expr, Ident, Item, ListPatElem, Module, Pattern, Type,
};
use ridge_lexer::Span;

use crate::{
    error::ResolveError,
    imports::{Binding, EffectiveBinding, ImportResolution},
    node_id::{NodeIdMap, NodeKind},
    qualified,
    scope::{LocalKind, ScopeIndex, ScopeKind, ScopeStack},
    symbol::{ClassMethodIndex, SymbolKind, SymbolTable},
    ModuleId,
};

// ── Or-pattern binding-name helper ──────────────────────────────────────────

/// Render a sorted set of names as a comma-separated list for diagnostics.
fn join_names(names: &std::collections::BTreeSet<String>) -> String {
    names.iter().cloned().collect::<Vec<_>>().join(", ")
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Resolve all use-site identifiers in one module.
///
/// Returns `(bindings, errors)` where:
/// - `bindings` is a `Vec<Option<Binding>>` indexed by `NodeId.0`.
///   `bindings.len() == node_id_map.len()`.
/// - `errors` accumulates `R010`, `R011`, `R017` (and R999 on defensive paths).
///
/// # Inputs
///
/// - `module_id` — the index of this module in the workspace.
/// - `ast` — the parsed module AST (read-only).
/// - `node_id_map` — must have been populated by [`crate::node_id::assign_node_ids`]
///   over the same AST before calling this function.
/// - `symbol_tables` — one per module, indexed by `ModuleId.0`.  Used for
///   module-symbol lookups and qualified-name target resolution.
/// - `module_imports` — import resolutions for this module (from T7).
/// - `class_method_index` — optional workspace-scoped index of class method names.
///   When provided, bare idents that miss all other lookups are checked against
///   the index before emitting R010. Pass `None` in unit tests that do not set
///   up a workspace.
#[must_use]
pub fn resolve_module_uses(
    module_id: ModuleId,
    ast: &Module,
    node_id_map: &NodeIdMap,
    symbol_tables: &[SymbolTable],
    module_imports: &[ImportResolution],
    class_method_index: Option<&ClassMethodIndex>,
    retain: bool,
) -> (Vec<Option<Binding>>, Vec<ResolveError>, ScopeIndex) {
    // Allocate one slot per NodeId (None = "not yet stamped").
    let mut bindings: Vec<Option<Binding>> = vec![None; node_id_map.len()];
    let mut errors: Vec<ResolveError> = Vec::new();

    let my_table = symbol_tables.get(module_id.0 as usize);

    let mut walker = ScopeWalker {
        module_id,
        node_id_map,
        my_table,
        all_symbol_tables: symbol_tables,
        module_imports,
        class_method_index,
        bindings: &mut bindings,
        errors: &mut errors,
        scope: ScopeStack::with_recording(retain),
        in_actor_state_names: Vec::new(),
        or_stamp_only: false,
    };

    walker.visit_module(ast);
    let scopes = walker.scope.take_scope_index();

    (bindings, errors, scopes)
}

// ── ScopeWalker ───────────────────────────────────────────────────────────────

struct ScopeWalker<'a> {
    /// The module being resolved.
    module_id: ModuleId,
    /// `NodeId` map built in Phase A.
    node_id_map: &'a NodeIdMap,
    /// Symbol table for the current module (may be None for empty modules).
    my_table: Option<&'a SymbolTable>,
    /// All modules' symbol tables (for qualified lookups into workspace modules).
    all_symbol_tables: &'a [SymbolTable],
    /// Import resolutions for the current module.
    module_imports: &'a [ImportResolution],
    /// Workspace-scoped class method index (optional — absent in unit tests).
    class_method_index: Option<&'a ClassMethodIndex>,
    /// Output bindings side-table (indexed by NodeId.0).
    bindings: &'a mut Vec<Option<Binding>>,
    /// Accumulated errors.
    errors: &'a mut Vec<ResolveError>,
    /// Lexical scope chain.
    scope: ScopeStack,
    /// State field names visible in the current actor body (pushed when we
    /// enter an actor, popped when we leave).
    in_actor_state_names: Vec<(String, Span)>,
    /// True while binding the non-first alternatives of an or-pattern. In this
    /// mode a repeated name is expected (every alternative binds the same
    /// variables), so the duplicate-local check is suppressed and the ident is
    /// stamped as a reference to the binding introduced by the first
    /// alternative — keeping goto/rename working across alternatives.
    or_stamp_only: bool,
}

// ── Helper methods ────────────────────────────────────────────────────────────

impl ScopeWalker<'_> {
    /// Stamp a binding for the `NodeId` at this span/kind.
    ///
    /// If no `NodeId` was stamped for the position, silently skips (defensive).
    fn stamp(&mut self, span: Span, kind: NodeKind, binding: Binding) {
        if let Some(nid) = self.node_id_map.get(span, kind) {
            let idx = nid.0 as usize;
            if idx < self.bindings.len() {
                self.bindings[idx] = Some(binding);
            }
        }
    }

    /// Resolve an identifier at a use-site and stamp the binding.
    ///
    /// Lookup order:
    /// 1. Local scope chain (innermost → outermost).
    /// 2. Module-level symbol table.
    /// 3. Import effective bindings.
    /// 4. Class method index (lowest precedence — locals and top-level fns shadow methods).
    /// 5. Miss → R010.
    fn resolve_ident(&mut self, id: &Ident) {
        let name = &id.text;
        let span = id.span;

        let binding = if let Some(local) = self.scope.lookup_local(name) {
            Binding::Local(local.id)
        } else if let Some(sym) = self.my_table.and_then(|t| t.lookup(name)) {
            Binding::ModuleSymbol {
                module: self.module_id,
                symbol: sym.id,
            }
        } else if let Some(eb) = self.find_import_binding(name) {
            eb.binding.clone()
        } else if let Some(binding) = self.resolve_class_method(name, span) {
            binding
        } else {
            let suggestions = self.r010_suggestions(name);
            self.errors.push(ResolveError::UnresolvedIdent {
                name: name.clone(),
                suggestions,
                span,
            });
            Binding::Error
        };

        self.stamp(span, NodeKind::Ident, binding);
    }

    /// Try to resolve `name` as a class method via the workspace method index.
    ///
    /// Returns `Some(Binding::ClassMethod { .. })` when the name belongs to exactly
    /// one class. Emits `R024 AmbiguousMethodName` and returns `Some(Binding::Error)`
    /// when two distinct classes declare the same name (caller must not also emit R010
    /// in that case).  Returns `None` when the name is not a class method at all.
    fn resolve_class_method(&mut self, name: &str, span: Span) -> Option<Binding> {
        let index = self.class_method_index?;

        // Check for an ambiguous collision first.
        if let Some((first_class, second_class)) = index.collisions.get(name) {
            self.errors.push(ResolveError::AmbiguousMethodName {
                name: name.to_owned(),
                first_class: first_class.clone(),
                second_class: second_class.clone(),
                span,
            });
            return Some(Binding::Error);
        }

        // Unambiguous method lookup.
        let (class_name, _arity) = index.lookup(name)?;
        Some(Binding::ClassMethod {
            class_name: class_name.to_owned(),
            method: name.to_owned(),
        })
    }

    /// Resolve a qualified name `Head.tail...` and stamp the binding on the
    /// `QualifiedName`'s span.
    ///
    /// Delegates entirely to [`qualified::resolve_qualified_name`] (T9).
    fn resolve_qualified(&mut self, qn: &QualifiedName) {
        let span = qn.span;
        let binding = qualified::resolve_qualified_name(
            qn,
            self.module_id,
            self.my_table,
            self.all_symbol_tables,
            self.module_imports,
            self.class_method_index,
            self.errors,
        );
        self.stamp(span, NodeKind::QualifiedName, binding);
    }

    /// Search the module's effective import bindings for a local name.
    fn find_import_binding(&self, name: &str) -> Option<&EffectiveBinding> {
        self.module_imports
            .iter()
            .flat_map(|ir| &ir.effective_bindings)
            .find(|eb| eb.local_name == name)
    }

    /// Resolve a type-constructor name appearing in a type position and stamp
    /// its binding on the name token.
    ///
    /// Only names that resolve to a `type` declaration — this module's or an
    /// imported one — are stamped; a type variable, a primitive, or an
    /// unresolved name is left without a binding. No diagnostic is emitted on a
    /// miss: type-name resolution for error reporting belongs to the type
    /// checker, so this stays purely additive (it feeds editor features such as
    /// go-to-definition and rename without changing what compiles).
    fn resolve_type_ref(&mut self, name: &Ident) {
        if let Some(binding) = self.lookup_type_binding(&name.text) {
            self.stamp(name.span, NodeKind::Ident, binding);
        }
    }

    /// Resolve a bare name in type position to the binding of the `type` it
    /// names, or `None` when it is not a known type. Locals never participate —
    /// a value binding cannot stand in a type position — so only the module
    /// symbol table and the import bindings are consulted.
    fn lookup_type_binding(&self, name: &str) -> Option<Binding> {
        if let Some(sym) = self.my_table.and_then(|t| t.lookup(name)) {
            return matches!(sym.kind, SymbolKind::Type { .. }).then(|| Binding::ModuleSymbol {
                module: self.module_id,
                symbol: sym.id,
            });
        }
        let eb = self.find_import_binding(name)?;
        self.import_binding_is_type(&eb.binding)
            .then(|| eb.binding.clone())
    }

    /// True when an import effective binding denotes a `type`: a workspace
    /// symbol whose entry is a `Type`, or a stdlib symbol (a stdlib name that
    /// reaches a type position is a type reference). Module aliases and value
    /// symbols are rejected so they never masquerade as a type reference.
    fn import_binding_is_type(&self, binding: &Binding) -> bool {
        match binding {
            Binding::ImportedSymbol { module, symbol, .. }
            | Binding::ModuleSymbol { module, symbol } => self
                .all_symbol_tables
                .get(module.0 as usize)
                .and_then(|t| t.entries.get(symbol.0 as usize))
                .is_some_and(|e| matches!(e.kind, SymbolKind::Type { .. })),
            Binding::StdlibSymbol { .. } => true,
            _ => false,
        }
    }

    /// Visit the type annotation of a parameter, if any. The parameter's own
    /// binders are added separately by [`Self::add_param`]; this only stamps the
    /// type references inside the annotation.
    fn visit_param_type(&mut self, p: &Param) {
        match p {
            Param::Bare(_) => {}
            Param::Annotated { ty, .. } | Param::PatternAnnotated { ty, .. } => self.visit_type(ty),
        }
    }

    /// Stamp type references that appear inside a `type` declaration's body
    /// (record field types, union constructor argument types, alias target).
    /// The declared type name, its parameters, and its constructor names are
    /// declarations, not references, so they are left to the symbol table.
    fn visit_type_decl_refs(&mut self, d: &TypeDecl) {
        match &d.body {
            TypeBody::Record(rb) => {
                for field in &rb.fields {
                    self.visit_type(&field.ty);
                }
            }
            TypeBody::Union(ub) => {
                for alt in &ub.alternatives {
                    match alt {
                        Constructor::Positional { args, .. } => {
                            for arg in args {
                                self.visit_type(arg);
                            }
                        }
                        Constructor::Record { body, .. } => {
                            for field in &body.fields {
                                self.visit_type(&field.ty);
                            }
                        }
                    }
                }
            }
            TypeBody::Alias(ty) => self.visit_type(ty),
        }
    }

    /// Resolve whether `binding` names the construction or pattern-match of an
    /// **opaque** type, returning the [`ModuleId`] that declares it.
    ///
    /// Opacity lives on the type entry and is mirrored on every constructor of
    /// that type. A bare or qualified constructor resolves to a
    /// [`Binding::Constructor`] whose `owner_type` points at the type entry; an
    /// imported constructor resolves to a [`Binding::ImportedSymbol`] pointing
    /// either at the type entry (a record auto-constructor shares the type's
    /// name and id) or at a union-variant constructor entry. Both shapes are
    /// inspected here. Any other binding — or a non-opaque type — yields `None`.
    fn opaque_owner(&self, binding: &Binding) -> Option<ModuleId> {
        match binding {
            Binding::Constructor {
                owner_type,
                owner_module,
                ..
            } => self
                .type_symbol_is_opaque(*owner_module, *owner_type)
                .then_some(*owner_module),
            Binding::ImportedSymbol { module, symbol, .. } => {
                self.imported_opaque_owner(*module, *symbol)
            }
            // Stdlib taint wrappers (e.g. `Sql`, `Html`): opacity is carried on the
            // generated builtin manifest. A stdlib symbol is never reached from
            // inside its own module in user code, so any opaque use is cross-module.
            // The sentinel `ModuleId(u32::MAX)` can never equal a real module id.
            Binding::StdlibSymbol { module, name } => crate::stdlib_builtin::BUILTINS
                .get(module.0 as usize)
                .is_some_and(|m| m.opaque_types.contains(&name.as_str()))
                .then_some(ModuleId(u32::MAX)),
            _ => None,
        }
    }

    /// True iff the entry at `(module, symbol)` is a `Type` declared `opaque`.
    fn type_symbol_is_opaque(&self, module: ModuleId, symbol: crate::SymbolId) -> bool {
        self.all_symbol_tables
            .get(module.0 as usize)
            .and_then(|t| t.entries.get(symbol.0 as usize))
            .is_some_and(|e| matches!(e.kind, SymbolKind::Type { opaque: true, .. }))
    }

    /// Opacity for an imported symbol: the entry is either the type itself (an
    /// imported record auto-constructor) or a union-variant constructor that
    /// mirrors the type's opacity. Returns the declaring module when opaque.
    fn imported_opaque_owner(&self, module: ModuleId, symbol: crate::SymbolId) -> Option<ModuleId> {
        let entry = self
            .all_symbol_tables
            .get(module.0 as usize)
            .and_then(|t| t.entries.get(symbol.0 as usize))?;
        match entry.kind {
            SymbolKind::Type { opaque: true, .. } => Some(module),
            SymbolKind::Constructor {
                opaque: true,
                owner_module,
                ..
            } => Some(owner_module),
            _ => None,
        }
    }

    /// Rewrite an imported-constructor binding to the [`Binding::Constructor`] a
    /// local constructor would get.
    ///
    /// A constructor imported from a workspace module resolves to a
    /// [`Binding::ImportedSymbol`] pointing at the producing module's entry. In
    /// pattern position that has to be stamped as a real constructor, or lowering
    /// emits a wildcard instead of a constructor match — silently turning the
    /// first arm into a catch-all. This is reached in the standard library's own
    /// build, where every `std.*` module is a workspace module so cross-module
    /// constructor imports come back as `ImportedSymbol` rather than the
    /// manifest-driven `StdlibSymbol` a user build sees. Returns `None` for any
    /// binding that is not an imported constructor, so the caller keeps it as is.
    fn imported_constructor_binding(&self, binding: &Binding) -> Option<Binding> {
        let Binding::ImportedSymbol { module, symbol, .. } = binding else {
            return None;
        };
        let entry = self
            .all_symbol_tables
            .get(module.0 as usize)
            .and_then(|t| t.entries.get(symbol.0 as usize))?;
        match entry.kind {
            SymbolKind::Constructor {
                owner_type,
                variant,
                is_record,
                owner_module,
                ..
            } => Some(Binding::Constructor {
                owner_type,
                variant,
                is_record,
                owner_module,
            }),
            _ => None,
        }
    }

    /// O3 gate: a constructor of an opaque type may only build or match a value
    /// inside the module that declares the type. Emits `R025` (construction) or
    /// `R026` (pattern) when the use crosses the defining module boundary.
    /// In-module use (O4) is unrestricted and never fires.
    fn check_opaque_use(
        &mut self,
        binding: &Binding,
        ctor_name: &str,
        span: Span,
        is_pattern: bool,
    ) {
        let Some(owner_module) = self.opaque_owner(binding) else {
            return;
        };
        if owner_module == self.module_id {
            return;
        }
        let ctor_name = ctor_name.to_owned();
        self.errors.push(if is_pattern {
            ResolveError::OpaquePattern { ctor_name, span }
        } else {
            ResolveError::OpaqueConstruct { ctor_name, span }
        });
    }

    /// Visit a `Send.message` payload, treating its head identifier as a
    /// handler-name LABEL (no resolution against current scope).
    ///
    /// Send messages take three syntactic forms (parser builds them via the
    /// `!` postfix arm in `crates/ridge-parser/src/expr.rs`):
    ///
    /// 1. `actor ! handler`            → bare `Expr::Ident(handler)`
    /// 2. `actor ! handler arg ...`    → `Expr::Call { callee: Ident, args }`
    /// 3. anything else (e.g. parenthesised expression — not yet emitted by
    ///    the parser, but defensive) → recurse normally.
    ///
    /// Cases 1 and 2 skip resolving the head Ident — handler validation is
    /// Phase 4's job (it has the actor's `on`-handler list in scope).  This
    /// mirrors how `Expr::Ask { message: Ident }` is silently ignored via
    /// the walker's `visit_ident` no-op.
    fn visit_send_message(&mut self, msg: &Expr) {
        match msg {
            // `actor ! handler` — head is a handler label; no scope lookup.
            Expr::Ident(_) => {}
            // `actor ! handler arg1 arg2 ...` — head is a label, args resolve
            // normally as use-site expressions.
            Expr::Call { callee, args, .. } if matches!(callee.as_ref(), Expr::Ident(_)) => {
                for arg in args {
                    self.visit_expr(arg);
                }
            }
            // Defensive — the parser does not currently emit other shapes for
            // Send.message, but if it ever does (qualified handler, etc.)
            // fall back to full resolution rather than silently ignoring.
            _ => self.visit_expr(msg),
        }
    }

    /// Resolve an actor name at a `spawn` / `child` use-site to an
    /// [`Binding::ActorName`] binding (R010 when the name is missing or is
    /// not an actor).
    fn resolve_actor_name(&mut self, actor: &Ident) -> Binding {
        if let Some(sym) = self.my_table.and_then(|t| t.lookup(&actor.text)) {
            if let SymbolKind::Actor { .. } = &sym.kind {
                Binding::ActorName {
                    module: self.module_id,
                    actor: sym.id,
                }
            } else {
                // Name exists but is not an actor.
                let suggestions = self.r010_suggestions(&actor.text);
                self.errors.push(ResolveError::UnresolvedIdent {
                    name: actor.text.clone(),
                    suggestions,
                    span: actor.span,
                });
                Binding::Error
            }
        } else {
            let suggestions = self.r010_suggestions(&actor.text);
            self.errors.push(ResolveError::UnresolvedIdent {
                name: actor.text.clone(),
                suggestions,
                span: actor.span,
            });
            Binding::Error
        }
    }

    /// True when `callee` was just resolved to the compiler-known
    /// `std.actor.tryAsk` symbol.
    ///
    /// Reads back the binding stamped by the immediately preceding
    /// `visit_expr(callee)`: a bare `tryAsk` (import-list form) stamps an
    /// `Ident` node; an alias-qualified `Actor.tryAsk` stamps a
    /// `QualifiedName` node.
    fn callee_is_std_actor_tryask(&self, callee: &Expr) -> bool {
        let (span, kind) = match callee {
            Expr::Ident(id) => (id.span, NodeKind::Ident),
            Expr::Qualified(qn) => (qn.span, NodeKind::QualifiedName),
            _ => return false,
        };
        let Some(nid) = self.node_id_map.get(span, kind) else {
            return false;
        };
        matches!(
            self.bindings.get(nid.0 as usize),
            Some(Some(Binding::StdlibSymbol { module, name }))
                if name == "tryAsk"
                    && crate::BUILTINS
                        .get(module.0 as usize)
                        .is_some_and(|m| m.name == "std.actor")
        )
    }

    /// Build "did you mean?" candidates for an `R010 UnresolvedIdent` (T13).
    ///
    /// Mirrors the [`Self::resolve_ident`] lookup order:
    /// 1. Locals on the scope chain (innermost → outermost).
    /// 2. Module-level symbol names.
    /// 3. Import effective-binding local names.
    ///
    /// The exact `target` text is excluded so we never suggest the very name
    /// that was just rejected.  Visibility is implicit: scope locals belong
    /// to this resolution (always visible); `my_table` symbols are this
    /// module (always visible to itself); import effective bindings were
    /// already filtered by T7 (no `_private` symbols leak in).  See plan
    /// §11 risk R14.
    /// Build the suggestion list for an `R010` site.  Levenshtein candidates
    /// from `r010_candidates`, augmented with a well-known prelude-shorthand
    /// (e.g. `not` → `Bool.not`) inserted at the front when the user's name
    /// matches one of the cases the bare Levenshtein engine cannot bridge.
    fn r010_suggestions(&self, target: &str) -> Vec<String> {
        let mut suggestions = crate::suggest::suggest(target, self.r010_candidates(target));
        if let Some(shorthand) = crate::suggest::well_known_shorthand(target) {
            suggestions.retain(|s| s != shorthand);
            suggestions.insert(0, shorthand.to_owned());
            suggestions.truncate(crate::suggest::MAX_RESULTS);
        }
        suggestions
    }

    fn r010_candidates(&self, target: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        // 1. Locals.
        for scope in &self.scope.stack {
            for local in &scope.locals {
                if local.name != target {
                    out.push(local.name.clone());
                }
            }
        }

        // 2. Module-level symbols (names lookup-able through the symbol-table
        //    `index` only; auto-constructors / field accessors live in
        //    `entries` but should not be surfaced as use-site suggestions).
        if let Some(table) = self.my_table {
            for entry in &table.entries {
                if entry.name != target {
                    out.push(entry.name.clone());
                }
            }
        }

        // 3. Import effective bindings.
        for ir in self.module_imports {
            for eb in &ir.effective_bindings {
                if eb.local_name != target {
                    out.push(eb.local_name.clone());
                }
            }
        }

        out
    }

    /// Add a parameter (from Param) as a local in the current scope.
    ///
    /// Per R005: handler and init params inside an actor body that
    /// collide with a state field fire `R017 StateFieldShadowedByLocal`
    /// (warn-level).  Top-level fn params naturally skip the check because
    /// `check_r017_state_shadow` early-returns when no actor state is in scope.
    fn add_param(&mut self, p: &Param, kind: LocalKind) {
        match p {
            Param::Bare(name_ident) => {
                self.check_r017_state_shadow(name_ident);
                self.add_local_binding(name_ident, kind);
            }
            Param::Annotated { name, .. } => {
                self.check_r017_state_shadow(name);
                self.add_local_binding(name, kind);
            }
            // A destructuring param binds every variable of its pattern, reusing
            // the same machinery as `let`/`match` patterns. The pattern's
            // irrefutability is enforced later in typecheck.
            Param::PatternAnnotated { pat, .. } => {
                self.bind_pattern(pat, kind);
            }
        }
    }

    /// Attempt to add a local; on R011, emit the error.
    fn add_local_binding(&mut self, ident: &Ident, kind: LocalKind) {
        match self.scope.add_local(ident.text.clone(), ident.span, kind) {
            Ok(local_id) => {
                // Stamp the definition-site ident as Local.
                self.stamp(ident.span, NodeKind::Ident, Binding::Local(local_id));
            }
            Err((existing_id, existing_span)) => {
                // R011: duplicate local in the same scope — unless we are binding
                // a later or-pattern alternative, where a repeated name is the
                // whole point (every alternative binds the same variables). In
                // that mode the site is still stamped as `Local(existing_id)`, so
                // it links to the first alternative's binding.
                if !self.or_stamp_only {
                    self.errors.push(ResolveError::DuplicateLocal {
                        name: ident.text.clone(),
                        first_span: existing_span,
                        second_span: ident.span,
                    });
                }
                // Still stamp the site as Local(existing_id) so downstream sees a binding.
                self.stamp(ident.span, NodeKind::Ident, Binding::Local(existing_id));
            }
        }
    }

    /// Recursively extract all binders from a pattern and add them as locals.
    ///
    /// The `constructor_is_use_site` flag controls whether a `Pattern::Constructor`
    /// name is resolved as a use-site (true in match arms) or skipped (false in
    /// let patterns where we don't have separate Constructor resolution).
    #[allow(clippy::too_many_lines)]
    fn bind_pattern(&mut self, pat: &Pattern, kind: LocalKind) {
        match pat {
            Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::ListNil { .. } => {
                // Nothing to bind.
            }
            Pattern::Var { name, .. } => {
                // Check for R017 before adding.
                self.check_r017_state_shadow(name);
                self.add_local_binding(name, kind);
            }
            Pattern::Constructor {
                name, fields, args, ..
            } => {
                // Constructor name at a use-site in a pattern: stamp as Constructor
                // lookup or ModuleSymbol.  We only do a best-effort here —
                // look up `name` in the current module's symbol table.
                if let Some(sym) = self.my_table.and_then(|t| t.lookup(&name.text)) {
                    match &sym.kind {
                        SymbolKind::Constructor {
                            owner_type,
                            variant,
                            is_record,
                            owner_module,
                            ..
                        } => {
                            let (owner, var, is_rec, owner_mod) =
                                (*owner_type, *variant, *is_record, *owner_module);
                            self.stamp(
                                name.span,
                                NodeKind::Ident,
                                Binding::Constructor {
                                    owner_type: owner,
                                    variant: var,
                                    is_record: is_rec,
                                    owner_module: owner_mod,
                                },
                            );
                        }
                        _ if fields.is_some() => {
                            // A record-body pattern `Foo { … }` names the record
                            // type, whose auto-constructor shares the type's name
                            // and is not separately indexed (so `lookup` returns
                            // the type symbol). Stamp it as the record constructor
                            // — owner_type is the type symbol itself — so lowering
                            // builds the map pattern instead of falling back to a
                            // wildcard.
                            self.stamp(
                                name.span,
                                NodeKind::Ident,
                                Binding::Constructor {
                                    owner_type: sym.id,
                                    variant: 0,
                                    is_record: true,
                                    owner_module: self.module_id,
                                },
                            );
                        }
                        _ => {
                            // Could be a type with the same name as its record constructor.
                            self.stamp(
                                name.span,
                                NodeKind::Ident,
                                Binding::ModuleSymbol {
                                    module: self.module_id,
                                    symbol: sym.id,
                                },
                            );
                        }
                    }
                } else if let Some(eb) = self.find_import_binding(&name.text) {
                    // An imported constructor resolves to an `ImportedSymbol`;
                    // rewrite it to a `Constructor` binding so pattern lowering
                    // builds a real constructor match rather than a wildcard.
                    let b = self
                        .imported_constructor_binding(&eb.binding)
                        .unwrap_or_else(|| eb.binding.clone());
                    self.check_opaque_use(&b, &name.text, name.span, true);
                    self.stamp(name.span, NodeKind::Ident, b);
                } else {
                    // R010: unknown constructor name in pattern.
                    let suggestions = self.r010_suggestions(&name.text);
                    self.errors.push(ResolveError::UnresolvedIdent {
                        name: name.text.clone(),
                        suggestions,
                        span: name.span,
                    });
                    self.stamp(name.span, NodeKind::Ident, Binding::Error);
                }

                // Bind field pattern variables.
                if let Some(fps) = fields {
                    for fp in fps {
                        // fp.name is the field name — a use-site, not a binding.
                        // fp.pattern (if Some) contains the actual binder.
                        if let Some(inner) = &fp.pattern {
                            self.bind_pattern(inner, kind);
                        } else {
                            // Shorthand: `{ age }` binds `age` as a local.
                            self.check_r017_state_shadow(&fp.name);
                            self.add_local_binding(&fp.name, kind);
                        }
                    }
                }

                // Bind positional sub-pattern variables.
                for arg in args {
                    self.bind_pattern(arg, kind);
                }
            }
            Pattern::Tuple { elems, .. } => {
                for elem in elems {
                    self.bind_pattern(elem, kind);
                }
            }
            Pattern::Cons { head, tail, .. } => {
                self.bind_pattern(head, kind);
                self.bind_pattern(tail, kind);
            }
            Pattern::As { name, inner, .. } => {
                // `name @` binds `name`.
                self.check_r017_state_shadow(name);
                self.add_local_binding(name, LocalKind::AsAlias);
                self.bind_pattern(inner, kind);
            }
            Pattern::Paren { inner, .. } => {
                self.bind_pattern(inner, kind);
            }
            // Bracketed list pattern — bind each element in place. This handles
            // prefix, middle, and suffix rest uniformly; `desugar_list` only
            // expresses prefix rest (it cannot represent a suffix as cons), so
            // binding the elements directly is what registers a suffix/middle
            // binder such as the `last` in `[.., last]`.
            Pattern::List { elements, .. } => {
                for elem in elements {
                    match elem {
                        ListPatElem::Elem(p) => self.bind_pattern(p, kind),
                        ListPatElem::Rest {
                            bind: Some(name), ..
                        } => {
                            self.check_r017_state_shadow(name);
                            self.add_local_binding(name, kind);
                        }
                        ListPatElem::Rest { bind: None, .. } => {}
                    }
                }
            }
            Pattern::Record { fields, .. } => {
                for fp in fields {
                    if let Some(inner) = &fp.pattern {
                        self.bind_pattern(inner, kind);
                    } else {
                        // Shorthand `{ age }` — bind `age` as a local.
                        self.check_r017_state_shadow(&fp.name);
                        self.add_local_binding(&fp.name, kind);
                    }
                }
            }
            Pattern::Or { alts, .. } => {
                // Every alternative must bind the same variables (R027).
                self.check_or_pattern_same_vars(alts);
                // Bind the first alternative normally; stamp the rest without
                // re-binding (`or_stamp_only`), so a shared name links to the
                // first alternative's binding and no spurious R011 fires.
                if let Some((first, rest)) = alts.split_first() {
                    self.bind_pattern(first, kind);
                    let prev = self.or_stamp_only;
                    self.or_stamp_only = true;
                    for alt in rest {
                        self.bind_pattern(alt, kind);
                    }
                    self.or_stamp_only = prev;
                }
            }
        }
    }

    /// Emit R027 for any or-pattern alternative that binds a different set of
    /// variables than the first alternative.
    fn check_or_pattern_same_vars(&mut self, alts: &[Pattern]) {
        let Some(first) = alts.first() else {
            return;
        };
        let expected = first.bound_var_names();
        for alt in &alts[1..] {
            let bound = alt.bound_var_names();
            if bound != expected {
                self.errors.push(ResolveError::OrPatternBindingMismatch {
                    bound: join_names(&bound),
                    expected: join_names(&expected),
                    span: alt.span(),
                });
            }
        }
    }

    /// Emit R017 if `ident` shadows an actor state field in the current actor body.
    fn check_r017_state_shadow(&mut self, ident: &Ident) {
        // Later or-pattern alternatives re-mention names the first alternative
        // already checked; skip so the warning is not emitted once per alternative.
        if self.or_stamp_only {
            return;
        }
        // Only meaningful when inside an actor body.
        if self.in_actor_state_names.is_empty() {
            return;
        }
        if let Some((_, field_span)) = self
            .in_actor_state_names
            .iter()
            .find(|(name, _)| *name == ident.text)
        {
            let field_span = *field_span;
            self.errors.push(ResolveError::StateFieldShadowedByLocal {
                name: ident.text.clone(),
                local_span: ident.span,
                field_span,
            });
        }
    }

    /// Walk a `LambdaParam`, binding any variables it introduces.
    fn bind_lambda_param(&mut self, lp: &LambdaParam) {
        match lp {
            LambdaParam::Pattern(p) => self.bind_pattern(p, LocalKind::LambdaParam),
            LambdaParam::Annotated { pat, ty, .. } => {
                self.bind_pattern(pat, LocalKind::LambdaParam);
                self.visit_type(ty);
            }
        }
    }
}

// ── Visit impl ────────────────────────────────────────────────────────────────

impl<'ast> Visit<'ast> for ScopeWalker<'_> {
    // ── Module top level ──────────────────────────────────────────────────────

    fn visit_module(&mut self, m: &'ast Module) {
        // Push a module-level scope (imports live here, not as locals).
        self.scope.push_with_start(ScopeKind::Module, m.span.start);
        for item in &m.items {
            self.visit_item(item);
        }
        self.scope.pop_into(m.span.end);
    }

    fn visit_item(&mut self, i: &'ast Item) {
        match i {
            // Imports and class declarations are handled by other passes; skip
            // in the use-site resolver.
            Item::Import(_) | Item::ClassDecl(_) => {}
            // A type declaration introduces no use-sites for the value resolver,
            // but its body references other types (record field types, union
            // constructor arguments, an alias target). Stamp those so type
            // references inside type definitions are findable too.
            Item::Type(d) => self.visit_type_decl_refs(d),
            // Instance method bodies are resolved so that use-sites inside them
            // (module fns, locals, prelude constructors, and — crucially for
            // parametric instances — bare class-method calls on the constrained
            // variable) bind correctly during lowering.
            Item::InstanceDecl(d) => self.visit_instance_decl(d),
            Item::Const(d) => {
                // Const type annotation: stamp the type references it names.
                self.visit_type(&d.ty);
                // Const value: resolve use-sites in the value expression.
                // The const name itself is a module symbol, not a local.
                self.visit_expr(&d.value);
            }
            Item::Fn(d) => self.visit_fn_decl(d),
            Item::Actor(d) => self.visit_actor_decl(d),
        }
    }

    // ── Function declarations ─────────────────────────────────────────────────

    fn visit_fn_decl(&mut self, d: &'ast FnDecl) {
        // The function name is a module-level symbol — do not add as local here
        // (it was already added to the SymbolTable by T6).  When this fn_decl
        // comes from an InnerFn, the caller adds the name to the enclosing scope.

        // Push FnBody scope and bind parameters.
        self.scope.push_with_start(ScopeKind::FnBody, d.span.start);
        for param in &d.params {
            self.add_param(param, LocalKind::FnParam);
            self.visit_param_type(param);
        }

        // Stamp type references in the return annotation (it has no scope
        // implications, so the order relative to the body does not matter).
        if let Some(ret) = &d.ret {
            self.visit_type(ret);
        }

        // Walk the body. Body::Ffi has no expression child to walk — T3 handles
        // its validation.
        if let Body::Expr(e) = &d.body {
            self.visit_expr(e);
        }
        self.scope.pop_into(d.span.end);
    }

    // ── Instance declarations ─────────────────────────────────────────────────

    /// Resolve use-sites inside each instance method body.
    ///
    /// Each method is walked like a function body: a fresh `FnBody` scope is
    /// pushed, its parameters are bound as locals, and the body expression is
    /// visited. This binds module-fn calls, locals, prelude constructors, and
    /// bare class-method calls (`encode e` inside `instance Encode (List a)`)
    /// so the lowering pass can thread dictionaries through them.
    fn visit_instance_decl(&mut self, d: &'ast InstanceDecl) {
        for method in &d.methods {
            self.scope
                .push_with_start(ScopeKind::FnBody, method.span.start);
            for param in &method.params {
                self.add_param(param, LocalKind::FnParam);
            }
            self.visit_expr(&method.body);
            self.scope.pop_into(method.span.end);
        }
    }

    // ── Actor declarations ────────────────────────────────────────────────────

    fn visit_actor_decl(&mut self, d: &'ast ActorDecl) {
        // Push an ActorBody scope.
        self.scope
            .push_with_start(ScopeKind::ActorBody, d.span.start);

        // Collect state field names for R017 detection before walking members.
        let state_fields: Vec<(String, Span)> = d
            .members
            .iter()
            .filter_map(|m| match m {
                ActorMember::State(s) => Some((s.name.text.clone(), s.name.span)),
                _ => None,
            })
            .collect();

        // Add state fields as locals in the ActorBody scope.
        for (name, span) in &state_fields {
            // Use add_local directly; span refers to the state decl ident.
            if let Ok(local_id) = self
                .scope
                .add_local(name.clone(), *span, LocalKind::StateField)
            {
                self.stamp(*span, NodeKind::Ident, Binding::Local(local_id));
            }
            // Err: duplicate state field — R005 handles this in T6, not T8.
        }

        // Push state field names into the actor shadow-detection list.
        let prev_state = std::mem::replace(&mut self.in_actor_state_names, state_fields);

        // Walk all members (init, on-handlers, state default exprs).
        for member in &d.members {
            self.visit_actor_member(member);
        }

        // Restore previous state list (handles nested actors if they ever exist).
        self.in_actor_state_names = prev_state;
        self.scope.pop_into(d.span.end);
    }

    fn visit_actor_member(&mut self, m: &'ast ActorMember) {
        match m {
            ActorMember::State(s) => self.visit_state_decl(s),
            ActorMember::Init(i) => self.visit_init_decl(i),
            ActorMember::On(h) => self.visit_on_handler(h),
            ActorMember::Mailbox(_) => {
                // Mailbox config has no identifier references to resolve.
            }
        }
    }

    fn visit_state_decl(&mut self, d: &'ast StateDecl) {
        // Stamp type references in the state field's type annotation.
        self.visit_type(&d.ty);
        // Walk the default expression if present.
        if let Some(default) = &d.default {
            self.visit_expr(default);
        }
    }

    fn visit_init_decl(&mut self, d: &'ast InitDecl) {
        self.scope
            .push_with_start(ScopeKind::InitBlock, d.span.start);
        for param in &d.params {
            self.add_param(param, LocalKind::InitParam);
        }
        walk_init_decl(self, d);
        self.scope.pop_into(d.span.end);
    }

    fn visit_on_handler(&mut self, h: &'ast OnHandler) {
        self.scope
            .push_with_start(ScopeKind::OnHandler, h.span.start);
        for param in &h.params {
            self.add_param(param, LocalKind::HandlerParam);
        }
        walk_on_handler(self, h);
        self.scope.pop_into(h.span.end);
    }

    // ── Block ─────────────────────────────────────────────────────────────────

    fn visit_block(&mut self, b: &'ast Block) {
        self.scope.push_with_start(ScopeKind::Block, b.span.start);
        walk_block(self, b);
        self.scope.pop_into(b.span.end);
    }

    // ── Match arm ─────────────────────────────────────────────────────────────

    fn visit_match_arm(&mut self, arm: &'ast MatchArm) {
        self.scope
            .push_with_start(ScopeKind::MatchArm, arm.span.start);
        // Bind all pattern variables.
        self.bind_pattern(&arm.pattern, LocalKind::PatternBinding);
        // Walk the optional guard.
        if let Some(guard) = &arm.guard {
            self.visit_expr(guard);
        }
        // Walk the body.
        self.visit_expr(&arm.body);
        self.scope.pop_into(arm.span.end);
    }

    // ── Expressions ───────────────────────────────────────────────────────────

    // visit_expr is an exhaustive match over all Expr variants — cannot be split.
    #[allow(clippy::too_many_lines)]
    fn visit_expr(&mut self, e: &'ast Expr) {
        match e {
            Expr::Ident(id) => {
                // Plain identifier use-site.
                self.resolve_ident(id);
            }

            Expr::Qualified(qn) => {
                // Qualified name use-site (Io.println, List.map, etc.).
                self.resolve_qualified(qn);
                // Do NOT also call walk_qualified_name — the segments are part of
                // the qualified name, not independent use-site Idents.
            }

            Expr::FieldAccessorFn { field, .. } => {
                // `(.name)` — stamp as FieldAccessor without scope lookup.
                self.stamp(
                    field.span,
                    NodeKind::Ident,
                    Binding::FieldAccessor {
                        field: field.text.clone(),
                    },
                );
            }

            Expr::Spawn { actor, args, .. } => {
                // Resolve actor name as ActorName binding.
                let actor_binding = self.resolve_actor_name(actor);
                self.stamp(actor.span, NodeKind::Ident, actor_binding);
                for arg in args {
                    self.visit_expr(arg);
                }
            }

            Expr::ChildSpec { actor, args, .. } => {
                // Same actor-name resolution as `spawn` — the spec is
                // typed against the actor's TyCon at type-check time.
                let actor_binding = self.resolve_actor_name(actor);
                self.stamp(actor.span, NodeKind::Ident, actor_binding);
                for arg in args {
                    self.visit_expr(arg);
                }
            }

            Expr::Record {
                constructor,
                fields,
                ..
            } => {
                // T8 (Phase 4 §3.8): constructor is now RecordCtor::Bare or RecordCtor::Qualified.
                // Bare form: existing bare-ctor resolution logic unchanged.
                // Qualified form: new code path via resolve_qualified_record_constructor.
                match constructor {
                    RecordCtor::Bare(ctor_ident) => {
                        // Record construction OR a bare UPPER_IDENT (const, type, ctor, import alias).
                        // The parser produces Expr::Record for every UPPER_IDENT not followed by '.' or '{'.
                        // Resolve in priority order:
                        // 1. Module symbol table (handles constructors, types, fn/const with UPPER names)
                        // 2. Import effective bindings (handles module aliases like `List`, `Io`)
                        // 3. Scope locals (unlikely for UPPER_IDENT but possible)
                        // 4. R010 miss.
                        let ctor_binding = if let Some(sym) =
                            self.my_table.and_then(|t| t.lookup(&ctor_ident.text))
                        {
                            match &sym.kind {
                                SymbolKind::Constructor {
                                    owner_type,
                                    variant,
                                    is_record,
                                    owner_module,
                                    ..
                                } => {
                                    let (owner, var, is_rec, owner_mod) =
                                        (*owner_type, *variant, *is_record, *owner_module);
                                    Binding::Constructor {
                                        owner_type: owner,
                                        variant: var,
                                        is_record: is_rec,
                                        owner_module: owner_mod,
                                    }
                                }
                                // For all other symbol kinds (Type, Fn, Const, Actor, FieldAccessor)
                                // stamp as ModuleSymbol.
                                _ => Binding::ModuleSymbol {
                                    module: self.module_id,
                                    symbol: sym.id,
                                },
                            }
                        } else if let Some(eb) = self.find_import_binding(&ctor_ident.text) {
                            // A constructor imported from a workspace module, or a
                            // module alias (e.g. `List` from `import std.list as
                            // List`). For an imported constructor, rewrite the
                            // `ImportedSymbol` to a `Constructor` so it lowers to
                            // the constructor's tag rather than a cross-module
                            // symbol reference; aliases and other symbols are left
                            // unchanged.
                            self.imported_constructor_binding(&eb.binding)
                                .unwrap_or_else(|| eb.binding.clone())
                        } else if let Some(local) = self.scope.lookup_local(&ctor_ident.text) {
                            Binding::Local(local.id)
                        } else {
                            let suggestions = self.r010_suggestions(&ctor_ident.text);
                            self.errors.push(ResolveError::UnresolvedIdent {
                                name: ctor_ident.text.clone(),
                                suggestions,
                                span: ctor_ident.span,
                            });
                            Binding::Error
                        };
                        self.check_opaque_use(
                            &ctor_binding,
                            &ctor_ident.text,
                            ctor_ident.span,
                            false,
                        );
                        self.stamp(ctor_ident.span, NodeKind::Ident, ctor_binding);
                    }
                    RecordCtor::Qualified(qn) => {
                        // Qualified record constructor: Http.Response { ... }
                        // Delegate to resolve_qualified_record_constructor which walks the
                        // module-alias chain and verifies the final segment is a Constructor.
                        let binding = qualified::resolve_qualified_record_constructor(
                            qn,
                            self.module_id,
                            self.my_table,
                            self.all_symbol_tables,
                            self.module_imports,
                            self.class_method_index,
                            self.errors,
                        );
                        let ctor_name = qn.segments.last().map_or("", |s| s.text.as_str());
                        self.check_opaque_use(&binding, ctor_name, qn.span, false);
                        self.stamp(qn.span, NodeKind::Ident, binding);
                    }
                }
                for fi in fields {
                    self.visit_field_init(fi);
                }
            }

            Expr::Lambda { params, body, .. } => {
                self.scope
                    .push_with_start(ScopeKind::Lambda, e.span().start);
                for lp in params {
                    self.bind_lambda_param(lp);
                }
                self.visit_expr(body);
                self.scope.pop_into(e.span().end);
            }

            Expr::InnerFn { decl, .. } => {
                // Inner fn name is added to the enclosing scope, then the fn body
                // gets its own FnBody scope.
                self.add_local_binding(&decl.name, LocalKind::FnParam);
                self.scope
                    .push_with_start(ScopeKind::FnBody, decl.span.start);
                for param in &decl.params {
                    self.add_param(param, LocalKind::FnParam);
                    self.visit_param_type(param);
                }
                if let Some(ret) = &decl.ret {
                    self.visit_type(ret);
                }
                // Inner fns always have Body::Expr; Body::Ffi is only valid at
                // module top-level (enforced in T3).
                if let Body::Expr(inner) = &decl.body {
                    self.visit_expr(inner);
                }
                self.scope.pop_into(decl.span.end);
            }

            Expr::Let { pat, ty, value, .. } => {
                // Stamp type references in the optional annotation.
                if let Some(t) = ty {
                    self.visit_type(t);
                }
                // Walk the RHS first (before the pattern is in scope).
                self.visit_expr(value);
                // Bind pattern vars into the current scope.
                self.bind_pattern(pat, LocalKind::LetImmutable);
            }

            Expr::Var {
                name, ty, value, ..
            } => {
                // Stamp type references in the optional annotation.
                if let Some(t) = ty {
                    self.visit_type(t);
                }
                // Walk the RHS first.
                self.visit_expr(value);
                // Bind `name` into the current scope (R017 check included).
                self.check_r017_state_shadow(name);
                self.add_local_binding(name, LocalKind::VarMutable);
            }

            Expr::Try { block, .. } => {
                self.scope
                    .push_with_start(ScopeKind::TryBlock, e.span().start);
                walk_block(self, block);
                self.scope.pop_into(e.span().end);
            }

            Expr::Guard {
                cond, else_branch, ..
            } => {
                self.visit_expr(cond);
                self.scope
                    .push_with_start(ScopeKind::GuardElse, else_branch.span.start);
                walk_block(self, else_branch);
                self.scope.pop_into(else_branch.span.end);
            }

            Expr::Send {
                handle, message, ..
            } => {
                // The handle is a normal use-site expression — resolve it
                // (typically an `Ident` bound to a `Local` or `ActorName`).
                self.visit_expr(handle);
                // The HEAD of `message` is a handler-name LABEL (BEAM atom),
                // resolved against the target actor's on-handler list at
                // type-check time (Phase 4), NOT against the current lexical
                // scope.  Mirror `Expr::Ask`'s behaviour — Ask has
                // `message: Ident` and the walker's `visit_ident` no-op
                // already silently skips it — so Send's head Ident must
                // also be skipped to avoid spurious `R010 UnresolvedIdent`.
                self.visit_send_message(message);
            }

            Expr::Call { callee, args, .. } => {
                // Resolve the callee first so its binding is stamped; then
                // decide whether this call targets the compiler-known
                // `std.actor.tryAsk`.
                self.visit_expr(callee);
                if self.callee_is_std_actor_tryask(callee) {
                    // `tryAsk handle message timeoutMs` — the message
                    // argument's head is a handler-name LABEL, exactly like
                    // `!` / `?>` (the type checker validates it against the
                    // target actor's `on` handlers), so it must not be
                    // resolved against the lexical scope. Parens are peeled
                    // first: `tryAsk h (shorten url) 1000`.
                    if let Some((handle, rest)) = args.split_first() {
                        self.visit_expr(handle);
                        if let Some((message, rest)) = rest.split_first() {
                            let mut msg = message;
                            while let Expr::Paren { inner, .. } = msg {
                                msg = inner;
                            }
                            self.visit_send_message(msg);
                            for arg in rest {
                                self.visit_expr(arg);
                            }
                        }
                    }
                } else {
                    for arg in args {
                        self.visit_expr(arg);
                    }
                }
            }

            // Default: delegate to the standard walk_expr which recurses into children.
            _ => walk_expr(self, e),
        }
    }

    // ── Field init ────────────────────────────────────────────────────────────

    fn visit_field_init(&mut self, fi: &'ast FieldInit) {
        // The field name in a record-construction expression is NOT a use-site
        // Ident (it's a structural label); skip stamping it.  Only the value
        // expression (if present) contains use-site names.
        if let Some(val) = &fi.value {
            self.visit_expr(val);
        } else {
            // Shorthand `{ age }` — the field name is also a use-site Ident.
            self.resolve_ident(&fi.name);
        }
    }

    // ── Type positions ────────────────────────────────────────────────────────

    /// Stamp a binding on every type-constructor name in a type expression.
    ///
    /// Use-site type names (`User` in `x: User`, the head of `Map k v`, a record
    /// field's type) resolve to the `type` they name; type variables and
    /// primitives name no declaration and are skipped. Structural shapes (tuple,
    /// list, function, parenthesised) carry no name of their own and recurse
    /// into their children. The reachability of this method is what gives types
    /// the same editor support as values: go-to-definition, find-references, and
    /// document highlight all read the bindings stamped here.
    fn visit_type(&mut self, t: &'ast Type) {
        match t {
            Type::Named { name, .. } => self.resolve_type_ref(name),
            Type::App { head, args, .. } => {
                self.resolve_type_ref(head);
                for arg in args {
                    self.visit_type(arg);
                }
            }
            // Inline record: field names are labels and the row tail is a
            // variable; only the field types carry references.
            Type::Record { fields, .. } => {
                for field in fields {
                    self.visit_type(&field.ty);
                }
            }
            // A type variable or a built-in primitive names no declaration.
            Type::Var { .. } | Type::Primitive { .. } => {}
            // Tuple, list, function, and parenthesised types have no head of
            // their own — recurse into their component types.
            Type::Tuple { .. } | Type::List { .. } | Type::Fn { .. } | Type::Paren { .. } => {
                walk_type(self, t);
            }
        }
    }

    // ── Ident: no-op (we handle idents explicitly in visit_expr) ─────────────

    fn visit_ident(&mut self, _i: &'ast Ident) {
        // Intentionally empty.  This visitor handles Idents explicitly in
        // visit_expr and bind_pattern.  The default `walk_*` helpers will NOT
        // be invoked for ident positions that we handle directly.
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        collect_symbols,
        imports::{resolve_imports, ImportResolutionResult},
        module_graph::build_module_graph,
        node_id::assign_node_ids,
        ModuleId,
    };
    use ridge_parser::parse_source;
    use std::fs;
    use tempfile::TempDir;

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn parse_mod(src: &str) -> Module {
        parse_source(src).module
    }

    fn write_file(dir: &std::path::Path, rel: &str, content: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("dirs");
        }
        fs::write(full, content).expect("write");
    }

    fn workspace_toml(members: &[&str]) -> String {
        let list = members
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [{list}]\n")
    }

    fn project_toml(name: &str) -> String {
        format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\nkind = \"library\"\n")
    }

    /// Full pipeline: discover → `build_module_graph` → `collect_symbols` → `resolve_imports` → `assign_node_ids` → `resolve_module_uses`.
    /// Returns (bindings, `resolve_errors`, `import_resolution`).
    #[allow(clippy::type_complexity)]
    fn full_resolve_single(
        src: &str,
    ) -> (
        Vec<Option<Binding>>,
        Vec<ResolveError>,
        ImportResolutionResult,
        NodeIdMap,
    ) {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(td.path(), "libs/proj/src/Main.ridge", src);

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("workspace");
        let g = build_module_graph(&ws);

        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();

        let import_result = resolve_imports(&mut ws, &g, &symbol_tables);

        let pm = g.modules.first().expect("module 0");
        let (node_id_map, _nid_errors) = assign_node_ids(&pm.ast);
        let module_imports = import_result
            .imports
            .first()
            .map_or([].as_slice(), Vec::as_slice);

        let (bindings, errors, _scopes) = resolve_module_uses(
            pm.id,
            &pm.ast,
            &node_id_map,
            &symbol_tables,
            module_imports,
            None,
            false,
        );

        drop(td);
        (bindings, errors, import_result, node_id_map)
    }

    /// Resolve a bare module AST (no workspace, no imports).
    fn resolve_bare(src: &str) -> (Vec<Option<Binding>>, Vec<ResolveError>, NodeIdMap) {
        let m = parse_mod(src);
        let (nid_map, _) = assign_node_ids(&m);
        let module_id = ModuleId(0);
        let (table, _) = collect_symbols(module_id, &m);
        let all_tables = vec![table];
        let (bindings, errors, _scopes) =
            resolve_module_uses(module_id, &m, &nid_map, &all_tables, &[], None, false);
        (bindings, errors, nid_map)
    }

    fn count_binding<F: Fn(&Binding) -> bool>(bindings: &[Option<Binding>], f: F) -> usize {
        bindings.iter().flatten().filter(|b| f(b)).count()
    }

    fn count_errors<F: Fn(&ResolveError) -> bool>(errors: &[ResolveError], f: F) -> usize {
        errors.iter().filter(|e| f(e)).count()
    }

    // ── Test 1: plain ident resolves to Local ─────────────────────────────────

    #[test]
    fn t1_ident_resolves_to_local() {
        // `fn f x = x` — the `x` in body should bind to Local(0).
        let (bindings, errors, _nid) = resolve_bare("fn f x = x\n");
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let local_count = count_binding(&bindings, |b| matches!(b, Binding::Local(_)));
        // `x` at def site + `x` at use site = 2 Local stamps; `f` is ModuleSymbol.
        assert!(
            local_count >= 1,
            "expected ≥1 Local binding for x; got {local_count}"
        );
    }

    // ── Test 2: plain ident resolves to ModuleSymbol ──────────────────────────

    #[test]
    fn t2_ident_resolves_to_module_symbol() {
        // `fn myFunc x = x` then `fn f = myFunc 1` — lower-case fn name is Expr::Ident.
        let src = "fn myFunc x = x\nfn f = myFunc 1\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let ms_count = count_binding(&bindings, |b| matches!(b, Binding::ModuleSymbol { .. }));
        assert!(
            ms_count >= 1,
            "expected ≥1 ModuleSymbol for myFunc, got {ms_count}"
        );
    }

    // ── Test 3: qualified name → StdlibSymbol ────────────────────────────────

    #[test]
    fn t3_qualified_name_resolves_to_stdlib_symbol() {
        let src = "import std.io as Io\nfn foo = Io.println \"hi\"\n";
        let (bindings, errors, _import, _nid) = full_resolve_single(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let stdlib_count = count_binding(&bindings, |b| matches!(b, Binding::StdlibSymbol { .. }));
        assert!(
            stdlib_count >= 1,
            "expected ≥1 StdlibSymbol for Io.println, got {stdlib_count}"
        );
    }

    // ── Test 4: qualified name → ImportedSymbol (2-module workspace) ──────────

    #[test]
    fn t4_qualified_name_resolves_to_imported_symbol() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(
            td.path(),
            "libs/proj/src/A.ridge",
            "import proj.B as B\nfn useB = B.helper\n",
        );
        write_file(td.path(), "libs/proj/src/B.ridge", "pub fn helper = ()\n");

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();

        let import_result = resolve_imports(&mut ws, &g, &symbol_tables);

        // Find module A.
        let a_pm = g
            .modules
            .iter()
            .find(|pm| {
                // FQN ends with ".A" segment — not a file extension; allow lint.
                #[allow(clippy::case_sensitive_file_extension_comparisons)]
                ws.modules[pm.id.0 as usize]
                    .fully_qualified_name
                    .ends_with(".A")
            })
            .expect("module A");

        let a_imports = import_result
            .imports
            .get(a_pm.id.0 as usize)
            .map_or([].as_slice(), Vec::as_slice);

        let (nid_map, _) = assign_node_ids(&a_pm.ast);
        let (bindings, errors, _scopes) = resolve_module_uses(
            a_pm.id,
            &a_pm.ast,
            &nid_map,
            &symbol_tables,
            a_imports,
            None,
            false,
        );

        assert!(errors.is_empty(), "A: unexpected errors: {errors:?}");
        let imported = count_binding(&bindings, |b| matches!(b, Binding::ImportedSymbol { .. }));
        assert!(imported >= 1, "expected ≥1 ImportedSymbol, got {imported}");
        drop(td);
    }

    // ── Test 5: R010 UnresolvedIdent ─────────────────────────────────────────

    #[test]
    fn t5_r010_unresolved_ident() {
        let (bindings, errors, _nid) = resolve_bare("fn f = nonexistent\n");
        let r010_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::UnresolvedIdent { .. })
        });
        assert_eq!(r010_count, 1, "expected 1 R010; got: {errors:?}");
        let error_count = count_binding(&bindings, |b| matches!(b, Binding::Error));
        assert!(error_count >= 1, "expected Binding::Error for nonexistent");
    }

    /// Bare `not` is a famous prelude-shorthand expectation (Python/JS/Haskell);
    /// Ridge intentionally only exposes `Bool.not`.  The R010 suggestion list
    /// must surface `Bool.not` ahead of any junk Levenshtein candidates so the
    /// user gets a usable hint instead of `Int / Io / Set`.
    #[test]
    fn r010_not_suggests_bool_not_first() {
        let (_, errors, _nid) = resolve_bare("fn f x = not x\n");
        let r010 = errors
            .iter()
            .find_map(|e| match e {
                ResolveError::UnresolvedIdent {
                    name, suggestions, ..
                } if name == "not" => Some(suggestions.clone()),
                _ => None,
            })
            .expect("expected an R010 for `not`");
        assert_eq!(
            r010.first().map(String::as_str),
            Some("Bool.not"),
            "well-known shorthand must be first; got: {r010:?}"
        );
    }

    #[test]
    fn r010_print_suggests_io_println_first() {
        let (_, errors, _nid) = resolve_bare("fn f x = print x\n");
        let r010 = errors
            .iter()
            .find_map(|e| match e {
                ResolveError::UnresolvedIdent {
                    name, suggestions, ..
                } if name == "print" => Some(suggestions.clone()),
                _ => None,
            })
            .expect("expected an R010 for `print`");
        assert_eq!(
            r010.first().map(String::as_str),
            Some("Io.println"),
            "well-known shorthand must be first; got: {r010:?}"
        );
    }

    // ── Test 6: R011 DuplicateLocal (fn params) ───────────────────────────────

    #[test]
    fn t6_r011_duplicate_param() {
        // `fn f x x = x + x` — second `x` is a duplicate in the same FnBody scope.
        let (_, errors, _nid) = resolve_bare("fn f x x = x + x\n");
        let r011_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::DuplicateLocal { .. })
        });
        assert_eq!(
            r011_count, 1,
            "expected 1 R011 for duplicate param; got: {errors:?}"
        );
    }

    // ── Test 7: R011 DuplicateLocal (same-block let) ──────────────────────────

    #[test]
    fn t7_r011_duplicate_let_in_block() {
        // `fn f =\n  let x = 1\n  let x = 2\n  x` — same Block scope, second x is R011.
        let src = "fn f =\n    let x = 1\n    let x = 2\n    x\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::DuplicateLocal { .. })
        });
        assert_eq!(
            r011_count, 1,
            "expected 1 R011 for duplicate let; got: {errors:?}"
        );
    }

    // ── Test 8: cross-scope shadowing is OK (no R011) ────────────────────────

    #[test]
    fn t8_cross_scope_shadowing_ok() {
        // Lambda param `x` shadows outer fn param `x` — no R011.
        let src = "fn f x = (fn x -> x + 1) 5\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::DuplicateLocal { .. })
        });
        assert_eq!(r011_count, 0, "cross-scope shadow must not produce R011");
    }

    // ── Test 9: pattern binding in match arm ──────────────────────────────────

    #[test]
    fn t9_pattern_binding_in_match_arm() {
        let src = "fn f p =\n    match p\n        (a, b) -> a + b\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        // a and b in the body bind to Local (pattern bindings from the match arm).
        let r010_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "a" || name == "b"),
        );
        assert_eq!(
            r010_count, 0,
            "a and b from pattern must be in scope; errors: {errors:?}"
        );
        let local_count = count_binding(&bindings, |b| matches!(b, Binding::Local(_)));
        assert!(
            local_count >= 2,
            "expected ≥2 Local for a and b, got {local_count}"
        );
    }

    // ── Test 10: lambda with stdlib qualified name ────────────────────────────

    #[test]
    fn t10_lambda_local_and_qualified_stdlib() {
        let src = "import std.list as List\nfn f xs = List.map (fn x -> x + 1) xs\n";
        let (bindings, errors, _, _) = full_resolve_single(src);
        let r010_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::UnresolvedIdent { .. })
        });
        assert_eq!(r010_count, 0, "no R010 expected; errors: {errors:?}");
        // `xs` in the body binds to Local (fn param).
        let local_count = count_binding(&bindings, |b| matches!(b, Binding::Local(_)));
        assert!(
            local_count >= 1,
            "expected ≥1 Local for xs or x; got {local_count}"
        );
        // List.map → StdlibSymbol.
        let stdlib_count = count_binding(&bindings, |b| matches!(b, Binding::StdlibSymbol { .. }));
        assert!(
            stdlib_count >= 1,
            "expected ≥1 StdlibSymbol; got {stdlib_count}"
        );
    }

    // ── Test 11: spawn resolves actor name ───────────────────────────────────

    #[test]
    fn t11_spawn_resolves_actor_name() {
        let src =
            "actor Limiter =\n    state x: Int = 0\n    on inc = x + 1\nfn start = spawn Limiter\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        let r010_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::UnresolvedIdent { .. })
        });
        // We expect 0 R010 for Limiter in spawn.
        assert_eq!(
            r010_count, 0,
            "Limiter must resolve as ActorName; errors: {errors:?}"
        );
        let actor_count = count_binding(&bindings, |b| matches!(b, Binding::ActorName { .. }));
        assert!(
            actor_count >= 1,
            "expected ActorName binding for Limiter, got {actor_count}"
        );
    }

    // ── Test 12: actor on-handler can see state ───────────────────────────────

    #[test]
    fn t12_actor_on_handler_sees_state() {
        let src = "actor X =\n    state count: Int = 0\n    on inc = count + 1\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        let r010_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "count"),
        );
        assert_eq!(
            r010_count, 0,
            "count in on handler must be in scope; errors: {errors:?}"
        );
        let local_count = count_binding(&bindings, |b| matches!(b, Binding::Local(_)));
        assert!(
            local_count >= 1,
            "expected ≥1 Local for count; got {local_count}"
        );
    }

    // ── Test 13: R017 StateFieldShadowedByLocal ───────────────────────────────

    #[test]
    fn t13_r017_state_field_shadowed_by_local() {
        // `let count = 5` inside an on-handler shadows state field `count`.
        // We use a var binding (var count = 5) which is a Var expression.
        let src = "actor X =\n    state count: Int = 0\n    on inc =\n        var count = 5\n        count\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r017_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::StateFieldShadowedByLocal { .. })
        });
        assert!(r017_count >= 1, "expected R017; got: {errors:?}");
    }

    // ── Test 14: bindings vec length == node_id_map length ───────────────────

    #[test]
    fn t14_bindings_length_equals_node_id_count() {
        let src = "fn add x y = x + y\n";
        let (bindings, _, nid) = resolve_bare(src);
        assert_eq!(
            bindings.len(),
            nid.len(),
            "bindings.len() must equal node_id_map.len()"
        );
    }

    // ── Test 15: FieldAccessorFn stamps FieldAccessor ────────────────────────

    #[test]
    fn t15_field_accessor_fn_stamps_field_accessor() {
        let src = "import std.list as List\nfn f xs = xs |> List.map (.name)\n";
        let (bindings, _, _, _) = full_resolve_single(src);
        let fa_count = count_binding(&bindings, |b| matches!(b, Binding::FieldAccessor { .. }));
        assert!(
            fa_count >= 1,
            "expected ≥1 FieldAccessor binding; got {fa_count}"
        );
    }

    // ── Test 16: module-level const use (lower-case name) ────────────────────

    #[test]
    fn t16_const_use_resolves_to_module_symbol() {
        // Use lower-case const name so it parses as Expr::Ident (not Expr::Record).
        let src = "const maxValue: Int = 100\nfn check n = n < maxValue\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let ms_count = count_binding(&bindings, |b| matches!(b, Binding::ModuleSymbol { .. }));
        assert!(ms_count >= 1, "expected ModuleSymbol for maxValue");
    }

    // ── Test 17: Var binding (mutable) ───────────────────────────────────────

    #[test]
    fn t17_var_binding_local() {
        let src = "fn f =\n    var counter = 0\n    counter + 1\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let local_count = count_binding(&bindings, |b| matches!(b, Binding::Local(_)));
        assert!(local_count >= 1, "expected Local for counter");
    }

    // ── Test 18: tuple pattern binds both vars ────────────────────────────────

    #[test]
    fn t18_tuple_pattern_binds_both_vars() {
        let src = "fn f pair =\n    let (a, b) = pair\n    a + b\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        let r010_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "a" || name == "b"),
        );
        assert_eq!(
            r010_count, 0,
            "a and b must be in scope; errors: {errors:?}"
        );
        let local_count = count_binding(&bindings, |b| matches!(b, Binding::Local(_)));
        assert!(
            local_count >= 2,
            "expected ≥2 Locals for a and b; got {local_count}"
        );
    }

    // ── Test 19: cons pattern binds head and tail ─────────────────────────────

    #[test]
    fn t19_cons_pattern_binds_head_tail() {
        let src = "fn f xs =\n    match xs\n        h :: t -> h\n        _ -> 0\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r010_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "h" || name == "t"),
        );
        assert_eq!(
            r010_count, 0,
            "h and t must be in scope; errors: {errors:?}"
        );
    }

    // ── Test 20: as-pattern binds alias ──────────────────────────────────────

    #[test]
    fn t20_as_pattern_binds_alias() {
        // `fn f p = match p\n  whole @ _ -> whole`
        let src = "fn f p =\n    match p\n        whole @ _ -> whole\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r010_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "whole"),
        );
        assert_eq!(r010_count, 0, "whole must be in scope as AsAlias");
    }

    // ── Test 21: ModuleAlias resolves ─────────────────────────────────────────

    #[test]
    fn t21_module_alias_resolves() {
        let src = "import std.list as List\nfn f = List\n";
        let (bindings, errors, _, _) = full_resolve_single(src);
        let alias_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "List"),
        );
        assert_eq!(
            alias_count, 0,
            "List alias must be visible; errors: {errors:?}"
        );
        let ma_count = count_binding(&bindings, |b| matches!(b, Binding::ModuleAlias { .. }));
        assert!(ma_count >= 1, "expected ModuleAlias for List");
    }

    // ── Test 22: guard else pushes scope ─────────────────────────────────────

    #[test]
    fn t22_guard_else_scope() {
        let src = "fn f x =\n    guard x > 0 else\n        0\n    x + 1\n";
        let (_, errors, _nid) = resolve_bare(src);
        assert!(errors.is_empty(), "guard else scope errors: {errors:?}");
    }

    // ── Test 23: try block pushes scope ──────────────────────────────────────

    #[test]
    fn t23_try_block_scope() {
        // try block — body may have let bindings scoped to the try.
        let src = "fn f x =\n    try\n        let y = x + 1\n        y\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r010 = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "y"),
        );
        assert_eq!(r010, 0, "y must be in scope inside try; errors: {errors:?}");
    }

    // ── Test 24: inner fn creates nested scope ────────────────────────────────

    #[test]
    fn t24_inner_fn_nested_scope() {
        let src = "fn outer x =\n    fn inner y = x + y\n    inner 10\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r010_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::UnresolvedIdent { .. })
        });
        assert_eq!(r010_count, 0, "x and y must resolve; errors: {errors:?}");
    }

    // ── Test 25: empty module produces no errors ──────────────────────────────

    #[test]
    fn t25_empty_module_no_errors() {
        let (bindings, errors, nid) = resolve_bare("");
        assert!(errors.is_empty(), "empty module: errors: {errors:?}");
        assert_eq!(bindings.len(), nid.len());
        assert_eq!(nid.len(), 0);
    }

    // ── Test 26: multiple params all bound ────────────────────────────────────

    #[test]
    fn t26_multiple_params_all_bound() {
        let src = "fn f a b c = a + b + c\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let local_count = count_binding(&bindings, |b| matches!(b, Binding::Local(_)));
        // a, b, c each appear at def + use = at least 3 use-site Locals
        assert!(
            local_count >= 3,
            "expected ≥3 Local stamps; got {local_count}"
        );
    }

    // ── Test 27: record construction resolves constructor ─────────────────────

    #[test]
    fn t27_record_construction_resolves_constructor() {
        let src = "type Point = { x: Int, y: Int }\nfn origin = Point { x = 0, y = 0 }\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        // Point is either ModuleSymbol (type record) — constructor shares type name.
        let ms_or_ctor = count_binding(&bindings, |b| {
            matches!(
                b,
                Binding::ModuleSymbol { .. } | Binding::Constructor { .. }
            )
        });
        assert!(
            ms_or_ctor >= 1,
            "expected ModuleSymbol or Constructor for Point; errors: {errors:?}"
        );
    }

    // ── Test 28: shorthand field initializer in record construction ──────────

    #[test]
    fn t28_record_shorthand_field_resolves_local() {
        // `{ x }` shorthand — `x` in FieldInit is a use-site Ident.
        let src = "type Point = { x: Int, y: Int }\nfn make x y = Point { x, y }\n";
        let (_, errors, _nid) = resolve_bare(src);
        // x and y from the shorthand must resolve to the fn params, not R010.
        let r010_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "x" || name == "y"),
        );
        assert_eq!(
            r010_count, 0,
            "shorthand x/y must resolve; errors: {errors:?}"
        );
    }

    // ── Shadowing + DuplicateLocal policy (§4.8) ─────────────────────────────
    //
    //   • Cross-scope shadowing is permitted silently (R002).
    //   • Same-scope duplicate bindings are R011 DuplicateLocal — including
    //     duplicates within a single tuple/cons/match-arm pattern.
    //   • R017 StateFieldShadowedByLocal is warn-level for actor-state vs
    //     handler-local shadowing (R005).
    //   • The `_`-prefix convention (`_unused`) is just a name — same-scope
    //     duplicates of `_x` and `_x` still fire R011 (it is a same-scope
    //     bug, not "intentional shadowing").
    //   • Wildcard `_` patterns are NOT bindings — repeating `_` is fine.

    // ── R011 fires for duplicate vars in a single tuple pattern ──────────────

    #[test]
    fn t11_r011_duplicate_var_in_tuple_pattern() {
        // `let (x, x) = pair` — both vars added to the same Block scope; the
        // second hits the duplicate-name guard.
        let src = "fn f pair =\n    let (x, x) = pair\n    x\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::DuplicateLocal { name, .. } if name == "x"),
        );
        assert_eq!(
            r011_count, 1,
            "expected 1 R011 for duplicate tuple-pattern var; got: {errors:?}"
        );
    }

    // ── R011 fires for duplicate vars in a single match arm ──────────────────

    #[test]
    fn t11_r011_duplicate_var_in_match_arm_pattern() {
        // `match e { (x, x) -> 0; _ -> 1 }` — both vars share one MatchArm scope.
        let src = "fn f p =\n    match p\n        (x, x) -> x\n        _      -> 0\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::DuplicateLocal { name, .. } if name == "x"),
        );
        assert_eq!(
            r011_count, 1,
            "expected 1 R011 for duplicate match-arm-pattern var; got: {errors:?}"
        );
    }

    // ── cross-scope shadowing of let → no R011 ───────────────────────────────

    #[test]
    fn t11_cross_scope_let_shadow_silent() {
        // Inner block re-binds `x`; per R002 this is silent.
        // We use the existing tuple-pattern lift trick: `let (x, _) = (1, 0)`
        // is in one Block, and the lambda body is a NEW scope.
        let src = "fn f =\n    let x = 1\n    (fn _y -> let x = 2 in x) 0\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::DuplicateLocal { .. })
        });
        assert_eq!(
            r011_count, 0,
            "cross-scope let shadowing must be silent; errors: {errors:?}"
        );
    }

    // ── R017 fires for `var` shadowing state field ───────────────────────────

    #[test]
    fn t11_r017_var_shadows_state_field() {
        // Inside an `on` handler, `var count = 5` shadows state field `count`.
        let src = "actor X =\n    state count: Int = 0\n    on inc =\n        var count = 5\n        count\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r017_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::StateFieldShadowedByLocal { name, .. } if name == "count"),
        );
        assert_eq!(
            r017_count, 1,
            "expected 1 R017 for var shadowing state field; got: {errors:?}"
        );

        // R017 must be reported as warn-level severity (R005).
        let r017_warns = errors
            .iter()
            .filter(|e| {
                matches!(e, ResolveError::StateFieldShadowedByLocal { .. })
                    && e.severity() == crate::Severity::Warning
            })
            .count();
        assert_eq!(r017_warns, 1, "R017 must carry Severity::Warning");
    }

    // ── R017 fires for handler-param shadowing state field ───────────────────

    #[test]
    fn t11_r017_handler_param_shadows_state_field() {
        // `on set (count: Int)` — handler param shadows state field `count`.
        let src = "actor X =\n    state count: Int = 0\n    on set (count: Int) =\n        count\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r017_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::StateFieldShadowedByLocal { name, .. } if name == "count"),
        );
        assert!(
            r017_count >= 1,
            "expected R017 for handler param shadowing state field; got: {errors:?}"
        );
    }

    // ── R017 does NOT fire when locals do not name a state field ─────────────

    #[test]
    fn t11_r017_silent_when_no_actual_shadow() {
        // State `count`, handler binds an unrelated `delta`. No R017 expected.
        let src = "actor X =\n    state count: Int = 0\n    on add (delta: Int) =\n        let next = count + delta\n        next\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r017_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::StateFieldShadowedByLocal { .. })
        });
        assert_eq!(
            r017_count, 0,
            "R017 must not fire when locals do not collide with state; errors: {errors:?}"
        );
    }

    // ── same-scope `_x`/`_x` duplicate is still R011 ────────────────────────
    //
    // The `_`-prefix convention is a marker for *intentional* shadowing
    // or unused bindings — it does NOT carve out R011, which catches genuine
    // same-scope duplicates that almost always indicate a typo.

    #[test]
    fn t11_r011_underscore_prefixed_dup_still_fires() {
        let src = "fn f _x _x = _x\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::DuplicateLocal { name, .. } if name == "_x"),
        );
        assert_eq!(
            r011_count, 1,
            "_x/_x duplicate must still fire R011; errors: {errors:?}"
        );
    }

    // ── T11 test 8: repeated wildcard `_` in pattern is fine ──────────────────
    //
    // `_` is `Pattern::Wildcard`, not `Pattern::Var` — it binds nothing, so
    // repeating it can never produce R011.

    #[test]
    fn t11_repeated_wildcard_no_r011() {
        let src = "fn f triple =\n    let (_, _, x) = triple\n    x\n";
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::DuplicateLocal { .. })
        });
        assert_eq!(
            r011_count, 0,
            "repeated wildcard `_` must never fire R011; errors: {errors:?}"
        );
    }

    // ── T11 fixture loaders: assert that fixture files fire expected codes ────
    //
    // These lock T11's DoD: `tests/fixtures/resolve/r011_*.ridge` and `r017_*.ridge`
    // each fire exactly the expected diagnostic.  T15 will later add a generic
    // `-- expect: Rxxx` harness over the whole directory; until then these
    // inline checks guarantee the fixtures stay in sync with T11 behaviour.

    #[test]
    fn t11_fixture_r011_duplicate_param() {
        let src = include_str!("../tests/fixtures/resolve/r011_duplicate_param.ridge");
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::DuplicateLocal { .. })
        });
        assert_eq!(
            r011_count, 1,
            "fixture must fire 1 R011; errors: {errors:?}"
        );
    }

    #[test]
    fn t11_fixture_r011_duplicate_let() {
        let src = include_str!("../tests/fixtures/resolve/r011_duplicate_let.ridge");
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::DuplicateLocal { .. })
        });
        assert_eq!(
            r011_count, 1,
            "fixture must fire 1 R011; errors: {errors:?}"
        );
    }

    #[test]
    fn t11_fixture_r011_duplicate_pattern_var() {
        let src = include_str!("../tests/fixtures/resolve/r011_duplicate_pattern_var.ridge");
        let (_, errors, _nid) = resolve_bare(src);
        let r011_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::DuplicateLocal { .. })
        });
        assert_eq!(
            r011_count, 1,
            "fixture must fire 1 R011; errors: {errors:?}"
        );
    }

    #[test]
    fn t11_fixture_r017_let_shadows_state() {
        let src = include_str!("../tests/fixtures/resolve/r017_let_shadows_state.ridge");
        let (_, errors, _nid) = resolve_bare(src);
        let r017_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::StateFieldShadowedByLocal { .. })
        });
        assert_eq!(
            r017_count, 1,
            "fixture must fire 1 R017; errors: {errors:?}"
        );
    }

    #[test]
    fn t11_fixture_r017_handler_param_shadows_state() {
        let src = include_str!("../tests/fixtures/resolve/r017_handler_param_shadows_state.ridge");
        let (_, errors, _nid) = resolve_bare(src);
        let r017_count = count_errors(&errors, |e| {
            matches!(e, ResolveError::StateFieldShadowedByLocal { .. })
        });
        assert!(
            r017_count >= 1,
            "fixture must fire R017; errors: {errors:?}"
        );
    }

    // ── T13 acceptance: R010 carries Levenshtein "did you mean?" suggestions ─

    /// `fn f counter = countr` — typo `countr` should suggest `counter` (the
    /// in-scope fn parameter), distance 1.
    #[test]
    fn t13_r010_suggests_local_parameter() {
        let (_, errors, _nid) = resolve_bare("fn f counter = countr\n");
        let suggestions = errors
            .iter()
            .find_map(|e| match e {
                ResolveError::UnresolvedIdent {
                    name, suggestions, ..
                } if name == "countr" => Some(suggestions.clone()),
                _ => None,
            })
            .expect("expected R010 for `countr`");
        assert!(
            suggestions.contains(&"counter".to_owned()),
            "R010 must suggest `counter`; got {suggestions:?}"
        );
    }

    /// Module-level fn name typo: `fn helper = ...; fn caller = helpr` — the
    /// suggestion list must include the visible module-level symbol `helper`.
    #[test]
    fn t13_r010_suggests_module_symbol() {
        let src = "fn helper x = x\nfn caller = helpr\n";
        let (_, errors, _nid) = resolve_bare(src);
        let suggestions = errors
            .iter()
            .find_map(|e| match e {
                ResolveError::UnresolvedIdent {
                    name, suggestions, ..
                } if name == "helpr" => Some(suggestions.clone()),
                _ => None,
            })
            .expect("expected R010 for `helpr`");
        assert!(
            suggestions.contains(&"helper".to_owned()),
            "R010 must suggest `helper`; got {suggestions:?}"
        );
    }

    /// Beyond distance 2, no suggestion is produced.
    #[test]
    fn t13_r010_no_suggestion_when_distance_too_large() {
        // `xyzqrs` is distance 5+ from any name in scope (no fn params, no
        // module symbols other than `f`).
        let (_, errors, _nid) = resolve_bare("fn f = xyzqrs\n");
        let suggestions = errors
            .iter()
            .find_map(|e| match e {
                ResolveError::UnresolvedIdent {
                    name, suggestions, ..
                } if name == "xyzqrs" => Some(suggestions.clone()),
                _ => None,
            })
            .expect("expected R010 for `xyzqrs`");
        assert!(
            suggestions.is_empty(),
            "no suggestion expected for `xyzqrs`; got {suggestions:?}"
        );
    }

    /// `crate::suggest` truncates to 3 results — even with > 3 close
    /// candidates, R010 carries at most 3 suggestions.
    #[test]
    fn t13_r010_suggestion_caps_at_three() {
        // Six fn params each one edit away from `xx`: x1/x2/x3/x4/x5/x6.
        let src = "fn f x1 x2 x3 x4 x5 x6 = xx\n";
        let (_, errors, _nid) = resolve_bare(src);
        let suggestions = errors
            .iter()
            .find_map(|e| match e {
                ResolveError::UnresolvedIdent {
                    name, suggestions, ..
                } if name == "xx" => Some(suggestions.clone()),
                _ => None,
            })
            .expect("expected R010 for `xx`");
        assert!(
            suggestions.len() <= 3,
            "R010 suggestion list must cap at 3; got {} ({suggestions:?})",
            suggestions.len()
        );
    }

    // ── Class method resolution tests ─────────────────────────────────────────

    /// Helper: resolve a single-module workspace that contains a class declaration.
    /// Passes the class method index so method names can be resolved as
    /// `Binding::ClassMethod` rather than falling through to R010.
    fn resolve_with_class_index(src: &str) -> (Vec<Option<Binding>>, Vec<ResolveError>, NodeIdMap) {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(td.path(), "libs/proj/src/Main.ridge", src);

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("workspace");
        let g = build_module_graph(&ws);

        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();

        let import_result = resolve_imports(&mut ws, &g, &symbol_tables);
        let pm = g.modules.first().expect("module 0");
        let (nid_map, _) = assign_node_ids(&pm.ast);
        let module_imports = import_result
            .imports
            .first()
            .map_or([].as_slice(), Vec::as_slice);

        // Build the class method index from all parsed modules.
        let all_asts: Vec<&Module> = g.modules.iter().map(|pm| &*pm.ast).collect();
        let cmi = crate::symbol::ClassMethodIndex::build(&all_asts);

        let (bindings, errors, _scopes) = resolve_module_uses(
            pm.id,
            &pm.ast,
            &nid_map,
            &symbol_tables,
            module_imports,
            Some(&cmi),
            false,
        );

        drop(td);
        (bindings, errors, nid_map)
    }

    /// `describe Red` — where `describe` is a class method — must resolve to
    /// `Binding::ClassMethod`, NOT to R010 `UnresolvedIdent`.
    #[test]
    fn class_method_bare_call_resolves_to_class_method_binding() {
        let src = r"
class Describe a =
    describe (x: a) -> Text

type Color = Red | Green | Blue

fn main = describe Red
";
        let (bindings, errors, _nid) = resolve_with_class_index(src);

        // Must not produce R010 for `describe`.
        let r010_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "describe"),
        );
        assert_eq!(
            r010_count, 0,
            "describe must not produce R010; errors: {errors:?}"
        );

        // Must produce at least one ClassMethod binding for `describe`.
        let class_method_count = count_binding(
            &bindings,
            |b| matches!(b, Binding::ClassMethod { method, .. } if method == "describe"),
        );
        assert!(
            class_method_count >= 1,
            "expected Binding::ClassMethod for `describe`; bindings: {bindings:?}"
        );
    }

    /// A local fn named like a class method must shadow the method (existing
    /// programs stay green).
    #[test]
    fn local_fn_shadows_class_method() {
        let src = "
class Describe a =
    describe (x: a) -> Text

fn describe x = \"shadowed\"

fn main = describe 42
";
        let (bindings, errors, _nid) = resolve_with_class_index(src);

        // No R010 — `describe` resolves to the local fn.
        let r010 = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "describe"),
        );
        assert_eq!(
            r010, 0,
            "local fn must shadow class method, no R010; errors: {errors:?}"
        );

        // The binding for `describe` in `main` must be ModuleSymbol (the local fn),
        // NOT ClassMethod.
        let class_method_count = count_binding(
            &bindings,
            |b| matches!(b, Binding::ClassMethod { method, .. } if method == "describe"),
        );
        assert_eq!(
            class_method_count, 0,
            "local fn must shadow class method; no ClassMethod binding expected"
        );
    }

    /// Two distinct classes declaring the same method name must produce R024.
    #[test]
    fn two_classes_same_method_name_r024() {
        let src = r"
class Describe a =
    describe (x: a) -> Text

class Show a =
    describe (x: a) -> Text

fn main = describe 42
";
        let (_, errors, _) = resolve_with_class_index(src);

        let r024_count = count_errors(
            &errors,
            |e| matches!(e, ResolveError::AmbiguousMethodName { name, .. } if name == "describe"),
        );
        assert!(
            r024_count >= 1,
            "expected R024 for ambiguous method `describe`; errors: {errors:?}"
        );
    }

    /// Bare calls to prelude methods must resolve to `Binding::ClassMethod`
    /// without any inline `class` declaration in source (the clean public API).
    ///
    /// Checks all five prelude methods: `encode`, `decode`, `toText`, `eq`,
    /// `compare`.
    #[test]
    fn prelude_methods_resolve_without_class_decl() {
        // One fn per prelude method — no `class` declarations anywhere.
        let src = r"
type Person = { name: Text, age: Int }

fn callEncode  (p: Person) -> JsonValue = encode p
fn callDecode  (j: JsonValue) -> Int    = 0
fn callToText  (p: Person) -> Text      = toText p
fn callEq      (a: Int) (b: Int) -> Int = 0
fn callCompare (a: Int) (b: Int) -> Int = 0
";
        let (bindings, errors, _nid) = resolve_with_class_index(src);

        // None of the five names may produce R010.
        for method in &["encode", "decode", "toText", "eq", "compare"] {
            let r010 = count_errors(
                &errors,
                |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == *method),
            );
            assert_eq!(
                r010, 0,
                "`{method}` must not produce R010 without a class declaration; errors: {errors:?}"
            );
        }

        // Each must appear as a ClassMethod binding.
        for method in &["encode", "toText"] {
            let cm = count_binding(
                &bindings,
                |b| matches!(b, Binding::ClassMethod { method: m, .. } if m == *method),
            );
            assert!(
                cm >= 1,
                "expected Binding::ClassMethod for `{method}`; bindings: {bindings:?}"
            );
        }
    }

    /// Redeclaring a prelude class inline (the old workaround) must remain
    /// harmless: no R024 collision, same binding shape.
    #[test]
    fn prelude_class_redecl_is_idempotent() {
        let src = r"
class Encode a =
    encode (x: a) -> JsonValue

type Person = { name: Text, age: Int }

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)
";
        let (bindings, errors, _nid) = resolve_with_class_index(src);

        // No R010 for `encode`.
        let r010 = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "encode"),
        );
        assert_eq!(
            r010, 0,
            "encode must not produce R010 with inline class redecl; errors: {errors:?}"
        );

        // No R024 — same class name means idempotent, not a collision.
        let r024 = count_errors(
            &errors,
            |e| matches!(e, ResolveError::AmbiguousMethodName { name, .. } if name == "encode"),
        );
        assert_eq!(
            r024, 0,
            "redeclaring prelude class must not produce R024; errors: {errors:?}"
        );

        // encode resolves as ClassMethod.
        let cm = count_binding(
            &bindings,
            |b| matches!(b, Binding::ClassMethod { method, .. } if method == "encode"),
        );
        assert!(
            cm >= 1,
            "encode must resolve as ClassMethod; bindings: {bindings:?}"
        );
    }

    // ── O3: opaque-type construction / pattern gate (R025 / R026) ─────────────

    /// Resolve a 2-module workspace under one project `proj`: `main_src` is
    /// `Main.ridge`, `lib_src` is `Lib.ridge`. Returns Main's resolve errors.
    fn resolve_main_with_lib(main_src: &str, lib_src: &str) -> Vec<ResolveError> {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(td.path(), "libs/proj/src/Main.ridge", main_src);
        write_file(td.path(), "libs/proj/src/Lib.ridge", lib_src);

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("workspace");
        let g = build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let import_result = resolve_imports(&mut ws, &g, &symbol_tables);

        let main_pm = g
            .modules
            .iter()
            .find(|pm| {
                #[allow(clippy::case_sensitive_file_extension_comparisons)]
                ws.modules[pm.id.0 as usize]
                    .fully_qualified_name
                    .ends_with(".Main")
            })
            .expect("module Main");
        let main_imports = import_result
            .imports
            .get(main_pm.id.0 as usize)
            .map_or([].as_slice(), Vec::as_slice);

        let (nid_map, _) = assign_node_ids(&main_pm.ast);
        let (_bindings, errors, _scopes) = resolve_module_uses(
            main_pm.id,
            &main_pm.ast,
            &nid_map,
            &symbol_tables,
            main_imports,
            None,
            false,
        );
        drop(td);
        errors
    }

    fn count_opaque<F: Fn(&ResolveError) -> bool>(errors: &[ResolveError], f: F) -> usize {
        errors.iter().filter(|e| f(e)).count()
    }

    #[test]
    fn o3_in_module_opaque_construct_ok() {
        // Building an opaque record inside its defining module is allowed (O4).
        let src = "opaque type Sql = { raw: Text }\nfn make r = Sql { raw = r }\n";
        let (_b, errors, _n) = resolve_bare(src);
        let gated = count_opaque(&errors, |e| {
            matches!(
                e,
                ResolveError::OpaqueConstruct { .. } | ResolveError::OpaquePattern { .. }
            )
        });
        assert_eq!(
            gated, 0,
            "in-module construct must not be gated; {errors:?}"
        );
    }

    #[test]
    fn o3_in_module_opaque_pattern_ok() {
        // Destructuring an opaque record inside its defining module is allowed.
        let src =
            "opaque type Box = { v: Int }\nfn unwrap b =\n    match b\n        Box { v } -> v\n";
        let (_b, errors, _n) = resolve_bare(src);
        let gated = count_opaque(&errors, |e| matches!(e, ResolveError::OpaquePattern { .. }));
        assert_eq!(gated, 0, "in-module pattern must not be gated; {errors:?}");
    }

    #[test]
    fn o3_cross_module_imported_construct_r025() {
        // Headline case: an imported opaque constructor cannot build a value.
        let main = "import proj.Lib (Sql)\nfn make r = Sql { raw = r }\n";
        let lib = "pub opaque type Sql = { raw: Text }\n";
        let errors = resolve_main_with_lib(main, lib);
        let r025 = count_opaque(&errors, |e| {
            matches!(e, ResolveError::OpaqueConstruct { .. })
        });
        assert_eq!(
            r025, 1,
            "expected 1 R025 for imported opaque ctor; {errors:?}"
        );
    }

    #[test]
    fn o3_cross_module_imported_pattern_r026() {
        // An imported opaque constructor cannot be matched cross-module.
        let main =
            "import proj.Lib (Sql)\nfn unwrap s =\n    match s\n        Sql { raw } -> raw\n";
        let lib = "pub opaque type Sql = { raw: Text }\n";
        let errors = resolve_main_with_lib(main, lib);
        let r026 = count_opaque(&errors, |e| matches!(e, ResolveError::OpaquePattern { .. }));
        assert_eq!(
            r026, 1,
            "expected 1 R026 for imported opaque pattern; {errors:?}"
        );
    }

    #[test]
    fn o3_cross_module_qualified_construct_r025() {
        // A qualified opaque constructor `Lib.Sql { .. }` is gated too.
        let main = "import proj.Lib as Lib\nfn make r = Lib.Sql { raw = r }\n";
        let lib = "pub opaque type Sql = { raw: Text }\n";
        let errors = resolve_main_with_lib(main, lib);
        let r025 = count_opaque(&errors, |e| {
            matches!(e, ResolveError::OpaqueConstruct { .. })
        });
        assert_eq!(r025, 1, "expected 1 R025 via qualified ctor; {errors:?}");
    }

    #[test]
    fn o3_cross_module_imported_union_variant_construct_r025() {
        // An opaque union's variant cannot be constructed cross-module.
        let main = "import proj.Lib (Wrap)\nfn make = Wrap 1\n";
        let lib = "pub opaque type Boxed = | Wrap Int\n";
        let errors = resolve_main_with_lib(main, lib);
        let r025 = count_opaque(&errors, |e| {
            matches!(e, ResolveError::OpaqueConstruct { .. })
        });
        assert_eq!(
            r025, 1,
            "expected 1 R025 for opaque union variant; {errors:?}"
        );
    }

    #[test]
    fn o3_cross_module_transparent_construct_ok() {
        // A normal (transparent) imported record constructs fine cross-module.
        let main = "import proj.Lib (Plain)\nfn make = Plain { x = 1 }\n";
        let lib = "pub type Plain = { x: Int }\n";
        let errors = resolve_main_with_lib(main, lib);
        let gated = count_opaque(&errors, |e| {
            matches!(
                e,
                ResolveError::OpaqueConstruct { .. } | ResolveError::OpaquePattern { .. }
            )
        });
        assert_eq!(
            gated, 0,
            "transparent record must construct cross-module; {errors:?}"
        );
    }

    // ── O5: stdlib taint wrappers are opaque (Sql/Html/SecureCookie) ──────────

    #[test]
    fn stdlib_opaque_construct_is_r025() {
        // Forging a `Sql` directly from user code bypasses the escape — rejected.
        let src = "import std.sql (Sql)\nfn f = Sql { value = \"x\" }\n";
        let (_b, errors, _i, _n) = full_resolve_single(src);
        let r025 = count_opaque(&errors, |e| {
            matches!(e, ResolveError::OpaqueConstruct { .. })
        });
        assert_eq!(r025, 1, "stdlib Sql construct must be R025; {errors:?}");
    }

    #[test]
    fn stdlib_opaque_pattern_is_r026() {
        let src = "import std.sql (Sql)\nfn f s =\n    match s\n        Sql { value } -> value\n";
        let (_b, errors, _i, _n) = full_resolve_single(src);
        let r026 = count_opaque(&errors, |e| matches!(e, ResolveError::OpaquePattern { .. }));
        assert_eq!(r026, 1, "stdlib Sql pattern must be R026; {errors:?}");
    }

    #[test]
    fn stdlib_smart_constructor_is_allowed() {
        // The exported factory `sql` is a function, not the opaque constructor.
        let src = "import std.sql (sql)\nfn f = sql \"x\"\n";
        let (_b, errors, _i, _n) = full_resolve_single(src);
        let gated = count_opaque(&errors, |e| {
            matches!(
                e,
                ResolveError::OpaqueConstruct { .. } | ResolveError::OpaquePattern { .. }
            )
        });
        assert_eq!(
            gated, 0,
            "calling the `sql` factory must be allowed; {errors:?}"
        );
    }

    // ── Type-position references ────────────────────────────────────────────────

    /// The binding stamped at the `occurrence`-th (0-based) appearance of
    /// `needle` in `src`, looked up as a `NodeKind::Ident`. `None` when that
    /// position carries no binding.
    fn type_binding_at(
        src: &str,
        bindings: &[Option<Binding>],
        nid: &NodeIdMap,
        needle: &str,
        occurrence: usize,
    ) -> Option<Binding> {
        let mut start = 0usize;
        let mut count = 0usize;
        let offset = loop {
            let rel = src[start..].find(needle)?;
            let at = start + rel;
            if count == occurrence {
                break at;
            }
            count += 1;
            start = at + needle.len();
        };
        let span = Span::new(
            u32::try_from(offset).unwrap(),
            u32::try_from(offset + needle.len()).unwrap(),
        );
        let id = nid.get(span, NodeKind::Ident)?;
        bindings.get(id.0 as usize).cloned().flatten()
    }

    #[test]
    fn type_ref_in_param_annotation_binds() {
        // `Color` in `(c: Color)` (2nd "Color"; 1st is the declaration name).
        let src = "type Color = Red | Green\nfn f (c: Color) = c\n";
        let (bindings, errors, nid) = resolve_bare(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let b = type_binding_at(src, &bindings, &nid, "Color", 1)
            .expect("annotation `Color` should carry a binding");
        assert!(
            matches!(b, Binding::ModuleSymbol { .. }),
            "expected ModuleSymbol for the type annotation, got {b:?}"
        );
    }

    #[test]
    fn type_ref_in_return_annotation_binds() {
        // `Color` in `-> Color` (2nd "Color").
        let src = "type Color = Red | Green\nfn pick -> Color = Red\n";
        let (bindings, errors, nid) = resolve_bare(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let b = type_binding_at(src, &bindings, &nid, "Color", 1)
            .expect("return type `Color` should carry a binding");
        assert!(matches!(b, Binding::ModuleSymbol { .. }), "got {b:?}");
    }

    #[test]
    fn type_ref_in_record_field_binds_even_when_declared_later() {
        // The field type `Color` is referenced before `Color` is declared;
        // top-level collection is order-independent, so it still resolves.
        let src = "type Wrapper = { inner: Color }\ntype Color = Red | Green\n";
        let (bindings, errors, nid) = resolve_bare(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        // "Color" occurrences: field type (0), declaration name (1).
        let b = type_binding_at(src, &bindings, &nid, "Color", 0)
            .expect("record field type `Color` should carry a binding");
        assert!(matches!(b, Binding::ModuleSymbol { .. }), "got {b:?}");
    }

    #[test]
    fn type_variable_is_not_stamped() {
        // The `a` in `(x: a)` is a type variable, not a type reference.
        let src = "fn id (x: a) = x\n";
        let (bindings, errors, nid) = resolve_bare(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(
            type_binding_at(src, &bindings, &nid, "a", 0).is_none(),
            "a type variable must not be stamped with a binding"
        );
    }

    #[test]
    fn unknown_type_ref_is_left_unstamped_without_error() {
        // An unresolved type name carries no binding AND emits no diagnostic —
        // type-name errors belong to the type checker, not this pass.
        let src = "fn f (x: Bogus) = x\n";
        let (bindings, errors, nid) = resolve_bare(src);
        assert!(
            errors.is_empty(),
            "type-position miss must not emit a resolve error: {errors:?}"
        );
        assert!(
            type_binding_at(src, &bindings, &nid, "Bogus", 0).is_none(),
            "an unknown type name must be left without a binding"
        );
    }

    #[test]
    fn type_application_head_and_args_bind() {
        // `Box User` — both the head `Box` and the argument `User` are types.
        let src = "type User = { name: Text }\ntype Box a = Box a\nfn wrap (b: Box User) = b\n";
        let (bindings, errors, nid) = resolve_bare(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        // "Box" in the annotation is the 2nd "Box" (decl name, then annotation).
        let head = type_binding_at(src, &bindings, &nid, "Box", 1)
            .expect("application head `Box` should bind");
        assert!(matches!(head, Binding::ModuleSymbol { .. }), "got {head:?}");
        // "User" in the annotation is the 2nd "User" (decl name, then argument).
        let arg = type_binding_at(src, &bindings, &nid, "User", 1)
            .expect("application argument `User` should bind");
        assert!(matches!(arg, Binding::ModuleSymbol { .. }), "got {arg:?}");
    }

    #[test]
    fn type_decl_name_itself_is_not_stamped() {
        // The declaration name carries no binding here — it is resolved through
        // the symbol table, mirroring how fn/const declaration names work.
        let src = "type Color = Red | Green\nfn f (c: Color) = c\n";
        let (bindings, errors, nid) = resolve_bare(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(
            type_binding_at(src, &bindings, &nid, "Color", 0).is_none(),
            "the `type Color` declaration name must not be stamped by this pass"
        );
    }

    // ── child / tryAsk ────────────────────────────────────────────────────────

    #[test]
    fn child_spec_resolves_actor_name() {
        // `child Counter` stamps the actor name as ActorName, like `spawn`.
        let src = "actor Counter =\n    state count: Int = 0\n\n    on io tick =\n        count <- count + 1\nfn f = child Counter\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let actor_names = count_binding(&bindings, |b| matches!(b, Binding::ActorName { .. }));
        assert_eq!(
            actor_names, 1,
            "expected exactly one ActorName binding for `child Counter`"
        );
    }

    #[test]
    fn child_spec_args_resolve_normally() {
        // The argument list is resolved as ordinary use-site expressions.
        let src = "actor Counter =\n    state count: Int = 0\n\n    on io tick =\n        count <- count + 1\nfn f x = child Counter (x)\n";
        let (bindings, errors, _nid) = resolve_bare(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let actor_names = count_binding(&bindings, |b| matches!(b, Binding::ActorName { .. }));
        assert_eq!(actor_names, 1, "expected one ActorName binding");
    }

    #[test]
    fn child_spec_unknown_actor_r010() {
        let src = "fn f = child Bogus\n";
        let (_bindings, errors, _nid) = resolve_bare(src);
        let r010 = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "Bogus"),
        );
        assert_eq!(r010, 1, "expected one R010 for `Bogus`, got {errors:?}");
    }

    #[test]
    fn tryask_qualified_message_label_not_resolved() {
        // `Actor.tryAsk c tick 1000` — `tick` is a handler-name label, so no
        // R010 is emitted for it even though no such local exists.
        let src = "import std.actor as Actor\nactor Counter =\n    state count: Int = 0\n\n    on io tick =\n        count <- count + 1\nfn f c = Actor.tryAsk c tick 1000\n";
        let (bindings, errors, _imports, _nid) = full_resolve_single(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        // The callee resolved to the stdlib symbol.
        let stdlib_syms = count_binding(
            &bindings,
            |b| matches!(b, Binding::StdlibSymbol { name, .. } if name == "tryAsk"),
        );
        assert_eq!(
            stdlib_syms, 1,
            "expected the tryAsk callee to resolve as StdlibSymbol"
        );
    }

    #[test]
    fn tryask_bare_import_message_label_not_resolved() {
        // `import std.actor (tryAsk)` — the bare form takes the same path.
        let src = "import std.actor (tryAsk)\nactor Counter =\n    state count: Int = 0\n\n    on io tick =\n        count <- count + 1\nfn f c = tryAsk c tick 1000\n";
        let (_bindings, errors, _imports, _nid) = full_resolve_single(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn tryask_payload_args_resolve_normally() {
        // `Actor.tryAsk c (bump x) 1000` — `bump` is the handler label
        // (skipped), but the payload argument `x` is a normal use-site.
        let src = "import std.actor as Actor\nactor Counter =\n    state count: Int = 0\n\n    on io tick =\n        count <- count + 1\nfn f c = Actor.tryAsk c (bump x) 1000\n";
        let (_bindings, errors, _imports, _nid) = full_resolve_single(src);
        let r010_x = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "x"),
        );
        assert_eq!(
            r010_x, 1,
            "expected exactly one R010 for payload `x`, got {errors:?}"
        );
        let r010_bump = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "bump"),
        );
        assert_eq!(
            r010_bump, 0,
            "handler label `bump` must not be resolved: {errors:?}"
        );
    }

    #[test]
    fn non_tryask_call_args_still_resolve() {
        // Without the std.actor import, `Actor.tryAsk` is unresolved and the
        // message argument is visited as an ordinary expression (R010).
        let src = "fn f c = Actor.tryAsk c tick 1000\n";
        let (_bindings, errors, _nid) = resolve_bare(src);
        let r010_tick = count_errors(
            &errors,
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name == "tick"),
        );
        assert_eq!(
            r010_tick, 1,
            "a non-tryAsk call must resolve the message ident normally: {errors:?}"
        );
    }
}
