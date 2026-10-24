//! Repairs for asks that now return a Result instead of a bare reply.

use std::collections::HashSet;

use ridge_ast::{visit::Visit, Body, Expr, FnDecl, Span};
use ridge_resolve::{ModuleId, NodeIdMap, NodeKind, ResolvedWorkspace};
use ridge_typecheck::{ctx::InferCtx, error::TypeDesc, TypeError, TypedModule, TypedWorkspace};
use ridge_types::{shape_key::type_to_key, Constraint, Type, ERROR_FROM_CLASS};
use tower_lsp::lsp_types::{Position, Range};

use crate::index::{SyntaxFix, WorkspaceIndex};

/// Offer propagation when the reply fits and its scope accepts the error.
///
/// The compiler's own class solver checks
/// concrete conversions; no editor-side instance selection is maintained.
#[must_use]
pub fn collect_ask_fixes(
    index: &WorkspaceIndex,
    typed: &TypedWorkspace,
    resolved: &ResolvedWorkspace,
    errors: &[(ModuleId, TypeError)],
) -> Vec<SyntaxFix> {
    let mut out = Vec::new();
    for module in &typed.modules {
        if !errors.iter().any(|(id, error)| {
            *id == module.id
                && matches!(
                    error,
                    TypeError::TypeMismatch { .. }
                        | TypeError::TypeMismatchInCall { .. }
                        | TypeError::DiscardedResult { .. }
                )
        }) {
            continue;
        }
        let mi = module.id.0 as usize;
        let Some(rm) = resolved.modules.get(mi) else {
            continue;
        };
        let mut finder = AskFinder {
            typed,
            module,
            nodes: &rm.node_ids,
            errors,
            scope: None,
            boundary: None,
            callback_scopes: Vec::new(),
            paren_asks: Vec::new(),
            out: Vec::new(),
        };
        finder.visit_module(&module.ast);
        let (Some(Some(uri)), Some(li)) = (index.module_uris.get(mi), index.line_indices.get(mi))
        else {
            continue;
        };
        for (ask, diagnostic, code) in finder.out {
            let (line, col) = li.byte_to_utf16(ask.end);
            let position = Position::new(line, col);
            let (sl, sc) = li.byte_to_utf16(diagnostic.start);
            let (el, ec) = li.byte_to_utf16(diagnostic.end);
            let discarded = code == "T022";
            out.push(SyntaxFix {
                uri: uri.clone(),
                decl_range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
                edit_range: Range::new(position, position),
                new_text: " ?".to_owned(),
                code,
                title: if discarded {
                    "Propagate this ask's error and discard its reply"
                } else {
                    "Propagate this ask's error with `?`"
                }
                .to_owned(),
                extra_edits: if discarded {
                    let start = Position::new(sl, sc);
                    vec![(Range::new(start, start), "let _ = ".to_owned())]
                } else {
                    Vec::new()
                },
            });
        }
    }
    out
}

struct AskFinder<'a> {
    typed: &'a TypedWorkspace,
    module: &'a TypedModule,
    nodes: &'a NodeIdMap,
    errors: &'a [(ModuleId, TypeError)],
    scope: Option<Type>,
    boundary: Option<Span>,
    callback_scopes: Vec<(Span, Type)>,
    paren_asks: Vec<(Span, Span)>,
    out: Vec<(Span, Span, &'static str)>,
}

const fn node_kind(e: &Expr) -> NodeKind {
    match e {
        Expr::Block(_) => NodeKind::Block,
        Expr::Try { .. } => NodeKind::Try,
        _ => NodeKind::Expr,
    }
}

fn peel(ty: &Type) -> &Type {
    match ty {
        Type::Alias { body, .. } => peel(body),
        _ => ty,
    }
}

fn ground(ty: &Type) -> bool {
    match peel(ty) {
        Type::Con(_, args) | Type::Tuple(args) => args.iter().all(ground),
        _ => false,
    }
}

