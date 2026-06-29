//! Regression: a parameter (or `let`/`var`) that shares its name with a
//! same-module top-level fn must lower to the bound variable, NOT a reference to
//! that fn. Before the fix, the `IrExpr::Local` arm emitted a `LocalFnRef` for
//! any name in `fn_arity` (which is seeded with every top-level fn), so the
//! shadowing param was miscompiled into a curried `#Fun<...>` value — a silent
//! miscompile that typechecked clean and only surfaced at runtime.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, run_pipeline};
use ridge_codegen_erl::codegen_module_ast;
use ridge_codegen_erl::printer::print_module;

#[test]
fn param_shadowing_top_level_fn_lowers_to_variable() {
    // `tag`'s parameter `label` shadows the top-level fn `label`. The body must
    // emit the parameter variable, not a reference to the function.
    let src = r#"
fn label (x: Text) -> Text = x

fn tag (label: Text) -> Text = label

pub fn run () -> Text = tag "ok"
"#;
    let tw = make_workspace("shadow_param_fn", "Main", src);
    let result = run_pipeline(&tw.path);
    let m = result.lowered.modules[0].as_ref().expect("module 0");
    let cerl = codegen_module_ast(m, &result.lowered).expect("codegen");
    let text = print_module(&cerl);

    // Isolate `tag`'s own clause (everything up to its closing `-| []`).
    let tag_block = text
        .split("'tag'/1 =")
        .nth(1)
        .expect("tag/1 must be generated");
    let tag_body = tag_block.split("-| []").next().expect("tag clause body");

    assert!(
        tag_body.contains("V_Label"),
        "tag's body must reference the parameter variable V_Label; got:\n{tag_body}"
    );
    assert!(
        !tag_body.contains("'label'"),
        "tag's body must NOT reference the top-level fn 'label' \
         (param-shadows-module-fn miscompile regressed); got:\n{tag_body}"
    );
}
