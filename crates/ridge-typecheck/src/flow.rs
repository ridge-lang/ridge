//! Reachability of an expression's continuation, independent of its value type.

use ridge_ast::{visit::Visit, BinOp, Block, Expr, Span};

use crate::ctx::InferCtx;

pub type CacheKey = (Span, std::mem::Discriminant<Expr>, bool);

/// Inspect continuation before a result expectation can constrain generic slots.
/// This never infers a type or enters a latent function body. Cache only within
/// this module's inference context; edits receive a fresh context.
pub fn expression_flow(ctx: &mut InferCtx, expr: &Expr) -> Flow {
    let key = (
        expr.span(),
        std::mem::discriminant(expr),
        ctx.current_fn_ret.is_some(),
    );
    if let Some(flow) = ctx.value_flow_cache.get(&key) {
        return *flow;
    }
    let flow = match expr {
        Expr::Lambda { .. } | Expr::InnerFn { .. } => Flow::NEXT,
        Expr::Return { value, .. } => {
            let exit = if ctx.current_fn_ret.is_some() {
                Flow::RETURN
            } else {
                Flow::NEXT
            };
            expression_flow(ctx, value).then(exit)
        }
        Expr::Propagate { inner, .. } => expression_flow(ctx, inner).then(Flow::PROPAGATE),
        Expr::Try { block, .. } => block_flow(ctx, block).capture(),
        Expr::Block(block) => block_flow(ctx, block),
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            let cond = expression_flow(ctx, cond);
            let yes = expression_flow(ctx, then_branch);
            let no = else_branch
                .as_ref()
                .map_or(Flow::NEXT, |other| expression_flow(ctx, other));
            cond.then(yes.either(no))
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            let input = expression_flow(ctx, scrutinee);
            let branches = arms.iter().fold(Flow::RETURN, |branches, arm| {
                let guard = arm
                    .guard
                    .as_ref()
                    .map_or(Flow::NEXT, |guard| expression_flow(ctx, guard));
                branches.either(guard.then(expression_flow(ctx, &arm.body)))
            });
            input.then(branches)
        }
        Expr::Guard {
            cond, else_branch, ..
        } => expression_flow(ctx, cond).then(Flow::NEXT.either(block_flow(ctx, else_branch))),
        Expr::Binary {
            op: BinOp::And | BinOp::Or,
            lhs,
            rhs,
            ..
        } => expression_flow(ctx, lhs).then(expression_flow(ctx, rhs).either(Flow::NEXT)),
        Expr::Pipe { lhs, rhs, .. } => pipe_flow(ctx, lhs, rhs),
        _ => {
            struct Children<'a> {
                ctx: &'a mut InferCtx,
                flow: Flow,
            }
            impl<'ast> Visit<'ast> for Children<'_> {
                fn visit_expr(&mut self, expr: &'ast Expr) {
                    self.flow = self.flow.then(expression_flow(self.ctx, expr));
                }
            }
            let mut children = Children {
                ctx,
                flow: Flow::NEXT,
            };
            ridge_ast::visit::walk_expr(&mut children, expr);
            children.flow
        }
    };
    ctx.value_flow_cache.insert(key, flow);
    flow
}

fn block_flow(ctx: &mut InferCtx, block: &Block) -> Flow {
    block.stmts.iter().fold(Flow::NEXT, |flow, stmt| {
        flow.then(expression_flow(ctx, stmt))
    })
}

/// Match the data-last call and `?` rewrite in `infer_pipe`.
fn pipe_flow(ctx: &mut InferCtx, lhs: &Expr, rhs: &Expr) -> Flow {
    match rhs {
        Expr::Propagate { inner, .. } => pipe_flow(ctx, lhs, inner).then(Flow::PROPAGATE),
        Expr::Call { callee, args, .. } => {
            let callee = expression_flow(ctx, callee);
            args.iter()
                .fold(callee, |flow, arg| flow.then(expression_flow(ctx, arg)))
                .then(expression_flow(ctx, lhs))
        }
        _ => expression_flow(ctx, rhs).then(expression_flow(ctx, lhs)),
    }
}

/// Outcomes needed when composing expressions in the current return boundary.
/// Explicit returns stop the continuation; propagation can also exit a `try`.
#[derive(Clone, Copy, Debug)]
pub struct Flow {
    /// At least one path produces a value for the following expression.
    pub(crate) falls_through: bool,
    /// At least one path leaves through `?`, caught by an enclosing `try`.
    propagates: bool,
}

impl Default for Flow {
    fn default() -> Self {
        Self::NEXT
    }
}

impl Flow {
    pub(crate) const NEXT: Self = Self {
        falls_through: true,
        propagates: false,
    };
    pub(crate) const RETURN: Self = Self {
        falls_through: false,
        propagates: false,
    };
    pub(crate) const PROPAGATE: Self = Self {
        falls_through: true,
        propagates: true,
    };

    /// Evaluate the right region only along paths that reach it.
    pub(crate) const fn then(self, next: Self) -> Self {
        Self {
            falls_through: self.falls_through && next.falls_through,
            propagates: self.propagates || (self.falls_through && next.propagates),
        }
    }

    /// Either branch may be selected.
    pub(crate) const fn either(self, other: Self) -> Self {
        Self {
            falls_through: self.falls_through || other.falls_through,
            propagates: self.propagates || other.propagates,
        }
    }

    /// A propagation failure becomes a result of this `try`, not a return.
    pub(crate) const fn capture(self) -> Self {
        Self {
            falls_through: self.falls_through || self.propagates,
            propagates: false,
        }
    }
}
