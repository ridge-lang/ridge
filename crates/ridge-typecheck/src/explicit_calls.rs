//! Semantic message inventory shared by explicit-call migration and diagnostics.

use ridge_ast::{visit::Visit, Expr, Ident, Module, Span};
use ridge_resolve::NodeKind;
use ridge_types::{BuiltinTyCons, TyConArena, TyConKind, Type};
use rustc_hash::FxHashSet;

use crate::{actor, ctx::InferCtx};

fn unalias(mut ty: &Type) -> &Type {
    while let Type::Alias { body, .. } = ty {
        ty = body;
    }
    ty
}

/// A function value used where its concrete return type is required.
///
/// This is guidance only: expected types never invoke a function implicitly.
#[must_use]
pub fn missing_function_call(error: &crate::TypeError) -> Option<(&Type, Span)> {
    use crate::{error::TypeDesc, TypeError};
    let (TypeDesc::Ty(left), TypeDesc::Ty(right), span) = (match error {
        TypeError::TypeMismatch {
            expected,
            found,
            span,
            ..
        }
        | TypeError::TypeMismatchInCall {
            expected,
            found,
            span,
            ..
        } => (expected, found, *span),
        _ => return None,
    }) else {
        return None;
    };
    for (function, value) in [
        (left.as_ref(), right.as_ref()),
        (right.as_ref(), left.as_ref()),
    ] {
        let function = unalias(function);
        let value = unalias(value);
        if let Type::Fn { params, ret, .. } = function {
            // Open return types do not establish that an invocation fixes the error.
            if params.is_empty()
                && matches!(value, Type::Con(_, _))
                && ridge_types::shape_key::type_to_key(unalias(ret))
                    == ridge_types::shape_key::type_to_key(value)
            {
                return Some((function, span));
            }
        }
    }
    None
}

/// One source message operation and its selected handler's parameter count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageCallSite {
    /// Entire operation; independent of its source spelling.
    pub operation: Span,
    /// Handler identifier, excluding surrounding parentheses and comments.
    pub message: Span,
    /// Handler name as written in the source.
    pub handler: String,
    /// Number of actual parameters; unknown receivers remain unresolved.
    pub parameter_count: Option<usize>,
    /// The receiver still has a free type variable after inference.
    pub needs_receiver_annotation: bool,
    /// Number of syntactic message arguments, including a literal `()` marker.
    pub argument_count: usize,
    /// Function-form sends/asks require the whole message to remain one argument.
    pub parenthesize: bool,
}

impl MessageCallSite {
    /// A replacement exists only for a resolved, bare, zero-parameter message.
    #[must_use]
    pub fn replacement(&self) -> Option<String> {
        if self.parameter_count != Some(0) || self.argument_count != 0 {
            return None;
        }
        Some(if self.parenthesize {
            format!("({} ())", self.handler)
        } else {
            format!("{} ()", self.handler)
        })
    }
}

pub(crate) fn collect(
    ctx: &InferCtx,
    builtins: &BuiltinTyCons,
    arena: &TyConArena,
    ast: &Module,
    node_types: &[Option<Type>],
) -> (Vec<MessageCallSite>, FxHashSet<Span>) {
    let mut finder = Finder {
        ctx,
        builtins,
        arena,
        node_types,
        sites: Vec::new(),
        nullary_markers: FxHashSet::default(),
    };
    finder.visit_module(ast);
    // Export abstracts signature rigids into variables. An existing receiver
    // error is authoritative: that variable does not need another annotation.
    let invalid_receivers: rustc_hash::FxHashSet<_> = ctx
        .errors
        .iter()
        .filter_map(|error| match error {
            crate::TypeError::SendOnNonActor { span, .. }
            | crate::TypeError::AskOnNonActor { span, .. } => Some(*span),
            _ => None,
        })
        .collect();
    for site in &mut finder.sites {
        site.needs_receiver_annotation &= !invalid_receivers.contains(&site.operation);
    }
    (finder.sites, finder.nullary_markers)
}

struct Finder<'a> {
    ctx: &'a InferCtx,
    builtins: &'a BuiltinTyCons,
    arena: &'a TyConArena,
    node_types: &'a [Option<Type>],
    sites: Vec<MessageCallSite>,
    nullary_markers: FxHashSet<Span>,
}

impl Finder<'_> {
    fn receiver_type(&self, handle: &Expr) -> Option<&Type> {
        let kind = match handle {
            Expr::Block(_) => NodeKind::Block,
            Expr::Try { .. } => NodeKind::Try,
            _ => NodeKind::Expr,
        };
        let node = self.ctx.node_id_map.as_ref()?.get(handle.span(), kind)?;
        self.node_types.get(node.0 as usize)?.as_ref().map(unalias)
    }

    fn arity(&self, operation: Span, handle: &Expr, name: &str) -> Option<usize> {
        if let Some(&count) = self.ctx.message_parameter_counts.get(&operation) {
            return Some(count);
        }
        let ty = self.receiver_type(handle)?;
        let Type::Con(head, args) = ty else {
            return None;
        };
        let actor = if *head == self.builtins.handle {
            let Type::Con(actor, _) = unalias(args.first()?) else {
                return None;
            };
            *actor
        } else {
            *head
        };
        let TyConKind::Actor(schema) = &self.arena.all().get(actor.0 as usize)?.kind else {
            return None;
        };
        schema
            .handlers
            .iter()
            .find(|h| h.name == name)
            .map(|h| h.params.len())
    }

    fn record(
        &mut self,
        operation: Span,
        handle: &Expr,
        message: &Ident,
        args: &[Expr],
        parenthesize: bool,
    ) {
        let parameter_count = self.arity(operation, handle, &message.text);
        if parameter_count == Some(0) && matches!(args, [Expr::Unit(_)]) {
            self.nullary_markers.insert(operation);
        }
        self.sites.push(MessageCallSite {
            operation,
            message: message.span,
            handler: message.text.clone(),
            parameter_count,
            needs_receiver_annotation: parameter_count.is_none()
                && matches!(self.receiver_type(handle), Some(Type::Var(_))),
            argument_count: args.len(),
            parenthesize,
        });
    }
}

fn message_parts(message: &Expr) -> Option<(&Ident, &[Expr])> {
    match crate::infer::peel_parens(message) {
        Expr::Ident(name) => Some((name, &[])),
        Expr::Call { callee, args, .. } => match callee.as_ref() {
            Expr::Ident(name) => Some((name, args)),
            _ => None,
        },
        _ => None,
    }
}

impl<'ast> Visit<'ast> for Finder<'_> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            Expr::Send {
                handle,
                message,
                span,
            } => {
                if let Some((name, args)) = message_parts(message) {
                    self.record(*span, handle, name, args, false);
                }
            }
            Expr::Ask {
                handle,
                message,
                args,
                span,
                ..
            } => {
                self.record(*span, handle, message, args, false);
            }
            Expr::Call { callee, args, span }
                if (actor::is_actor_send_callee(self.ctx, callee) && args.len() == 2)
                    || (actor::is_tryask_callee(self.ctx, callee) && args.len() == 3) =>
            {
                if let Some((name, message_args)) = message_parts(&args[1]) {
                    self.record(*span, &args[0], name, message_args, true);
                }
            }
            _ => {}
        }
        ridge_ast::visit::walk_expr(self, expr);
    }
}
