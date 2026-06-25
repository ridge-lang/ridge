//! Go-to-definition support for the embedded Ridge standard library.
//!
//! The stdlib ships as source text baked into the compiler (see
//! [`ridge_stdlib::STDLIB_SOURCES`]); it has no [`ModuleId`] in the workspace
//! index, so the cross-file machinery in [`crate::index`] cannot reach it. To
//! let an editor jump into a stdlib definition we materialise those embedded
//! sources to a stable on-disk cache once per process and point the resulting
//! `file://` location at them.
//!
//! [`ModuleId`]: ridge_resolve::ModuleId
//!
//! # Cache layout
//!
//! Sources are written under
//! `<temp-dir>/ridge-lsp/stdlib/<compiler-version>/<relative-path>`. Namespacing
//! by compiler version keeps a stale layout from a previous build from shadowing
//! the current one, and the path is derived entirely from the fixed builtin
//! table — no user-controlled component, so there is no path-traversal surface.
//! Only the embedded (non-secret) stdlib text is written. The cache path is
//! stable across calls so an editor can re-navigate to the same location.
//!
//! # Caching
//!
//! Materialisation runs at most once per process, guarded by a [`OnceLock`].
//! Each module is parsed at most once: its declaration spans are computed lazily
//! the first time it is navigated into and stored in a process-global map.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use ridge_ast::{Item, Span};
use ridge_lexer::LineIndex;
use ridge_resolve::{StdlibModuleId, BUILTINS};
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::index::{build_signature, const_header, fn_header, type_header, SignatureSig};

/// A hover card for a stdlib top-level symbol: its written header, the role to
/// label it with, and the documentation lifted from the `--` comment block
/// above the declaration.
#[derive(Debug, Clone)]
pub struct StdlibCard {
    /// The written declaration head, e.g. `pub fn map (f: fn a -> b) (xs: List a) -> List b`.
    pub header: String,
    /// `function` / `constant` / `type` / `actor`, for the kind line.
    pub kind: &'static str,
    /// The doc block above the declaration, or `None` when undocumented.
    pub doc: Option<String>,
}

/// The materialised, parsed form of one stdlib module.
struct StdlibModuleDef {
    /// `file://` URL of the materialised source on disk.
    uri: Url,
    /// UTF-16 ↔ byte line index, built from the in-memory source so spans and
    /// the on-disk file agree exactly.
    line_index: LineIndex,
    /// Top-level declaration name → its definition span.
    defs: HashMap<String, Span>,
    /// Top-level function name → its rendered signature, for signature help.
    fn_sigs: HashMap<String, SignatureSig>,
    /// Top-level declaration name → its rendered hover card (header + doc), for hover.
    cards: HashMap<String, StdlibCard>,
}

/// A stdlib class method located by `(class, method)`: its owning module, the
/// method-name span (for go-to-definition), the rendered signature, and the doc
/// lifted from the `--` block above the method — or, when the method has none of
/// its own, the doc of the class it belongs to.
struct ClassMethodEntry {
    /// Owning builtin module id (indexes [`BUILTINS`] and the per-module cache).
    module: u32,
    /// The method name's span inside the `class` declaration.
    name_span: Span,
    /// The method signature rendered for signature help.
    sig: SignatureSig,
    /// The method's documentation, falling back to the owning class's doc.
    doc: Option<String>,
}

/// How a single source line reads for the purpose of lifting a doc block.
enum LineClass {
    /// A `--` line comment; carries the text after the dashes, trimmed.
    Comment(String),
    /// An attribute line (`@ffi(...)`, `@test`, ...) sitting between a doc block
    /// and the declaration it documents — skipped, not a block boundary.
    Attr,
    /// Blank or code — ends a doc block when walking upward.
    Boundary,
}

/// Classify every line of `source` for doc-block extraction (index = 0-based line).
fn classify_lines(source: &str) -> Vec<LineClass> {
    source
        .lines()
        .map(|raw| {
            let trimmed = raw.trim_start();
            match trimmed.strip_prefix("--") {
                Some(rest) => LineClass::Comment(rest.trim().to_owned()),
                None if trimmed.starts_with('@') => LineClass::Attr,
                None => LineClass::Boundary,
            }
        })
        .collect()
}

