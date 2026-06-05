//! Qualified-name resolution (plan §4.6).
//!
//! [`resolve_qualified_name`] is the single public entry point.  The walker
//! delegates every `Expr::Qualified` here; the walker only stamps the returned
//! `Binding` and handles the `Ident` case directly.
//!
//! ## Algorithm (§4.6)
//!
//! 1. **Head-segment lookup** — the first segment is always `UPPER_IDENT`.
//!    Search import effective-bindings for `head_text`:
//!    - [`Binding::ModuleAlias`] → descend into target (step 2).
//!    - [`Binding::ImportedSymbol`] whose `SymbolKind` is `Type`/`Actor` →
//!      treat the symbol as the resolved head; descend through it for
//!      constructor/handler lookup (step 3).
//!    - Other binding kinds → `R012`.
//!
//!    If not in imports, search `my_table` for a local `Type`/`Actor` (e.g.
//!    same-module `Result.Ok`).
//!
//!    If still not found → `R012`.  Exception: if the head alias pointed at an
//!    `Unresolved` target, suppress R012 (R011 sentinel).
//!
//! 2. **`ModuleAlias` descent** — take the last segment as the symbol name.
//!    - `BuiltinStdlib(sid)`: check `BUILTINS[sid].exports`.  Hit → `StdlibSymbol`.
//!      Miss → `R014 UnknownStdlibSymbol` with Levenshtein suggestions.
//!    - `WorkspaceModule(mid)`: look up last-seg in `all_symbol_tables[mid.0]`.
//!      Hit → `ImportedSymbol`.  Miss → `R012`.
//!    - `External` / `Unresolved`: silent `Binding::Error` (suppression).
//!
//! 3. **Type/Actor head descent** — `Result.Ok`, `Option.Some`, etc.
//!    - `Type` head: look up `last_seg` among the type's constructors.
//!    - `Actor` head: `R012` (no handler resolution at qualified-name sites).

use ridge_ast::expr::QualifiedName;

use crate::{
    error::ResolveError,
    imports::{Binding, EffectiveBinding, ImportResolution, ImportTarget},
    stdlib_builtin::BUILTINS,
    suggest,
    symbol::{SymbolKind, SymbolTable},
    ModuleId, NodeId, SymbolId,
};

// ── Qualified record constructor resolution (T8, Phase 4 §3.8) ───────────────

