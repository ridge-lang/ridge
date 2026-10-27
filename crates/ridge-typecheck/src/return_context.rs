//! Validate lexical return destinations independently of expression inference.
//!
//! Some source expressions, including actor state defaults, are not visited by
//! inference. Return validity must not depend on which expressions it reaches.

use ridge_ast::visit::{walk_expr, Visit};
use ridge_ast::{Expr, FnDecl, InstanceDecl, Module};

use crate::TypeError;

/// Accumulate absent return destinations without changing inference state.
pub fn check_module(module: &Module, errors: &mut Vec<TypeError>) {
    struct Check<'a>(&'a mut Vec<TypeError>);

    impl<'ast> Visit<'ast> for Check<'_> {
        // Every return inside a source function has a destination. Payload
        // types and nested functions are still checked by ordinary inference.
        fn visit_fn_decl(&mut self, _decl: &'ast FnDecl) {}

        // Instance method bodies are functions, but their AST representation
        // is MethodDef rather than FnDecl.
        fn visit_instance_decl(&mut self, _decl: &'ast InstanceDecl) {}

        fn visit_expr(&mut self, expr: &'ast Expr) {
            if matches!(expr, Expr::Lambda { .. }) {
                return;
            }
            if let Expr::Return { span, .. } = expr {
                self.0
                    .push(TypeError::ReturnOutsideFunction { span: *span });
            }
            // A try is not a return boundary. Continue through operands and
            // later statements, including when a return is already invalid.
            walk_expr(self, expr);
        }
    }

    Check(errors).visit_module(module);
}