impl AskFinder<'_> {
    fn visit_paren(&mut self, span: Span, inner: &Expr) {
        let mut bare = inner;
        let mut wrappers = vec![span];
        while let Expr::Paren { inner, span } = bare {
            wrappers.push(*span);
            bare = inner;
        }
        let saved_len = self.paren_asks.len();
        if matches!(bare, Expr::Ask { .. }) {
            self.paren_asks
                .extend(wrappers.into_iter().map(|span| (bare.span(), span)));
        }
        self.visit_expr(bare);
        self.paren_asks.truncate(saved_len);
    }

    fn node_type(&self, e: &Expr) -> Option<&Type> {
        let id = self.nodes.get(e.span(), node_kind(e))?;
        self.module.node_types.get(id.0 as usize)?.as_ref()
    }

    fn result_error<'a>(&self, ty: &'a Type) -> Option<&'a Type> {
        match peel(ty) {
            Type::Con(id, args) if *id == self.typed.builtins.result => args.get(1),
            _ => None,
        }
    }

    fn accepts_error(&self, source: &Type, target: &Type) -> bool {
        if type_to_key(source) == type_to_key(target) {
            return true;
        }
        if !ground(source) || !ground(target) {
            return false;
        }
        let mut ctx = InferCtx::new(&self.typed.builtins);
        ctx.tycon_decls.clone_from(&self.typed.tycons);
        let source_var = ctx.fresh_tyvid();
        let target_var = ctx.fresh_tyvid();
        if ridge_typecheck::unify::unify(&mut ctx, &Type::Var(source_var), source).is_err()
            || ridge_typecheck::unify::unify(&mut ctx, &Type::Var(target_var), target).is_err()
        {
            return false;
        }
        let constraint = Constraint::new(
            ERROR_FROM_CLASS,
            [source_var, target_var].into_iter().collect(),
        );
        ctx.deferred_constraints.push(constraint.clone());
        let (retained, plans) = ridge_typecheck::solve::solve_constraints(
            &mut ctx,
            &self.typed.instance_env,
            &self.typed.class_table,
            &HashSet::default(),
            Span::point(0),
            Some(&self.typed.builtins),
        );
        ctx.errors.is_empty() && retained.is_empty() && plans.contains_key(&constraint)
    }

    fn consider(&mut self, e: &Expr) {
        let (Some(scope), Some(ask_type)) = (&self.scope, self.node_type(e)) else {
            return;
        };
        let (Some(target), Some(source)) = (self.result_error(scope), self.result_error(ask_type))
        else {
            return;
        };
        if !self.accepts_error(source, target) {
            return;
        }
        let Type::Con(_, args) = peel(ask_type) else {
            return;
        };
        let Some(reply) = args.first() else { return };
        for (mid, err) in self.errors {
            if *mid != self.module.id {
                continue;
            }
            let (span, matching) = match err {
                TypeError::TypeMismatch {
                    expected: TypeDesc::Ty(expected),
                    found: TypeDesc::Ty(found),
                    span,
                    ..
                }
                | TypeError::TypeMismatchInCall {
                    expected: TypeDesc::Ty(expected),
                    found: TypeDesc::Ty(found),
                    span,
                    ..
                } => {
                    let matching = (type_to_key(expected) == type_to_key(reply)
                        && type_to_key(found) == type_to_key(ask_type))
                        || (type_to_key(found) == type_to_key(reply)
                            && type_to_key(expected) == type_to_key(ask_type));
                    (*span, matching)
                }
                TypeError::DiscardedResult { span, .. } => (
                    *span,
                    *span == e.span() || self.paren_asks.contains(&(e.span(), *span)),
                ),
                _ => continue,
            };
            let outside_boundary = self
                .boundary
                .is_some_and(|boundary| span.start <= boundary.start && boundary.end <= span.end);
            if matching
                && !outside_boundary
                && span.start <= e.span().start
                && e.span().end <= span.end
            {
                self.out.push((e.span(), span, err.code()));
                break;
            }
        }
    }
}

impl<'ast> Visit<'ast> for AskFinder<'_> {
    fn visit_on_handler(&mut self, handler: &'ast ridge_ast::OnHandler) {
        // A handler's Result annotation does not establish the compiler's
        // function-return context. Do not offer an edit that would emit T058.
        let saved = self.scope.take();
        ridge_ast::visit::walk_on_handler(self, handler);
        self.scope = saved;
    }

    fn visit_fn_decl(&mut self, f: &'ast FnDecl) {
        let scope = match &f.body {
            Body::Expr(e) => self
                .nodes
                .get(e.span(), node_kind(e))
                .and_then(|id| self.module.schemes.get(&id))
                .and_then(|scheme| match peel(&scheme.ty) {
                    Type::Fn { ret, .. } => Some((**ret).clone()),
                    _ => None,
                }),
            _ => None,
        };
        let saved = std::mem::replace(&mut self.scope, scope);
        ridge_ast::visit::walk_fn_decl(self, f);
        self.scope = saved;
    }

    fn visit_expr(&mut self, e: &'ast Expr) {
        let scope = match e {
            Expr::Paren { inner, span } => {
                self.visit_paren(*span, inner);
                return;
            }
            Expr::Call { callee, args, .. } => {
                let saved_len = self.callback_scopes.len();
                if let Some(Type::Fn { params, .. }) = self.node_type(callee).map(peel) {
                    let scopes: Vec<_> = args
                        .iter()
                        .zip(params)
                        .filter_map(|(arg, param)| {
                            let mut arg = arg;
                            while let Expr::Paren { inner, .. } = arg {
                                arg = inner;
                            }
                            let Type::Fn { ret, .. } = peel(param) else {
                                return None;
                            };
                            matches!(arg, Expr::Lambda { .. })
                                .then(|| (arg.span(), (**ret).clone()))
                        })
                        .collect();
                    self.callback_scopes.extend(scopes);
                }
                ridge_ast::visit::walk_expr(self, e);
                self.callback_scopes.truncate(saved_len);
                return;
            }
            Expr::Lambda { .. } => self
                .callback_scopes
                .iter()
                .rev()
                .find(|(span, _)| *span == e.span())
                .map(|(_, ty)| ty.clone())
                .or_else(|| {
                    self.node_type(e).and_then(|ty| match peel(ty) {
                        Type::Fn { ret, .. } => Some((**ret).clone()),
                        _ => None,
                    })
                }),
            Expr::Try { .. } => {
                let inferred = self.node_type(e).cloned();
                if inferred
                    .as_ref()
                    .and_then(|ty| self.result_error(ty))
                    .is_some_and(|ty| matches!(ty, Type::Var(_)))
                {
                    self.scope
                        .clone()
                        .filter(|ty| self.result_error(ty).is_some())
                        .or(inferred)
                } else {
                    inferred
                }
            }
            _ => {
                if matches!(e, Expr::Ask { .. }) {
                    self.consider(e);
                }
                ridge_ast::visit::walk_expr(self, e);
                return;
            }
        };
        let saved = std::mem::replace(&mut self.scope, scope);
        let saved_boundary = self.boundary.replace(e.span());
        ridge_ast::visit::walk_expr(self, e);
        self.scope = saved;
        self.boundary = saved_boundary;
    }
}