/// Resolve a qualified record constructor (`Http.Response { ... }`) and return
/// its [`Binding`].
///
/// ## Algorithm
///
/// The prefix segments `segments[..-1]` are walked via the existing
/// module-alias chain (identical to [`resolve_qualified_name`]'s step 2).
/// The final segment must resolve to a `Constructor` binding inside the target
/// module's symbol table.  If it doesn't, the nearest existing `R###` error
/// code is reused (no new codes per hard constraint §1.3 rule 2):
/// - Unknown path / unknown symbol → `R012 UnresolvedQualifiedName`.
///
/// Errors are pushed into `errors`.  Returns `Binding::Error` on failure.
///
/// This is a **new code path** — the bare-ctor walker path (unchanged) never
/// calls this function.
#[must_use]
pub fn resolve_qualified_record_constructor(
    ctor: &QualifiedName,
    module_id: ModuleId,
    my_table: Option<&SymbolTable>,
    all_symbol_tables: &[SymbolTable],
    module_imports: &[ImportResolution],
    errors: &mut Vec<ResolveError>,
) -> Binding {
    // Delegate to the general qualified-name resolver.
    // The resolver already handles the module-alias chain walk and verifies
    // that the final segment resolves as a Constructor (via `resolve_type_actor_head`).
    // The result is `Binding::Constructor { owner_type, variant }` on success,
    // or `Binding::Error` (with an error pushed) on failure.
    resolve_qualified_name(
        ctor,
        module_id,
        my_table,
        all_symbol_tables,
        module_imports,
        errors,
    )
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Resolve a qualified name and return its [`Binding`].
///
/// Errors are pushed into `errors`; the caller stamps the returned binding into
/// the side-table at `qn.span`.
///
/// # Suppression rules
///
/// - If the head alias resolved to `ImportTarget::Unresolved` → silently return
///   `Binding::Error` with no diagnostic (R011).
/// - If the head alias resolved to `ImportTarget::External` → same.
#[must_use]
pub fn resolve_qualified_name(
    qn: &QualifiedName,
    _module_id: ModuleId,
    my_table: Option<&SymbolTable>,
    all_symbol_tables: &[SymbolTable],
    module_imports: &[ImportResolution],
    errors: &mut Vec<ResolveError>,
) -> Binding {
    if qn.segments.is_empty() {
        return Binding::Error;
    }

    let head = &qn.segments[0];
    let head_text = &head.text;
    let all_segs: Vec<String> = qn.segments.iter().map(|s| s.text.clone()).collect();

    // ── Step 1: Head-segment lookup in import effective bindings ─────────────

    // First search import bindings.
    let head_import_binding = find_import_binding(head_text, module_imports).cloned();

    if let Some(eb) = head_import_binding {
        return match &eb.binding {
            Binding::ModuleAlias { target, .. } => {
                let target = target.clone();
                // ── Step 2: ModuleAlias descent ──────────────────────────────
                resolve_in_target(&target, qn, &all_segs, all_symbol_tables, errors)
            }
            Binding::ImportedSymbol { module, symbol, .. } => {
                // The head bound to a symbol (e.g. prelude StdlibSymbol is more
                // common, but an explicit `import X (MyType)` can also land here).
                // Descend into it as a Type/Actor head (step 3).
                let module = *module;
                let symbol = *symbol;
                resolve_type_actor_head(module, symbol, qn, &all_segs, all_symbol_tables, errors)
            }
            Binding::StdlibSymbol { module, .. } => {
                // A StdlibSymbol at the head position can arise when the prelude
                // injected e.g. `Option` as a StdlibSymbol from std.option.
                // In 0.1.0, constructor resolution for Option/Result goes via
                // prelude's StdlibSymbol — we resolve last-seg as a stdlib export
                // from the same module.
                let module = *module;
                let last_seg = &qn.segments[qn.segments.len() - 1];
                let last_text = &last_seg.text;
                if let Some(m) = BUILTINS.get(module.0 as usize) {
                    if m.exports.contains(&last_text.as_str()) {
                        return Binding::StdlibSymbol {
                            module,
                            name: last_text.clone(),
                        };
                    }
                    // Miss — R014 with stdlib-export suggestions.
                    let suggestions =
                        suggest::suggest(last_text, m.exports.iter().map(|s| (*s).to_owned()));
                    errors.push(ResolveError::UnknownStdlibSymbol {
                        module: m.name.to_owned(),
                        name: last_text.clone(),
                        suggestions,
                        span: last_seg.span,
                    });
                }
                Binding::Error
            }
            // Any other kind (Local, Constructor, FieldAccessor, Error) at head
            // position cannot be qualified — emit R012 with head-replacement suggestions.
            _ => {
                let suggestions =
                    head_replacement_suggestions(head_text, qn, my_table, module_imports);
                errors.push(ResolveError::UnresolvedQualifiedName {
                    segments: all_segs,
                    suggestions,
                    span: qn.span,
                });
                Binding::Error
            }
        };
    }

    // ── Not in imports: search my_table for a local Type/Actor ───────────────

    if let Some(table) = my_table {
        if let Some(sym) = table.lookup(head_text) {
            let sym_id = sym.id;
            let sym_module = table.module;
            match &sym.kind {
                SymbolKind::Type { .. } | SymbolKind::Actor { .. } => {
                    return resolve_type_actor_head(
                        sym_module,
                        sym_id,
                        qn,
                        &all_segs,
                        all_symbol_tables,
                        errors,
                    );
                }
                _ => {
                    // Non-type/actor in head position — R012 with head-replacement suggestions.
                    let suggestions =
                        head_replacement_suggestions(head_text, qn, my_table, module_imports);
                    errors.push(ResolveError::UnresolvedQualifiedName {
                        segments: all_segs,
                        suggestions,
                        span: qn.span,
                    });
                    return Binding::Error;
                }
            }
        }
    }

    // ── Head not found anywhere — R012 ────────────────────────────────────────

    let suggestions = head_replacement_suggestions(head_text, qn, my_table, module_imports);
    errors.push(ResolveError::UnresolvedQualifiedName {
        segments: all_segs,
        suggestions,
        span: qn.span,
    });
    Binding::Error
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Find the first effective binding with the given local name across all imports.
fn find_import_binding<'a>(
    name: &str,
    module_imports: &'a [ImportResolution],
) -> Option<&'a EffectiveBinding> {
    module_imports
        .iter()
        .flat_map(|ir| &ir.effective_bindings)
        .find(|eb| eb.local_name == name)
}

