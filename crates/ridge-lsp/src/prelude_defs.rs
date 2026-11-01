//! Source-backed editor definitions for compiler-provided prelude declarations.

use std::sync::OnceLock;

use ridge_ast::{ClassDecl, Item};
use ridge_lexer::LineIndex;
use sha2::{Digest, Sha256};
use tower_lsp::lsp_types::{Location, Position, Range, SymbolKind, TypeHierarchyItem, Url};

use crate::index::{build_signature, SignatureSig};

/// Readable declarations of compiler-provided classes.
const SOURCE: &str = include_str!("prelude.ridge");

/// Parse the small prelude source once, sharing spans across editor surfaces.
fn declaration(name: &str) -> Option<&'static ClassDecl> {
    static MODULE: OnceLock<ridge_ast::Module> = OnceLock::new();
    MODULE
        .get_or_init(|| ridge_parser::parse_source(SOURCE).module)
        .items
        .iter()
        .find_map(|item| match item {
            Item::ClassDecl(decl)
                if decl.name.text == name || decl.methods.iter().any(|m| m.name.text == name) =>
            {
                Some(decl)
            }
            _ => None,
        })
}

/// Signature of the prelude method, derived from its readable declaration.
pub fn signature(class: &str, method: &str) -> Option<SignatureSig> {
    let decl = declaration(class)?;
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
    let decl = declaration(name)?;
    let header = if name == decl.name.text {
        SOURCE
            .get(decl.span.start as usize..)?
            .lines()
            .next()?
            .trim_end()
            .trim_end_matches('=')
            .trim_end()
            .to_owned()
    } else {
        signature(&decl.name.text, name)?.label
    };
    let mut comments = SOURCE
        .get(..decl.span.start as usize)?
        .lines()
        .rev()
        .take_while(|line| line.starts_with("--"))
        .map(|line| line.trim_start_matches("--").trim())
        .collect::<Vec<_>>();
    comments.reverse();
    Some((header, Some(comments.join("\n"))))
}

/// Materialise an immutable, content-addressed definition for editor navigation.
pub fn location(name: &str) -> Option<Location> {
    let decl = declaration(name)?;
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
    let decl = declaration(name)?;
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

/// Direct superclass names in the readable intrinsic declaration.
pub fn superclasses(name: &str) -> Option<Vec<&'static str>> {
    Some(
        declaration(name)?
            .superclasses
            .iter()
            .map(|c| c.class.text.as_str())
            .collect(),
    )
}

/// Intrinsic classes directly extending the requested class.
pub fn subclasses(name: &str) -> Vec<TypeHierarchyItem> {
    ["ErrorFrom", "Eq", "Ord", "Hash"]
        .into_iter()
        .filter(|class| superclasses(class).is_some_and(|supers| supers.contains(&name)))
        .filter_map(hierarchy_item)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn editor_declaration_matches_the_compiler_class_contract() {
        let parsed = ridge_parser::parse_source(super::SOURCE);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut classes = ridge_typecheck::ClassTable::new();
        ridge_typecheck::register_prelude_classes(&mut classes);
        for id in [
            ridge_types::ERROR_FROM_CLASS,
            ridge_types::EQ_CLASS,
            ridge_types::ORD_CLASS,
            ridge_types::HASH_CLASS,
        ] {
            let class = classes.get(id).unwrap();
            let declaration = super::declaration(&class.name).unwrap();
            assert_eq!(declaration.name.text, class.name);
            assert_eq!(declaration.ty_vars.len(), class.arity);
            assert!(declaration.fundeps.is_empty());
            assert_eq!(
                declaration
                    .superclasses
                    .iter()
                    .map(|c| c.class.text.as_str())
                    .collect::<Vec<_>>(),
                class
                    .superclasses
                    .iter()
                    .map(|id| classes.get(*id).unwrap().name.as_str())
                    .collect::<Vec<_>>()
            );
            assert_eq!(declaration.methods.len(), class.method_sigs.len());
            for (declared, registered) in declaration.methods.iter().zip(&class.method_sigs) {
                assert_eq!(declared.name.text, registered.name);
                assert_eq!(declared.params.len(), registered.arity);
            }
        }
    }
}