/// The contiguous `--` comment block immediately above `line` (0-based).
///
/// Joined in source order. Attribute lines between the block and the declaration
/// are skipped; a blank or code line ends the block. `None` when there is no
/// comment.
fn doc_above(line: u32, classes: &[LineClass]) -> Option<String> {
    let mut collected: Vec<&str> = Vec::new();
    let mut idx = line as usize;
    while idx > 0 {
        idx -= 1;
        match classes.get(idx) {
            Some(LineClass::Comment(text)) => collected.push(text),
            Some(LineClass::Attr) => {}
            _ => break,
        }
    }
    if collected.is_empty() {
        return None;
    }
    collected.reverse();
    Some(collected.join("\n"))
}

/// The 0-based line a span starts on, via `line_index`.
fn line_of(line_index: &LineIndex, span: Span) -> u32 {
    line_index.byte_to_utf16(span.start).0
}

/// Process-global cache of parsed stdlib modules, keyed by [`StdlibModuleId.0`].
fn module_cache() -> &'static Mutex<HashMap<u32, StdlibModuleDef>> {
    static CACHE: OnceLock<Mutex<HashMap<u32, StdlibModuleDef>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Root directory the stdlib sources are materialised into.
fn cache_root() -> PathBuf {
    std::env::temp_dir()
        .join("ridge-lsp")
        .join("stdlib")
        .join(env!("CARGO_PKG_VERSION"))
}

/// Ensure the embedded stdlib sources have been written to the cache directory.
///
/// Runs at most once per process. Best-effort: if writing fails the goto simply
/// resolves to a path that may not exist, which an editor tolerates, so the
/// error is swallowed here rather than propagated.
fn ensure_materialised() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let root = cache_root();
        // Tolerate the directory (and files within) already existing from an
        // earlier run; `write_stdlib_sources_to` overwrites file contents.
        let _ = ridge_stdlib::write_stdlib_sources_to(&root);
        mark_sources_read_only(&root);
    });
}

/// Best-effort: mark every materialised source read-only so a user does not
/// accidentally edit the stdlib copy. Failures are ignored — a writable stdlib
/// copy is harmless, and goto must not depend on the permission change.
fn mark_sources_read_only(root: &std::path::Path) {
    for (rel, _) in ridge_stdlib::STDLIB_SOURCES {
        let path = root.join(rel);
        if let Ok(meta) = std::fs::metadata(&path) {
            let mut perms = meta.permissions();
            if !perms.readonly() {
                perms.set_readonly(true);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }
    }
}

/// The embedded-source relative path for a stdlib module name.
///
/// `std.list` → `list.ridge`, `std.net.http` → `net/http.ridge`. Matches the
/// keys of [`ridge_stdlib::STDLIB_SOURCES`].
fn relative_source_path(module_name: &str) -> Option<String> {
    let tail = module_name.strip_prefix("std.")?;
    Some(format!("{}.ridge", tail.replace('.', "/")))
}

/// The embedded source text for a relative path, if present.
fn source_for(rel: &str) -> Option<&'static str> {
    ridge_stdlib::STDLIB_SOURCES
        .iter()
        .find_map(|(key, text)| (*key == rel).then_some(*text))
}

/// Collect top-level declaration `name → span` pairs from a parsed module.
///
/// Covers the named top-level value and type declarations: `fn`, `const`,
/// `type` (including `opaque type`), and `actor`. The span used is the
/// declaration name's identifier span, which puts the cursor on the name when
/// the editor jumps. Class and instance bodies are out of scope here.
fn collect_defs(items: &[Item]) -> HashMap<String, Span> {
    let mut defs = HashMap::new();
    for item in items {
        let entry = match item {
            Item::Fn(decl) => Some((decl.name.text.clone(), decl.name.span)),
            Item::Const(decl) => Some((decl.name.text.clone(), decl.name.span)),
            Item::Type(decl) => Some((decl.name.text.clone(), decl.name.span)),
            Item::Actor(decl) => Some((decl.name.text.clone(), decl.name.span)),
            Item::Import(_) | Item::ClassDecl(_) | Item::InstanceDecl(_) => None,
        };
        if let Some((name, span)) = entry {
            // Keep the first declaration of a given name; redeclarations are a
            // resolver error and not this layer's concern.
            defs.entry(name).or_insert(span);
        }
    }
    defs
}