/// Resolve the last segment in a module-alias target (step 2).
///
/// `External` and `Unresolved` targets are silent (`Binding::Error` no
/// diagnostic) per R011 suppression.
fn resolve_in_target(
    target: &ImportTarget,
    qn: &QualifiedName,
    all_segs: &[String],
    all_symbol_tables: &[SymbolTable],
    errors: &mut Vec<ResolveError>,
) -> Binding {
    let last_seg = &qn.segments[qn.segments.len() - 1];
    let last_text = &last_seg.text;

    match target {
        ImportTarget::BuiltinStdlib(sid) => {
            let idx = sid.0 as usize;
            if let Some(m) = BUILTINS.get(idx) {
                if m.exports.contains(&last_text.as_str()) {
                    return Binding::StdlibSymbol {
                        module: *sid,
                        name: last_text.clone(),
                    };
                }
                // Unknown stdlib symbol — R014 with suggestions.
                let suggestions =
                    suggest::suggest(last_text, m.exports.iter().map(|s| (*s).to_owned()));
                errors.push(ResolveError::UnknownStdlibSymbol {
                    module: m.name.to_owned(),
                    name: last_text.clone(),
                    suggestions,
                    span: last_seg.span,
                });
            }
            Binding::Error
        }
        ImportTarget::WorkspaceModule(mid) => {
            if let Some(table) = all_symbol_tables.get(mid.0 as usize) {
                if let Some(sym) = table.lookup(last_text) {
                    return Binding::ImportedSymbol {
                        module: *mid,
                        symbol: sym.id,
                        via_import: NodeId(0),
                    };
                }
                // Symbol not found in workspace module — R012 with member-
                // replacement suggestions.  Note: we don't apply
                // visibility filtering here because reaching this branch
                // means R007 / R009 didn't already gate the import; the
                // ModuleAlias is fully visible and members of the alias's
                // target module are visible at the qualified-use site.
                let suggestions = qualified_member_suggestions(table, qn, last_text);
                errors.push(ResolveError::UnresolvedQualifiedName {
                    segments: all_segs.to_vec(),
                    suggestions,
                    span: qn.span,
                });
                return Binding::Error;
            }
            // Defensive: target module index out of bounds.
            errors.push(ResolveError::UnresolvedQualifiedName {
                segments: all_segs.to_vec(),
                suggestions: Vec::new(),
                span: qn.span,
            });
            Binding::Error
        }
        // External / Unresolved: R006 already fired — suppress cascade (R011).
        ImportTarget::External { .. } | ImportTarget::Unresolved => Binding::Error,
    }
}

/// Descend through a Type or Actor symbol to resolve the last segment.
///
/// For a `Type` head: search its constructors.
/// For an `Actor` head: always R012 (no handler resolution at qualified sites
/// per §4.5 R012).
fn resolve_type_actor_head(
    owner_module: ModuleId,
    head_sym_id: SymbolId,
    qn: &QualifiedName,
    all_segs: &[String],
    all_symbol_tables: &[SymbolTable],
    errors: &mut Vec<ResolveError>,
) -> Binding {
    let last_text = &qn.segments[qn.segments.len() - 1].text;

    let Some(table) = all_symbol_tables.get(owner_module.0 as usize) else {
        errors.push(ResolveError::UnresolvedQualifiedName {
            segments: all_segs.to_vec(),
            suggestions: Vec::new(),
            span: qn.span,
        });
        return Binding::Error;
    };

    // Get the head entry to determine if it's a Type or Actor.
    let head_entry = table.entries.get(head_sym_id.0 as usize);
    match head_entry.map(|e| &e.kind) {
        Some(SymbolKind::Type { .. }) => {
            // Search all entries for a Constructor whose owner_type == head_sym_id
            // and whose name matches last_text.
            for entry in &table.entries {
                if let SymbolKind::Constructor {
                    owner_type,
                    variant,
                    is_record,
                    owner_module,
                    ..
                } = &entry.kind
                {
                    if *owner_type == head_sym_id && entry.name == *last_text {
                        return Binding::Constructor {
                            owner_type: head_sym_id,
                            variant: *variant,
                            is_record: *is_record,
                            owner_module: *owner_module,
                        };
                    }
                }
            }
            // Constructor not found — R012 with constructor-name suggestions
            // restricted to this Type's variants.
            let candidates = table.entries.iter().filter_map(|e| match &e.kind {
                SymbolKind::Constructor { owner_type, .. } if *owner_type == head_sym_id => {
                    Some(e.name.clone())
                }
                _ => None,
            });
            let suggestions = qualified_member_suggestions_from_iter(qn, last_text, candidates);
            errors.push(ResolveError::UnresolvedQualifiedName {
                segments: all_segs.to_vec(),
                suggestions,
                span: qn.span,
            });
            Binding::Error
        }
        Some(SymbolKind::Actor { .. }) => {
            // Per §4.5 R012: actor heads at qualified-name sites → R012.
            // No qualified-handler resolution in 0.1.0; suggestions empty.
            errors.push(ResolveError::UnresolvedQualifiedName {
                segments: all_segs.to_vec(),
                suggestions: Vec::new(),
                span: qn.span,
            });
            Binding::Error
        }
        _ => {
            errors.push(ResolveError::UnresolvedQualifiedName {
                segments: all_segs.to_vec(),
                suggestions: Vec::new(),
                span: qn.span,
            });
            Binding::Error
        }
    }
}

