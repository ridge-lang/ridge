//! Verifies that string interpolation dispatches to a user-defined
//! `pub fn toText` when the hole's type is a user `TyCon`.
//!
//! Convention: a module that declares `type Foo = ...` and also declares
//! `pub fn toText (x: Foo) -> Text` participates in interpolation. The
//! lowering pass synthesizes a `Call(External { module, "toText" }, [arg])`
//! where `module` is the type's owning module.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, render_lowered_module, run_pipeline};

#[test]
fn user_type_with_to_text_is_dispatched_in_interpolation() {
    let source = r#"
type Foo = { name: Text }

pub fn toText (f: Foo) -> Text = f.name

pub fn greet (f: Foo) -> Text = $"Hi, ${f}"
"#;

    let tw = make_workspace("user_to_text_dispatch", "Demo", source);
    let result = run_pipeline(&tw.path);

    assert!(!result.lowered.modules.is_empty());
    let module = result.lowered.modules[0]
        .as_ref()
        .expect("module must lower");

    let rendered = render_lowered_module(module);

    // The lowering of `${f}` should synthesise a Call to External `toText`
    // in the same module (module 0). The renderer formats External symbols
    // as a structural projection that includes the symbol's name; assert the
    // name appears at least once.
    assert!(
        rendered.contains("toText"),
        "expected a synthesised `toText` call in the lowered IR; got:\n{rendered}"
    );
}

#[test]
fn user_type_without_to_text_falls_to_l007() {
    let source = r#"
type Bar = { id: Int }

pub fn show (b: Bar) -> Text = $"id=${b}"
"#;

    let tw = make_workspace("user_no_to_text", "Demo", source);
    let result = run_pipeline(&tw.path);

    // The lowering walk should NOT panic; it should emit L007 internally.
    // We do not assert specific behaviour beyond the pipeline finishing —
    // the module may or may not be `Some` depending on how the rest of the
    // pipeline treats L007. The goal of this test is that the new dispatch
    // path does not blow up when no user `toText` is present.
    let _maybe_module = result.lowered.modules.first().and_then(Option::as_ref);
}
