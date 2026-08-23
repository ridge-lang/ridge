//! `Item::span` covers what the source writes above a declaration.
//!
//! A doc comment or a `@test` attribute is parsed before the declaration it
//! belongs to, so the declaration's own span starts below it.  Anything asking
//! where an item sits on the page — blank-line normalisation in the formatter,
//! folding ranges in the editor — has to see the whole thing, or it reads the
//! attribute as part of the gap between two items.

use ridge_ast::Item;

/// Parse `src` and return the text each item's span covers.
fn item_texts(src: &str) -> Vec<String> {
    let parsed = ridge_parser::parse_module_with_trivia(src);
    assert!(
        parsed.result.errors.is_empty(),
        "fixture must parse: {:?}",
        parsed.result.errors.first().map(ToString::to_string)
    );
    let n = &parsed.normalised_src;
    parsed
        .result
        .module
        .items
        .iter()
        .map(|item| {
            let s = item.span();
            n[s.start as usize..s.end as usize].to_string()
        })
        .collect()
}

/// The single item of `src` reaches back over its leading trivia.
fn assert_covers(src: &str, expected: &str) {
    let texts = item_texts(src);
    assert_eq!(
        texts.len(),
        1,
        "fixture must hold exactly one item: {texts:?}"
    );
    assert_eq!(texts[0], expected, "item span text");
}

#[test]
fn fn_span_reaches_over_its_attribute() {
    assert_covers(
        "@test \"alpha\"\npub fn t_alpha () -> Result Unit Text = Ok ()\n",
        "@test \"alpha\"\npub fn t_alpha () -> Result Unit Text = Ok ()",
    );
}

#[test]
fn fn_span_reaches_over_every_attribute_when_there_is_more_than_one() {
    assert_covers(
        "@test \"a\"\n@test \"b\"\npub fn t () -> Result Unit Text = Ok ()\n",
        "@test \"a\"\n@test \"b\"\npub fn t () -> Result Unit Text = Ok ()",
    );
}

#[test]
fn fn_span_reaches_over_its_doc_comment() {
    assert_covers(
        "---\nAdds one.\n---\npub fn inc (x: Int) -> Int = x + 1\n",
        "---\nAdds one.\n---\npub fn inc (x: Int) -> Int = x + 1",
    );
}

#[test]
fn fn_span_reaches_over_a_doc_comment_and_an_attribute_together() {
    assert_covers(
        "---\nChecks alpha.\n---\n@test \"alpha\"\npub fn t_alpha () -> Result Unit Text = Ok ()\n",
        "---\nChecks alpha.\n---\n@test \"alpha\"\npub fn t_alpha () -> Result Unit Text = Ok ()",
    );
}

#[test]
fn a_blank_line_between_the_doc_comment_and_the_declaration_stays_inside() {
    assert_covers(
        "---\nAdds one.\n---\n\npub fn inc (x: Int) -> Int = x + 1\n",
        "---\nAdds one.\n---\n\npub fn inc (x: Int) -> Int = x + 1",
    );
}

#[test]
fn every_declaration_kind_reaches_over_its_doc_comment() {
    let doc = "---\nWhat it is.\n---\n";
    for decl in [
        "import std.list as L",
        "pub const limit: Int = 3",
        "pub type Point = Int",
        "pub fn inc (x: Int) -> Int = x + 1",
        "actor Counter =\n    state count: Int = 0\n\n    on bump () -> Unit =\n        count <- count + 1",
        "class Encode a =\n    encode (x: a) -> Text",
        "instance Encode Int =\n    encode (x: Int) -> Text = \"i\"",
    ] {
        let src = format!("{doc}{decl}\n");
        let texts = item_texts(&src);
        assert_eq!(texts.len(), 1, "one item for {decl:?}, got {texts:?}");
        assert!(
            texts[0].starts_with("---\nWhat it is.\n---"),
            "span must start at the doc comment for {decl:?}, got {:?}",
            texts[0]
        );
    }
}

#[test]
fn a_declaration_with_no_leading_trivia_is_unchanged() {
    // `pub` sits outside the parser's declaration span, and `Item::span` does
    // not widen it — nothing above the declaration belongs to the item here.
    let parsed = ridge_parser::parse_module_with_trivia("pub fn inc (x: Int) -> Int = x + 1\n");
    let items = &parsed.result.module.items;
    assert_eq!(items.len(), 1, "fixture must hold exactly one item");
    let declaration_span = items.iter().find_map(|item| match item {
        Item::Fn(decl) => Some(decl.span),
        _ => None,
    });
    assert_eq!(
        Some(items[0].span()),
        declaration_span,
        "no doc, no attrs: the item span is the declaration span"
    );
}