// ── Suggestion helpers ────────────────────────────────────────────────────────

/// Render a head-replacement suggestion: replace `qn.segments[0]` with
/// `new_head` and rebuild the rest verbatim.
///
/// `Li.map` with `new_head = "List"` → `"List.map"`.
fn replace_head(qn: &QualifiedName, new_head: &str) -> String {
    if qn.segments.len() == 1 {
        return new_head.to_owned();
    }
    let mut out = String::new();
    out.push_str(new_head);
    for seg in qn.segments.iter().skip(1) {
        out.push('.');
        out.push_str(&seg.text);
    }
    out
}

/// Render a member-replacement suggestion: replace `qn.segments[last]` with
/// `new_member` keeping the head-and-middle segments intact.
///
/// `head.middle.lass` with `new_member = "last"` → `"head.middle.last"`.
fn replace_member(qn: &QualifiedName, new_member: &str) -> String {
    if qn.segments.len() == 1 {
        return new_member.to_owned();
    }
    let mut out = String::new();
    let last_idx = qn.segments.len() - 1;
    for (i, seg) in qn.segments.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        if i == last_idx {
            out.push_str(new_member);
        } else {
            out.push_str(&seg.text);
        }
    }
    out
}

/// Build R012 suggestions for a head miss (cases 1–3).
///
/// Candidates: every `local_name` from the module's import effective bindings,
/// plus every `Type` / `Actor` symbol name from `my_table`.  These are exactly
/// the names that could legally appear at a qualified-name head position.
///
/// Visibility: import effective bindings are pre-filtered by import resolution (private items
/// never make it into [`EffectiveBinding`]); `my_table` symbols are local to
/// the module, all in scope.  Plan §11 risk R14 is satisfied at the call site.
///
/// The exact head text is excluded from candidates so we don't suggest the
/// user's own (wrong-kind) name back at them.
fn head_replacement_suggestions(
    head_text: &str,
    qn: &QualifiedName,
    my_table: Option<&SymbolTable>,
    module_imports: &[ImportResolution],
) -> Vec<String> {
    let mut candidates: Vec<String> = module_imports
        .iter()
        .flat_map(|ir| ir.effective_bindings.iter().map(|eb| eb.local_name.clone()))
        .filter(|n| n != head_text)
        .collect();

    if let Some(table) = my_table {
        for entry in &table.entries {
            if matches!(
                entry.kind,
                SymbolKind::Type { .. } | SymbolKind::Actor { .. }
            ) && entry.name != head_text
            {
                candidates.push(entry.name.clone());
            }
        }
    }

    suggest::suggest(head_text, candidates)
        .into_iter()
        .map(|s| replace_head(qn, &s))
        .collect()
}

/// Build R012 member-replacement suggestions when a qualified-name's last
/// segment failed to resolve in the workspace module pointed to by its head.
fn qualified_member_suggestions(
    table: &SymbolTable,
    qn: &QualifiedName,
    last_text: &str,
) -> Vec<String> {
    let candidates = table.entries.iter().map(|e| e.name.clone());
    qualified_member_suggestions_from_iter(qn, last_text, candidates)
}

