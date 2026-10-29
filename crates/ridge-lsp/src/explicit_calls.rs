//! Quick fixes from the compiler's semantic message-call inventory.

use std::collections::HashMap;

use ridge_ast::{visit::Visit, Expr, Span};
use ridge_resolve::{ModuleId, NodeIdMap, NodeKind, ResolvedWorkspace};
use ridge_typecheck::TypedWorkspace;
use ridge_typecheck::{TypeError, TypedModule};
use ridge_types::{
    shape_key::{type_to_key, TyKey},
    CapRow, Type,
};
use tower_lsp::lsp_types::{Position, Range};

use crate::index::{SyntaxFix, WorkspaceIndex};

/// Repair a missing invocation marker without guessing a handler's signature.
#[must_use]
pub fn collect_explicit_call_fixes(
    index: &WorkspaceIndex,
    typed: &TypedWorkspace,
) -> Vec<SyntaxFix> {
    let mut fixes = Vec::new();
    for module in &typed.modules {
        let mi = module.id.0 as usize;
        let (Some(Some(uri)), Some(lines), Some(source)) = (
            index.module_uris.get(mi),
            index.line_indices.get(mi),
            index.module_text.get(mi),
        ) else {
            continue;
        };
        for site in &module.message_call_sites {
            let Some(new_text) = site.replacement() else {
                continue;
            };
            if source.get(site.message.start as usize..site.message.end as usize)
                != Some(site.handler.as_str())
            {
                continue;
            }
            let (sl, sc) = lines.byte_to_utf16(site.message.start);
            let (el, ec) = lines.byte_to_utf16(site.message.end);
            let range = Range::new(Position::new(sl, sc), Position::new(el, ec));
            fixes.push(SyntaxFix {
                uri: uri.clone(),
                decl_range: range,
                edit_range: range,
                new_text,
                code: "T063",
                title: "Add explicit `()` to this message invocation".to_owned(),
                extra_edits: Vec::new(),
            });
        }
    }
    fixes
}

/// Suggest an explicit call only for a pure function reference at a type error.
#[must_use]
pub fn collect_function_call_fixes(
    index: &WorkspaceIndex,
    typed: &TypedWorkspace,
    resolved: &ResolvedWorkspace,
    errors: &[(ModuleId, TypeError)],
) -> Vec<SyntaxFix> {
    let mut fixes = Vec::new();
    let mut requests = function_requests(errors);
    for module in &typed.modules {
        let Some(requests) = requests.remove(&module.id) else {
            continue;
        };
        let mi = module.id.0 as usize;
        let (Some(rm), Some(Some(uri)), Some(lines), Some(source)) = (
            resolved.modules.get(mi),
            index.module_uris.get(mi),
            index.line_indices.get(mi),
            index.module_text.get(mi),
        ) else {
            continue;
        };
        let mut by_span: HashMap<Span, Vec<usize>> = HashMap::new();
        for (i, request) in requests.iter().enumerate() {
            by_span.entry(request.diagnostic).or_default().push(i);
        }
        let mut finder = ReferenceFinder {
            module,
            nodes: &rm.node_ids,
            requests,
            by_span,
            found: Vec::new(),
        };
        finder.visit_module(&module.ast);
        for (span, request) in &finder.found {
            let request = &finder.requests[*request];
            let Some(text) = source.get(span.start as usize..span.end as usize) else {
                continue;
            };
            let range = |span: Span| {
                let (sl, sc) = lines.byte_to_utf16(span.start);
                let (el, ec) = lines.byte_to_utf16(span.end);
                Range::new(Position::new(sl, sc), Position::new(el, ec))
            };
            fixes.push(SyntaxFix {
                uri: uri.clone(),
                decl_range: range(request.diagnostic),
                edit_range: range(*span),
                new_text: format!("({text} ())"),
                code: request.code,
                title: "Invoke this function explicitly with `()`".to_owned(),
                extra_edits: Vec::new(),
            });
        }
    }
    fixes
}

struct FunctionCallRequest {
    function: TyKey,
    diagnostic: Span,
    code: &'static str,
}

fn function_requests(
    errors: &[(ModuleId, TypeError)],
) -> HashMap<ModuleId, Vec<FunctionCallRequest>> {
    let mut requests: HashMap<_, Vec<_>> = HashMap::new();
    for (module, error) in errors {
        let Some((function, diagnostic)) =
            ridge_typecheck::explicit_calls::missing_function_call(error)
        else {
            continue;
        };
        let Type::Fn {
            caps: CapRow::Concrete(caps),
            ..
        } = function
        else {
            continue;
        };
        if !caps.is_empty() {
            continue;
        }
        requests
            .entry(*module)
            .or_default()
            .push(FunctionCallRequest {
                function: type_to_key(function),
                diagnostic,
                code: error.code(),
            });
    }
    requests
}

struct ReferenceFinder<'a> {
    module: &'a TypedModule,
    nodes: &'a NodeIdMap,
    requests: Vec<FunctionCallRequest>,
    by_span: HashMap<Span, Vec<usize>>,
    found: Vec<(Span, usize)>,
}

impl ReferenceFinder<'_> {
    fn reference_key(&self, expr: &Expr) -> Option<TyKey> {
        if !matches!(
            expr,
            Expr::Ident(_) | Expr::Qualified(_) | Expr::Paren { .. }
        ) {
            return None;
        }
        let node = self.nodes.get(expr.span(), NodeKind::Expr)?;
        let ty = self.module.node_types.get(node.0 as usize)?.as_ref()?;
        Some(type_to_key(ty))
    }
}

impl<'ast> Visit<'ast> for ReferenceFinder<'_> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let Some(requests) = self.by_span.get(&expr.span()) {
            let reference = if let Expr::If { cond, .. } = expr {
                cond.as_ref()
            } else {
                expr
            };
            if let Some(key) = self.reference_key(reference) {
                for &request in requests {
                    if self.requests[request].function == key {
                        self.found.push((reference.span(), request));
                    }
                }
            }
        }
        ridge_ast::visit::walk_expr(self, expr);
    }
}