/// Collect `name → hover card` pairs for a module's top-level declarations.
///
/// Covers `fn`, `const`, `type`, and `actor` — the same set [`collect_defs`]
/// locates. Each card pairs the written header (reusing the same renderers hover
/// uses for workspace declarations) with the role label and the `--` doc block
/// above the declaration, lifted from `classes`.
fn collect_cards(
    source: &str,
    items: &[Item],
    line_index: &LineIndex,
    classes: &[LineClass],
) -> HashMap<String, StdlibCard> {
    let mut cards = HashMap::new();
    for item in items {
        let (name, header, kind, name_span) = match item {
            Item::Fn(d) => (&d.name.text, fn_header(source, d), "function", d.name.span),
            Item::Const(d) => (
                &d.name.text,
                const_header(source, d),
                "constant",
                d.name.span,
            ),
            Item::Type(d) => (&d.name.text, type_header(source, d), "type", d.name.span),
            Item::Actor(d) => (
                &d.name.text,
                format!("actor {}", d.name.text),
                "actor",
                d.name.span,
            ),
            Item::Import(_) | Item::ClassDecl(_) | Item::InstanceDecl(_) => continue,
        };
        // Keep the first declaration of a given name; redeclarations are a
        // resolver error and not this layer's concern.
        cards.entry(name.clone()).or_insert_with(|| StdlibCard {
            header,
            kind,
            doc: doc_above(line_of(line_index, name_span), classes),
        });
    }
    cards
}

/// Collect `(class_name, method_name) → entry` pairs from a parsed module's
/// `class` declarations.
///
/// Only `class` declarations are walked; `instance` bodies are skipped, since
/// goto targets the method signature in the class, not an instance's definition
/// of it. The span used is the method name's identifier span. `module` is the
/// owning builtin id, carried so the global index can recover the module's
/// materialised URI and line index later. Each method's doc is the `--` block
/// directly above it, or — since stdlib classes document the class rather than
/// each method — the class's own doc block when the method has none.
fn collect_class_methods(
    module: StdlibModuleId,
    source: &str,
    items: &[Item],
    out: &mut HashMap<(String, String), ClassMethodEntry>,
) {
    let line_index = LineIndex::new(source);
    let classes = classify_lines(source);
    for item in items {
        if let Item::ClassDecl(decl) = item {
            let class_doc = doc_above(line_of(&line_index, decl.name.span), &classes);
            for method in &decl.methods {
                // Keep the first declaration of a given `(class, method)`; a
                // redeclaration is a resolver error and not this layer's concern.
                out.entry((decl.name.text.clone(), method.name.text.clone()))
                    .or_insert_with(|| ClassMethodEntry {
                        module: module.0,
                        name_span: method.name.span,
                        sig: build_signature(
                            source,
                            &method.name.text,
                            &method.params,
                            Some(&method.ret),
                        ),
                        doc: doc_above(line_of(&line_index, method.name.span), &classes)
                            .or_else(|| class_doc.clone()),
                    });
            }
        }
    }
}

/// Collect `fn name → signature` pairs for a module's top-level functions.
fn collect_fn_sigs(source: &str, items: &[Item]) -> HashMap<String, SignatureSig> {
    let mut sigs = HashMap::new();
    for item in items {
        if let Item::Fn(decl) = item {
            sigs.entry(decl.name.text.clone()).or_insert_with(|| {
                build_signature(source, &decl.name.text, &decl.params, decl.ret.as_ref())
            });
        }
    }
    sigs
}