/// Underlying member-suggestion helper that takes a candidate iterator.
fn qualified_member_suggestions_from_iter<I>(
    qn: &QualifiedName,
    last_text: &str,
    candidates: I,
) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    suggest::suggest(last_text, candidates)
        .into_iter()
        .map(|s| replace_member(qn, &s))
        .collect()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        imports::{Binding, EffectiveBinding, ImportResolution, ImportTarget},
        scope::LocalId,
        stdlib_builtin::StdlibModuleId,
        symbol::{SymbolEntry, SymbolKind, SymbolTable},
        visibility::ResolvedVisibility,
        ModuleId, NodeId, SymbolId,
    };
    use ridge_ast::{expr::QualifiedName, Ident, Span};
    use rustc_hash::FxHashMap;

    // ── AST/fixture helpers ───────────────────────────────────────────────────

    fn sp() -> Span {
        Span::point(0)
    }

    fn ident(text: &str) -> Ident {
        Ident {
            text: text.into(),
            span: sp(),
        }
    }

    fn qn(segs: &[&str]) -> QualifiedName {
        QualifiedName {
            segments: segs.iter().map(|s| ident(s)).collect(),
            span: sp(),
        }
    }

    /// Build a minimal `ImportResolution` that introduces one `ModuleAlias`.
    fn alias_ir(local_name: &str, target: ImportTarget) -> ImportResolution {
        ImportResolution {
            decl_node: NodeId(0),
            target: target.clone(),
            alias: Some(local_name.to_owned()),
            explicit_items: None,
            effective_bindings: vec![EffectiveBinding {
                local_name: local_name.to_owned(),
                binding: Binding::ModuleAlias {
                    target,
                    via_import: NodeId(0),
                },
            }],
            span: sp(),
        }
    }

    /// Build a minimal `ImportResolution` that introduces one `StdlibSymbol`
    /// binding (used to simulate prelude entries).
    fn stdlib_symbol_ir(local_name: &str, module_id: u32) -> ImportResolution {
        let sid = StdlibModuleId(module_id);
        ImportResolution {
            decl_node: NodeId(0),
            target: ImportTarget::BuiltinStdlib(sid),
            alias: None,
            explicit_items: None,
            effective_bindings: vec![EffectiveBinding {
                local_name: local_name.to_owned(),
                binding: Binding::StdlibSymbol {
                    module: sid,
                    name: local_name.to_owned(),
                },
            }],
            span: sp(),
        }
    }

    /// Build an empty `SymbolTable` for a given module.
    fn empty_table(module_id: u32) -> SymbolTable {
        SymbolTable {
            module: ModuleId(module_id),
            entries: Vec::new(),
            index: FxHashMap::default(),
        }
    }

    /// Build a `SymbolTable` containing a union type with named constructors.
    fn union_table(module_id: u32, type_name: &str, ctors: &[&str]) -> SymbolTable {
        let mut table = SymbolTable {
            module: ModuleId(module_id),
            entries: Vec::new(),
            index: FxHashMap::default(),
        };

        let type_id = SymbolId(u32::try_from(table.entries.len()).unwrap_or(u32::MAX));
        table.entries.push(SymbolEntry {
            id: type_id,
            name: type_name.to_owned(),
            kind: SymbolKind::Type {
                arity: 0,
                opaque: false,
            },
            visibility: ResolvedVisibility::Pub,
            def_span: sp(),
            exported_externally: false,
        });
        table.index.insert(type_name.to_owned(), type_id);

        let owner_module = table.module;
        for (i, ctor_name) in ctors.iter().enumerate() {
            let ctor_id = SymbolId(u32::try_from(table.entries.len()).unwrap_or(u32::MAX));
            table.entries.push(SymbolEntry {
                id: ctor_id,
                name: (*ctor_name).to_owned(),
                kind: SymbolKind::Constructor {
                    owner_type: type_id,
                    variant: u32::try_from(i).unwrap_or(u32::MAX),
                    arity: 0,
                    is_record: false,
                    owner_module,
                    opaque: false,
                },
                visibility: ResolvedVisibility::Pub,
                def_span: sp(),
                exported_externally: false,
            });
            table.index.insert((*ctor_name).to_owned(), ctor_id);
        }

        table
    }

    /// Build a `SymbolTable` containing one public function.
    fn fn_table(module_id: u32, fn_name: &str) -> SymbolTable {
        let mut table = SymbolTable {
            module: ModuleId(module_id),
            entries: Vec::new(),
            index: FxHashMap::default(),
        };
        let id = SymbolId(0);
        table.entries.push(SymbolEntry {
            id,
            name: fn_name.to_owned(),
            kind: SymbolKind::Fn { caps: Vec::new() },
            visibility: ResolvedVisibility::Pub,
            def_span: sp(),
            exported_externally: false,
        });
        table.index.insert(fn_name.to_owned(), id);
        table
    }

    // ── Helper: resolve and collect errors ────────────────────────────────────

    fn resolve(
        qn_val: &QualifiedName,
        my_table: Option<&SymbolTable>,
        all_tables: &[SymbolTable],
        imports: &[ImportResolution],
    ) -> (Binding, Vec<ResolveError>) {
        let mut errors = Vec::new();
        let binding = resolve_qualified_name(
            qn_val,
            ModuleId(0),
            my_table,
            all_tables,
            imports,
            &mut errors,
        );
        (binding, errors)
    }

    // ── Test 1: Io.println → StdlibSymbol { module: std.io (id=9), name: "println" } ──

    #[test]
    fn t1_io_println_resolves_to_stdlib_symbol() {
        // std.io is at BUILTINS index 9.
        let io_ir = alias_ir("Io", ImportTarget::BuiltinStdlib(StdlibModuleId(9)));
        let (binding, errors) = resolve(&qn(&["Io", "println"]), None, &[], &[io_ir]);
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        assert!(
            matches!(
                binding,
                Binding::StdlibSymbol {
                    module: StdlibModuleId(9),
                    ref name,
                } if name == "println"
            ),
            "expected StdlibSymbol(std.io, \"println\"), got {binding:?}"
        );
    }

    // ── Test 2: List.map → StdlibSymbol { module: std.list (id=4), name: "map" } ──

    #[test]
    fn t2_list_map_resolves_to_stdlib_symbol() {
        let list_ir = alias_ir("List", ImportTarget::BuiltinStdlib(StdlibModuleId(4)));
        let (binding, errors) = resolve(&qn(&["List", "map"]), None, &[], &[list_ir]);
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        assert!(
            matches!(
                binding,
                Binding::StdlibSymbol {
                    module: StdlibModuleId(4),
                    ref name,
                } if name == "map"
            ),
            "expected StdlibSymbol(std.list, \"map\"), got {binding:?}"
        );
    }

    // ── Test 3: Map.empty → StdlibSymbol { module: std.map (id=5), name: "empty" } ──

    #[test]
    fn t3_map_empty_resolves_to_stdlib_symbol() {
        let map_ir = alias_ir("Map", ImportTarget::BuiltinStdlib(StdlibModuleId(5)));
        let (binding, errors) = resolve(&qn(&["Map", "empty"]), None, &[], &[map_ir]);
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        assert!(
            matches!(
                binding,
                Binding::StdlibSymbol {
                    module: StdlibModuleId(5),
                    ref name,
                } if name == "empty"
            ),
            "expected StdlibSymbol(std.map, \"empty\"), got {binding:?}"
        );
    }

    // ── Test 4: Random.choice → StdlibSymbol { module: std.random (id=12), name: "choice" } ──

    #[test]
    fn t4_random_choice_resolves_to_stdlib_symbol() {
        let rand_ir = alias_ir("Random", ImportTarget::BuiltinStdlib(StdlibModuleId(12)));
        let (binding, errors) = resolve(&qn(&["Random", "choice"]), None, &[], &[rand_ir]);
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        assert!(
            matches!(
                binding,
                Binding::StdlibSymbol {
                    module: StdlibModuleId(12),
                    ref name,
                } if name == "choice"
            ),
            "expected StdlibSymbol(std.random, \"choice\"), got {binding:?}"
        );
    }

    // ── Test 5: Bogus.thing (no Bogus in scope) → R012, Binding::Error ──────────

    #[test]
    fn t5_bogus_thing_emits_r012() {
        let (binding, errors) = resolve(&qn(&["Bogus", "thing"]), None, &[], &[]);
        assert!(
            matches!(binding, Binding::Error),
            "expected Binding::Error, got {binding:?}"
        );
        assert_eq!(errors.len(), 1, "expected 1 error; got {errors:?}");
        assert_eq!(errors[0].code(), "R012");
    }

    // ── Test 6: List.mapp (typo) → R014 with suggestions including "map" ────────

    #[test]
    fn t6_list_mapp_emits_r014_with_suggestion() {
        let list_ir = alias_ir("List", ImportTarget::BuiltinStdlib(StdlibModuleId(4)));
        let (binding, errors) = resolve(&qn(&["List", "mapp"]), None, &[], &[list_ir]);
        assert!(
            matches!(binding, Binding::Error),
            "expected Binding::Error, got {binding:?}"
        );
        assert_eq!(errors.len(), 1, "expected 1 R014 error; got {errors:?}");
        assert_eq!(errors[0].code(), "R014");
        // Suggestions must include "map".
        if let ResolveError::UnknownStdlibSymbol { suggestions, .. } = &errors[0] {
            assert!(
                suggestions.iter().any(|s| s == "map"),
                "suggestions {suggestions:?} must include \"map\""
            );
        } else {
            panic!("expected UnknownStdlibSymbol, got {:?}", errors[0]);
        }
    }

    // ── Test 7: Result.Ok → Constructor { owner_type, variant: 0 } ─────────────
    //
    // Build a local SymbolTable with Result as a Type and Ok, Err as Constructors.
    // Pass the same table as both my_table and all_symbol_tables[0].

    #[test]
    fn t7_result_ok_resolves_to_constructor() {
        let table = union_table(0, "Result", &["Ok", "Err"]);
        let table2 = union_table(0, "Result", &["Ok", "Err"]);
        let all = vec![table2];
        let (binding, errors) = resolve(&qn(&["Result", "Ok"]), Some(&table), &all, &[]);
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        assert!(
            matches!(binding, Binding::Constructor { variant: 0, .. }),
            "expected Constructor {{ variant: 0, .. }}, got {binding:?}"
        );
    }

    // ── Test 8: Option.Some → Constructor { owner_type, variant: 0 } ───────────

    #[test]
    fn t8_option_some_resolves_to_constructor() {
        let table = union_table(0, "Option", &["Some", "None"]);
        let table2 = union_table(0, "Option", &["Some", "None"]);
        let all = vec![table2];
        let (binding, errors) = resolve(&qn(&["Option", "Some"]), Some(&table), &all, &[]);
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        assert!(
            matches!(binding, Binding::Constructor { variant: 0, .. }),
            "expected Constructor {{ variant: 0, .. }}, got {binding:?}"
        );
    }

    // ── Test 9: Workspace module qualified path → ImportedSymbol ─────────────────

    #[test]
    fn t9_workspace_module_qualified_resolves_to_imported_symbol() {
        // Module 1 has a public `publicFn` function.
        let other_table = fn_table(1, "publicFn");
        let tables = vec![empty_table(0), other_table];
        let other_ir = alias_ir("Other", ImportTarget::WorkspaceModule(ModuleId(1)));
        let (binding, errors) = resolve(
            &qn(&["Other", "publicFn"]),
            Some(&tables[0]),
            &tables,
            &[other_ir],
        );
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        assert!(
            matches!(
                binding,
                Binding::ImportedSymbol {
                    module: ModuleId(1),
                    ..
                }
            ),
            "expected ImportedSymbol(ModuleId(1), ...), got {binding:?}"
        );
    }

    // ── Test 10: Head resolves to Unresolved target → Binding::Error, no R012 ───

    #[test]
    fn t10_unresolved_target_suppresses_r012() {
        let unresolved_ir = alias_ir("Missing", ImportTarget::Unresolved);
        let (binding, errors) = resolve(&qn(&["Missing", "thing"]), None, &[], &[unresolved_ir]);
        assert!(
            matches!(binding, Binding::Error),
            "expected Binding::Error, got {binding:?}"
        );
        // R011 suppression: no diagnostic must be emitted.
        assert!(
            errors.is_empty(),
            "expected no errors for Unresolved target; got {errors:?}"
        );
    }

    // ── Test 11: Workspace module — symbol not found → R012 ─────────────────────

    #[test]
    fn t11_workspace_module_missing_symbol_emits_r012() {
        let other_table = fn_table(1, "existingFn");
        let tables = vec![empty_table(0), other_table];
        let other_ir = alias_ir("Other", ImportTarget::WorkspaceModule(ModuleId(1)));
        let (binding, errors) = resolve(
            &qn(&["Other", "missingFn"]),
            Some(&tables[0]),
            &tables,
            &[other_ir],
        );
        assert!(matches!(binding, Binding::Error), "got {binding:?}");
        assert_eq!(errors.len(), 1, "expected 1 R012; got {errors:?}");
        assert_eq!(errors[0].code(), "R012");
    }

    // ── Test 12: Option.None → Constructor { variant: 1 } ───────────────────────

    #[test]
    fn t12_option_none_resolves_to_constructor_variant_1() {
        let table = union_table(0, "Option", &["Some", "None"]);
        let table2 = union_table(0, "Option", &["Some", "None"]);
        let all = vec![table2];
        let (binding, errors) = resolve(&qn(&["Option", "None"]), Some(&table), &all, &[]);
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        assert!(
            matches!(binding, Binding::Constructor { variant: 1, .. }),
            "expected Constructor {{ variant: 1, .. }}, got {binding:?}"
        );
    }

    // Low-level Damerau-Levenshtein and "did-you-mean" engine
    // tests live in `crate::suggest::tests`.

    // ── Test 15: StdlibSymbol head (prelude Option) → second stdlib export ───────
    //
    // When prelude injects `Option` as a `StdlibSymbol` (not a ModuleAlias),
    // `Option.Some` should still resolve via the StdlibSymbol branch.

    #[test]
    fn t15_stdlib_symbol_head_resolves_last_seg() {
        // Simulate: prelude injected "Option" as StdlibSymbol { module: 7, name: "Option" }
        let opt_ir = stdlib_symbol_ir("Option", 7); // std.option
        let (binding, errors) = resolve(&qn(&["Option", "Some"]), None, &[], &[opt_ir]);
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        // std.option (id=7) exports "Some"
        assert!(
            matches!(
                binding,
                Binding::StdlibSymbol {
                    module: StdlibModuleId(7),
                    ref name,
                } if name == "Some"
            ),
            "expected StdlibSymbol(std.option, \"Some\"), got {binding:?}"
        );
    }

    // ── Test 16: Local binding at head position → R012 ───────────────────────────

    #[test]
    fn t16_local_at_head_position_emits_r012() {
        // Create an IR that maps "Local" to a Local binding (not a module alias).
        let local_ir = ImportResolution {
            decl_node: NodeId(0),
            target: ImportTarget::Unresolved,
            alias: None,
            explicit_items: None,
            effective_bindings: vec![EffectiveBinding {
                local_name: "Local".to_owned(),
                binding: Binding::Local(LocalId(0)),
            }],
            span: sp(),
        };
        let (binding, errors) = resolve(&qn(&["Local", "field"]), None, &[], &[local_ir]);
        assert!(matches!(binding, Binding::Error), "got {binding:?}");
        assert_eq!(errors.len(), 1, "expected 1 R012; got {errors:?}");
        assert_eq!(errors[0].code(), "R012");
    }

    // ── R012 head-replacement suggestion ─────────────────────────────────────
    //
    // "Li.map" with `List` in scope must suggest `List.map` (head-replacement
    // composing the rest of the path).  The suggestion must NOT include
    // `List.empty` (unrelated stdlib member).
    #[test]
    fn t13_li_dot_map_suggests_list_dot_map() {
        let list_ir = alias_ir("List", ImportTarget::BuiltinStdlib(StdlibModuleId(4)));
        let (_, errors) = resolve(&qn(&["Li", "map"]), None, &[], &[list_ir]);
        assert_eq!(errors.len(), 1, "expected 1 R012; got {errors:?}");
        let suggestions = match &errors[0] {
            ResolveError::UnresolvedQualifiedName { suggestions, .. } => suggestions.clone(),
            other => panic!("expected R012 UnresolvedQualifiedName; got {other:?}"),
        };
        assert!(
            suggestions.contains(&"List.map".to_owned()),
            "must suggest `List.map`; got {suggestions:?}"
        );
        assert!(
            !suggestions.contains(&"List.empty".to_owned()),
            "must NOT suggest unrelated `List.empty`; got {suggestions:?}"
        );
    }

    // ── resolve_qualified_record_constructor happy path ───────────────────────
    //
    // Http.Response where Http is a local record type in the same module.
    //
    // Expected: Binding::Constructor { owner_type: TypeId(0), variant: 0 }
    #[test]
    fn t_qualified_record_ctor_happy_path() {
        // Module 0 has: type Http with constructor Response (variant 0).
        // head found in my_table as Type, then resolve_type_actor_head finds the constructor.
        let table = union_table(0, "Http", &["Response"]);
        let table2 = union_table(0, "Http", &["Response"]);
        let all = vec![table2];
        let mut errors = Vec::new();
        let binding = resolve_qualified_record_constructor(
            &qn(&["Http", "Response"]),
            ModuleId(0),
            Some(&table),
            &all,
            &[],
            &mut errors,
        );
        assert!(errors.is_empty(), "expected no errors; got {errors:?}");
        assert!(
            matches!(binding, Binding::Constructor { variant: 0, .. }),
            "expected Constructor {{ variant: 0, .. }}, got {binding:?}"
        );
    }

    // ── R012 member-replacement when the head DOES resolve ───────────────────
    //
    // `Result.Okk` (typo of `Ok`) — head resolves to a Type, last segment
    // misses; suggestion should be `Result.Ok`.
    #[test]
    fn t13_result_dot_okk_suggests_result_dot_ok() {
        let table = union_table(0, "Result", &["Ok", "Err"]);
        let table2 = union_table(0, "Result", &["Ok", "Err"]);
        let all = vec![table2];
        let (_, errors) = resolve(&qn(&["Result", "Okk"]), Some(&table), &all, &[]);
        assert_eq!(errors.len(), 1, "expected 1 R012; got {errors:?}");
        let suggestions = match &errors[0] {
            ResolveError::UnresolvedQualifiedName { suggestions, .. } => suggestions.clone(),
            other => panic!("expected R012; got {other:?}"),
        };
        assert!(
            suggestions.contains(&"Result.Ok".to_owned()),
            "must suggest `Result.Ok`; got {suggestions:?}"
        );
    }
}
