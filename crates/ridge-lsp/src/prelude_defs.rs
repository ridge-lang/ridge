//! Source-backed editor definitions for compiler-provided prelude declarations.

use std::sync::OnceLock;

use ridge_ast::{ClassDecl, Item};
use ridge_lexer::LineIndex;
use sha2::{Digest, Sha256};
use tower_lsp::lsp_types::{Location, Position, Range, SymbolKind, TypeHierarchyItem, Url};

use crate::index::{build_signature, SignatureSig};

/// Readable declaration of the compiler-provided conversion class.
const SOURCE: &str = include_str!("prelude.ridge");

/// Parse the small prelude source once, sharing spans across editor surfaces.
fn declaration() -> Option<&'static ClassDecl> {
    static MODULE: OnceLock<ridge_ast::Module> = OnceLock::new();
    MODULE
        .get_or_init(|| ridge_parser::parse_source(SOURCE).module)
        .items
        .iter()
        .find_map(|item| match item {
            Item::ClassDecl(decl) => Some(decl),
            _ => None,
        })
}

/// Signature of the prelude method, derived from its readable declaration.
pub fn signature(class: &str, method: &str) -> Option<SignatureSig> {
    let decl = declaration()?;
    if decl.name.text != class {
        return None;
    }
    let method = decl.methods.iter().find(|m| m.name.text == method)?;
    Some(build_signature(
        SOURCE,
        &method.name.text,
        &method.params,
        Some(&method.ret),
    ))
}

/// Header and documentation for either the class or its method.
pub fn card(name: &str) -> Option<(String, Option<String>)> {
    let decl = declaration()?;
    let header = if name == decl.name.text {
        let vars = decl
            .ty_vars
            .iter()
            .map(|v| v.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        format!("class {} {vars}", decl.name.text)
    } else {
        signature(&decl.name.text, name)?.label
    };
    let doc = SOURCE
        .lines()
        .take_while(|line| line.starts_with("--"))
        .map(|line| line.trim_start_matches("--").trim())
        .collect::<Vec<_>>()
        .join("\n");
    Some((header, Some(doc)))
}

/// Materialise an immutable, content-addressed definition for editor navigation.
pub fn location(name: &str) -> Option<Location> {
    let decl = declaration()?;
    let span = if name == decl.name.text {
        decl.name.span
    } else {
        decl.methods.iter().find(|m| m.name.text == name)?.name.span
    };
    let digest = format!("{:x}", Sha256::digest(SOURCE.as_bytes()));
    let root = std::env::temp_dir()
        .join("ridge-lsp")
        .join("prelude")
        .join(digest);
    let path = root.join("prelude.ridge");
    if !std::fs::read(&path).is_ok_and(|bytes| bytes == SOURCE.as_bytes()) {
        std::fs::create_dir_all(&root).ok()?;
        std::fs::write(&path, SOURCE).ok()?;
    }
    let mut permissions = std::fs::metadata(&path).ok()?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).ok()?;
    let lines = LineIndex::new(SOURCE);
    let start = lines.byte_to_utf16(span.start);
    let end = lines.byte_to_utf16(span.end);
    Some(Location {
        uri: Url::from_file_path(path).ok()?,
        range: Range {
            start: Position {
                line: start.0,
                character: start.1,
            },
            end: Position {
                line: end.0,
                character: end.1,
            },
        },
    })
}

/// The intrinsic class participates in the same instance hierarchy as source classes.
pub fn hierarchy_item(name: &str) -> Option<TypeHierarchyItem> {
    let decl = declaration()?;
    if decl.name.text != name {
        return None;
    }
    let location = location(name)?;
    let lines = LineIndex::new(SOURCE);
    let end = lines.byte_to_utf16(decl.span.end);
    Some(TypeHierarchyItem {
        name: name.to_owned(),
        kind: SymbolKind::INTERFACE,
        tags: None,
        detail: card(name).map(|card| card.0),
        uri: location.uri,
        range: Range {
            start: location.range.start,
            end: Position::new(end.0, end.1),
        },
        selection_range: location.range,
        data: Some(serde_json::json!({"name":name,"kind":"class"})),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn editor_declaration_matches_the_compiler_class_contract() {
        let declaration = super::declaration().unwrap();
        let mut classes = ridge_typecheck::ClassTable::new();
        ridge_typecheck::register_prelude_classes(&mut classes);
        let class = classes.get(ridge_types::ERROR_FROM_CLASS).unwrap();
        assert_eq!(declaration.name.text, class.name);
        assert_eq!(declaration.ty_vars.len(), class.arity);
        assert!(declaration.fundeps.is_empty());
        assert_eq!(declaration.methods.len(), class.method_sigs.len());
        for (declared, registered) in declaration.methods.iter().zip(&class.method_sigs) {
            assert_eq!(declared.name.text, registered.name);
            assert_eq!(declared.params.len(), registered.arity);
        }
    }
}
