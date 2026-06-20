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

/// The materialised, parsed form of one stdlib module.
struct StdlibModuleDef {
    /// `file://` URL of the materialised source on disk.
    uri: Url,
    /// UTF-16 ↔ byte line index, built from the in-memory source so spans and
    /// the on-disk file agree exactly.
    line_index: LineIndex,
    /// Top-level declaration name → its definition span.
    defs: HashMap<String, Span>,
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

/// Collect `(class_name, method_name) → (module-id, method-name span)` pairs
/// from a parsed module's `class` declarations.
///
/// Only `class` declarations are walked; `instance` bodies are skipped, since
/// goto targets the method signature in the class, not an instance's definition
/// of it. The span used is the method name's identifier span. `module` is the
/// owning builtin id, carried so the global index can recover the module's
/// materialised URI and line index later.
fn collect_class_methods(
    module: StdlibModuleId,
    items: &[Item],
    out: &mut HashMap<(String, String), (u32, Span)>,
) {
    for item in items {
        if let Item::ClassDecl(decl) = item {
            for method in &decl.methods {
                // Keep the first declaration of a given `(class, method)`; a
                // redeclaration is a resolver error and not this layer's concern.
                out.entry((decl.name.text.clone(), method.name.text.clone()))
                    .or_insert((module.0, method.name.span));
            }
        }
    }
}

/// Process-global index of stdlib class methods, keyed by `(class, method)`.
///
/// A class method binding (`Binding::ClassMethod`) carries only the class and
/// method names — no owning module — so the lookup must span every stdlib
/// module. The index is built once by parsing all embedded sources and pairs
/// each `(class, method)` with the owning module id and the method-name span,
/// which together resolve to a [`Location`] through the per-module cache.
fn class_method_index() -> &'static HashMap<(String, String), (u32, Span)> {
    static INDEX: OnceLock<HashMap<(String, String), (u32, Span)>> = OnceLock::new();
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
            collect_class_methods(builtin.id, &parsed.module.items, &mut index);
        }
        index
    })
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

    Some(StdlibModuleDef {
        uri,
        line_index,
        defs,
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
/// Returns `None` when the module has no materialised source, the name is not a
/// top-level declaration, or the cache is unavailable — goto then reports "no
/// definition" rather than failing.
#[must_use]
pub fn stdlib_location(module: StdlibModuleId, name: &str) -> Option<Location> {
    with_module_def(module, |def| {
        let span = *def.defs.get(name)?;
        Some(Location {
            uri: def.uri.clone(),
            range: span_to_range(&def.line_index, span),
        })
    })
    .flatten()
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
    let &(module_id, span) =
        class_method_index().get(&(class_name.to_owned(), method.to_owned()))?;
    with_module_def(StdlibModuleId(module_id), |def| Location {
        uri: def.uri.clone(),
        range: span_to_range(&def.line_index, span),
    })
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
}