/// Process-global index of stdlib class methods, keyed by `(class, method)`.
///
/// A class method binding (`Binding::ClassMethod`) carries only the class and
/// method names — no owning module — so the lookup must span every stdlib
/// module. The index is built once by parsing all embedded sources and pairs
/// each `(class, method)` with the owning module id and the method-name span,
/// which together resolve to a [`Location`] through the per-module cache.
fn class_method_index() -> &'static HashMap<(String, String), ClassMethodEntry> {
    static INDEX: OnceLock<HashMap<(String, String), ClassMethodEntry>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = HashMap::new();
        for builtin in BUILTINS {
            let Some(rel) = relative_source_path(builtin.name) else {
                continue;
            };
            let Some(source) = source_for(&rel) else {
                continue;
            };
            let parsed = ridge_parser::parse_source(source);
            collect_class_methods(builtin.id, source, &parsed.module.items, &mut index);
        }
        index
    })
}

/// Process-global `(module id, method name) → class name` index.
///
/// Derived from [`class_method_index`] with no extra parse: every stdlib class
/// method keyed by the builtin module its `class` is declared in and the method
/// name. It bridges the qualified form `Module.method`, which resolves to a
/// `Binding::StdlibSymbol` because the method is listed in the module's exports —
/// so the top-level def/card/signature lookups miss it (the method lives in a
/// `class` body, not as a `pub fn`). The first class wins if two classes in one
/// module ever declare a same-named method (none do today).
fn module_method_class_index() -> &'static HashMap<(u32, String), String> {
    static INDEX: OnceLock<HashMap<(u32, String), String>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index: HashMap<(u32, String), String> = HashMap::new();
        for ((class, method), entry) in class_method_index() {
            index
                .entry((entry.module, method.clone()))
                .or_insert_with(|| class.clone());
        }
        index
    })
}

/// The class owning the stdlib method `name` declared in `module`, when `name`
/// is a class method of a `class` declared in that builtin module. `None` for a
/// top-level symbol or an unknown name. Lets the qualified `Module.method` form
/// reuse the existing class-method card, location, and signature.
fn class_of_module_method(module: StdlibModuleId, name: &str) -> Option<&'static str> {
    module_method_class_index()
        .get(&(module.0, name.to_owned()))
        .map(String::as_str)
}

/// Build the [`StdlibModuleDef`] for a builtin module by parsing its embedded
/// source. Returns `None` if the id is unknown, the name has no source path, or
/// the source text is missing.
fn build_module_def(module: StdlibModuleId) -> Option<StdlibModuleDef> {
    let builtin = BUILTINS.get(module.0 as usize)?;
    let rel = relative_source_path(builtin.name)?;
    let source = source_for(&rel)?;

    let uri = Url::from_file_path(cache_root().join(&rel)).ok()?;
    let line_index = LineIndex::new(source);
    let parsed = ridge_parser::parse_source(source);
    let defs = collect_defs(&parsed.module.items);
    let fn_sigs = collect_fn_sigs(source, &parsed.module.items);
    let classes = classify_lines(source);
    let cards = collect_cards(source, &parsed.module.items, &line_index, &classes);

    Some(StdlibModuleDef {
        uri,
        line_index,
        defs,
        fn_sigs,
        cards,
    })
}

/// Convert a byte `span` to an LSP UTF-16 [`Range`] using `line_index`.
fn span_to_range(line_index: &LineIndex, span: Span) -> Range {
    let (start_line, start_char) = line_index.byte_to_utf16(span.start);
    let (end_line, end_char) = line_index.byte_to_utf16(span.end);
    Range {
        start: Position {
            line: start_line,
            character: start_char,
        },
        end: Position {
            line: end_line,
            character: end_char,
        },
    }
}

/// Run `f` with the cached [`StdlibModuleDef`] for `module`, populating the
/// cache on first access. Returns `None` if the module cannot be built or the
/// cache lock is poisoned.
fn with_module_def<T>(module: StdlibModuleId, f: impl FnOnce(&StdlibModuleDef) -> T) -> Option<T> {
    use std::collections::hash_map::Entry;

    ensure_materialised();
    let mut cache = module_cache().lock().ok()?;
    let result = match cache.entry(module.0) {
        Entry::Occupied(e) => Some(f(e.get())),
        Entry::Vacant(e) => build_module_def(module).map(|def| f(e.insert(def))),
    };
    drop(cache);
    result
}

/// Resolve a stdlib symbol `name` exported by `module` to its definition site.
///
/// Falls back to a class method declared in `module` when `name` is not a
/// top-level declaration — the qualified `Module.method` form resolves to a
/// stdlib symbol even though the method lives in a `class` body. Returns `None`
/// when the module has no materialised source, `name` is neither a top-level
/// declaration nor such a class method, or the cache is unavailable — goto then
/// reports "no definition" rather than failing.
#[must_use]
pub fn stdlib_location(module: StdlibModuleId, name: &str) -> Option<Location> {
    let top_level = with_module_def(module, |def| {
        let span = *def.defs.get(name)?;
        Some(Location {
            uri: def.uri.clone(),
            range: span_to_range(&def.line_index, span),
        })
    })
    .flatten();
    if top_level.is_some() {
        return top_level;
    }
    // Qualified class method (`Module.method`): jump to the method name in its
    // `class` declaration, the same target the unqualified form reaches.
    let class = class_of_module_method(module, name)?;
    stdlib_class_method_location(class, name)
}

/// The hover card (header + role + doc) of a stdlib symbol `name` in `module`.
///
/// Falls back to a class method declared in `module` when `name` is not a
/// top-level declaration, so the qualified `Module.method` form (which resolves
/// to a stdlib symbol) shows the same signature and doc as the unqualified one.
/// `None` when the module has no materialised source or `name` is neither.
#[must_use]
pub fn stdlib_symbol_card(module: StdlibModuleId, name: &str) -> Option<StdlibCard> {
    if let Some(card) = with_module_def(module, |def| def.cards.get(name).cloned()).flatten() {
        return Some(card);
    }
    // Qualified class method (`Module.method`): synthesise a card from the
    // method's signature and doc.
    let class = class_of_module_method(module, name)?;
    let (header, doc) = stdlib_class_method_card(class, name)?;
    Some(StdlibCard {
        header,
        kind: "class method",
        doc,
    })
}

/// The hover header and doc of a stdlib class method, by `(class, method)`.
///
/// The header is the method signature; the doc is the method's own `--` block
/// or, failing that, its class's. `None` when the pair is not a stdlib class
/// method — for example a class declared in the workspace.
#[must_use]
pub fn stdlib_class_method_card(
    class_name: &str,
    method: &str,
) -> Option<(String, Option<String>)> {
    class_method_index()
        .get(&(class_name.to_owned(), method.to_owned()))
        .map(|entry| (entry.sig.label.clone(), entry.doc.clone()))
}

/// Resolve a stdlib module alias to the start of its materialised source file.
#[must_use]
pub fn stdlib_module_location(module: StdlibModuleId) -> Option<Location> {
    with_module_def(module, |def| Location {
        uri: def.uri.clone(),
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        },
    })
}

/// Resolve a stdlib class method to the method-name signature in its `class`
/// declaration.
///
/// Looks `(class_name, method)` up in the process-global class-method index and
/// turns the owning module's method-name span into a [`Location`] through the
/// per-module cache. Returns `None` when the pair is not a stdlib class method —
/// for example a class declared in the workspace, which this layer does not
/// resolve — so goto reports "no definition" rather than failing.
#[must_use]
pub fn stdlib_class_method_location(class_name: &str, method: &str) -> Option<Location> {
    let entry = class_method_index().get(&(class_name.to_owned(), method.to_owned()))?;
    with_module_def(StdlibModuleId(entry.module), |def| Location {
        uri: def.uri.clone(),
        range: span_to_range(&def.line_index, entry.name_span),
    })
}

/// The signature of a stdlib top-level function for signature help.
///
/// Falls back to a class method declared in `module` when `name` is not a
/// top-level function, so signature help fires on the qualified `Module.method`
/// call form. Returns `None` when the module has no materialised source, `name`
/// is neither, or the cache is unavailable.
#[must_use]
pub(crate) fn stdlib_fn_signature(module: StdlibModuleId, name: &str) -> Option<SignatureSig> {
    if let Some(sig) = with_module_def(module, |def| def.fn_sigs.get(name).cloned()).flatten() {
        return Some(sig);
    }
    // Qualified class method (`Module.method`): its signature for signature help.
    let class = class_of_module_method(module, name)?;
    stdlib_class_method_signature(class, name)
}

/// The signature of a stdlib class method (`filter`, `joinOn`, …) for signature
/// help. Returns `None` when the pair is not a stdlib class method — for
/// example a class declared in the workspace.
#[must_use]
pub(crate) fn stdlib_class_method_signature(
    class_name: &str,
    method: &str,
) -> Option<SignatureSig> {
    class_method_index()
        .get(&(class_name.to_owned(), method.to_owned()))
        .map(|entry| entry.sig.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id of a builtin module by name, for tests.
    fn module_id(name: &str) -> StdlibModuleId {
        BUILTINS
            .iter()
            .find(|m| m.name == name)
            .map(|m| m.id)
            .expect("builtin module present")
    }

    #[test]
    fn relative_source_path_maps_dotted_names() {
        assert_eq!(
            relative_source_path("std.list").as_deref(),
            Some("list.ridge")
        );
        assert_eq!(
            relative_source_path("std.net.http").as_deref(),
            Some("net/http.ridge")
        );
        assert_eq!(relative_source_path("notstd.list"), None);
    }

    #[test]
    fn stdlib_location_points_into_list_source() {
        // `map` is a plain `pub fn` of `std.list`.
        let loc = stdlib_location(module_id("std.list"), "map")
            .expect("std.list should export a locatable `map`");
        let path = loc.uri.to_file_path().expect("location uri is a file path");
        assert!(
            path.ends_with("list.ridge"),
            "expected a path ending in list.ridge, got {path:?}"
        );
        // The name sits past the start of the file, so the range is non-trivial.
        assert!(
            loc.range.start.line > 0 || loc.range.start.character > 0,
            "expected a real declaration position, got {:?}",
            loc.range.start
        );
    }

    #[test]
    fn stdlib_module_location_points_at_file_start() {
        let loc = stdlib_module_location(module_id("std.list"))
            .expect("std.list should have a module location");
        let path = loc.uri.to_file_path().expect("uri is a file path");
        assert!(path.ends_with("list.ridge"), "got {path:?}");
        assert_eq!(loc.range.start.line, 0);
        assert_eq!(loc.range.start.character, 0);
    }

    #[test]
    fn unknown_symbol_resolves_to_none() {
        assert!(stdlib_location(module_id("std.list"), "definitelyNotAStdlibName").is_none());
    }

    #[test]
    fn out_of_range_module_resolves_to_none() {
        assert!(stdlib_location(StdlibModuleId(u32::MAX), "map").is_none());
        assert!(stdlib_module_location(StdlibModuleId(u32::MAX)).is_none());
    }

    #[test]
    fn class_method_location_points_at_method_signature() {
        // `filter` is a method of the `Refinable q p | q -> p` class declared in
        // the repo module.
        let loc = stdlib_class_method_location("Refinable", "filter")
            .expect("Refinable.filter should be a locatable stdlib class method");
        let path = loc.uri.to_file_path().expect("location uri is a file path");
        assert!(
            path.ends_with("repo.ridge"),
            "expected a path ending in repo.ridge, got {path:?}"
        );
        // The signature sits well past the start of the file, so the range is
        // non-trivial and points at the method name, not `(0, 0)`.
        assert!(
            loc.range.start.line > 0 || loc.range.start.character > 0,
            "expected a real method position, got {:?}",
            loc.range.start
        );
    }

    #[test]
    fn unknown_class_method_resolves_to_none() {
        // A real method name on the wrong class, and a wholly made-up pair, both
        // miss the index.
        assert!(stdlib_class_method_location("Refinable", "definitelyNotAMethod").is_none());
        assert!(stdlib_class_method_location("NotAStdlibClass", "filter").is_none());
    }

    #[test]
    fn symbol_card_carries_header_and_doc() {
        // `map` is a documented `pub fn` of `std.list`, with an `@ffi` attribute
        // between its `--` doc and the declaration — the doc lift skips it.
        let card = stdlib_symbol_card(module_id("std.list"), "map")
            .expect("std.list should expose a card for `map`");
        assert_eq!(card.kind, "function");
        assert!(
            card.header.contains("pub fn map") && card.header.contains("-> List b"),
            "header should be the written signature, got {:?}",
            card.header
        );
        let doc = card.doc.expect("`map` is documented");
        assert!(
            doc.contains("Apply a function to each element"),
            "doc should be lifted from the `--` block, got {doc:?}"
        );
    }

    #[test]
    fn symbol_card_unknown_name_is_none() {
        assert!(stdlib_symbol_card(module_id("std.list"), "definitelyNotAStdlibName").is_none());
    }

    #[test]
    fn class_method_card_falls_back_to_class_doc() {
        // `Refinable` documents the class, not `filter`; the card lifts the class
        // doc as the method's.
        let (header, doc) = stdlib_class_method_card("Refinable", "filter")
            .expect("Refinable.filter should expose a card");
        assert!(
            header.starts_with("filter"),
            "header should be the method signature, got {header:?}"
        );
        let doc = doc.expect("Refinable.filter inherits the class doc");
        assert!(
            doc.contains("One `filter` for both a query and a join"),
            "doc should fall back to the class block, got {doc:?}"
        );
    }

    #[test]
    fn class_method_card_unknown_is_none() {
        assert!(stdlib_class_method_card("Refinable", "definitelyNotAMethod").is_none());
        assert!(stdlib_class_method_card("NotAStdlibClass", "filter").is_none());
    }

    #[test]
    fn qualified_class_method_card_via_module() {
        // The idiomatic `Repo.filter` resolves to a stdlib symbol of std.repo, not
        // a class-method binding. `filter` is not a top-level decl there — it is a
        // method of `Refinable` — so the card comes from the class-method fallback.
        let card = stdlib_symbol_card(module_id("std.repo"), "filter")
            .expect("Repo.filter should resolve to a class-method card");
        assert_eq!(card.kind, "class method");
        assert!(
            card.header.starts_with("filter"),
            "header should be the method signature, got {:?}",
            card.header
        );
        let doc = card.doc.expect("filter inherits the Refinable class doc");
        assert!(
            doc.contains("One `filter` for both a query and a join"),
            "doc should be the class block, got {doc:?}"
        );
    }

    #[test]
    fn qualified_class_method_location_via_module() {
        // Goto on `Repo.filter` jumps into repo.ridge through the same fallback.
        let loc = stdlib_location(module_id("std.repo"), "filter")
            .expect("Repo.filter should be locatable through its class");
        let path = loc.uri.to_file_path().expect("location uri is a file path");
        assert!(path.ends_with("repo.ridge"), "got {path:?}");
        assert!(
            loc.range.start.line > 0 || loc.range.start.character > 0,
            "expected a real method position, got {:?}",
            loc.range.start
        );
    }

    #[test]
    fn qualified_class_method_signature_via_module() {
        // Signature help fires on the qualified `Repo.filter (...)` call form.
        let sig = stdlib_fn_signature(module_id("std.repo"), "filter")
            .expect("Repo.filter should expose a signature through its class");
        assert!(
            sig.label.starts_with("filter"),
            "expected the method signature, got {:?}",
            sig.label
        );
    }

    #[test]
    fn top_level_symbol_takes_precedence_over_class_method() {
        // `repo` is a top-level `pub fn`; it must resolve as a function, never via
        // the class-method fallback.
        let card = stdlib_symbol_card(module_id("std.repo"), "repo")
            .expect("std.repo should expose a card for the `repo` constructor");
        assert_eq!(card.kind, "function");
        assert!(
            card.header.contains("pub fn repo"),
            "header should be the written signature, got {:?}",
            card.header
        );
    }

    #[test]
    fn qualified_class_method_wrong_module_is_none() {
        // `orderBy` is a std.repo class method; std.list neither exports it nor
        // declares its class, so the fallback finds nothing there.
        assert!(stdlib_symbol_card(module_id("std.list"), "orderBy").is_none());
        assert!(stdlib_location(module_id("std.list"), "orderBy").is_none());
        assert!(stdlib_fn_signature(module_id("std.list"), "orderBy").is_none());
    }
}
