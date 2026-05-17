//! Capability inference walker (T13).
//!
//! # Design choice: two-pass (Option A)
//!
//! T6 runs type inference first — all unification (including cap-var binding from
//! HOF call sites) is complete when this module runs.  T13 is then a separate,
//! read-only walker over the AST that computes capability contributions using the
//! §8.1 inference rules table.
//!
//! For HOFs (§8.5, D041), T13 performs its own lightweight instantiation of the
//! callee's scheme and unifies the cap-var with the concrete capability set
//! produced by each callback argument.  This is independent of T6's unification
//! table; it uses fresh `CapVids` only for the cap-resolution bookkeeping of this
//! single call site.
//!
//! # Lambda capability isolation (§8.1 rule)
//!
//! A `Lambda` expression as a *value* contributes PURE to the enclosing decl's
//! capability set.  The lambda's *body* caps are stored in the lambda's
//! `Type::Fn { caps }` slot (populated by this pass, hybrid-style: when
//! `infer_caps` visits a Lambda it recurses into the body and records the result
//! in the returned caps, which callers then read back from the env if they call
//! the lambda).
//!
//! # Side-table (D079)
//!
//! Callers accumulate results in `InferCtx::current_caps` (already present from
//! T6 scaffold) or directly return `CapabilitySet` values.  The `TypedModule`
//! `inferred_caps: FxHashMap<NodeId, CapabilitySet>` field (defined in `lib.rs`)
//! is the long-term home; T13 currently populates `current_caps` and returns
//! values from the walker functions.  Full NodeId-keyed population is T14/T17.

use ridge_ast::{Block, Capability, Expr};
use ridge_types::{BuiltinTyCons, CapRow, CapabilitySet, Scheme, Type};

use crate::ctx::InferCtx;
use crate::instantiate::instantiate;
use crate::unify::unify_caps;

// ── Public API ────────────────────────────────────────────────────────────────

/// Walk `expr` and return the union of all capability contributions per §8.1.
///
/// This is a read-only pass that runs AFTER type inference (T6) is complete.
/// All `CapRow::Var(v)` in stdlib HOF types are already resolved in `ctx.capvids`
/// from T6's unify calls.
pub fn infer_caps(ctx: &mut InferCtx, b: &BuiltinTyCons, expr: &Expr) -> CapabilitySet {
    match expr {
        // ── Pure leaves ───────────────────────────────────────────────────────
        // Literals, unit, bare identifiers, Send, FieldAccessorFn, and InnerFn
        // carry no capability contribution by themselves.
        Expr::Literal(_)
        | Expr::Unit(_)
        | Expr::Ident(_)
        | Expr::Send { .. }
        | Expr::FieldAccessorFn { .. }
        | Expr::InnerFn { .. } => CapabilitySet::PURE,

        // ── Qualified name ────────────────────────────────────────────────────
        // A qualified name used as a value (not in a call position) contributes
        // its declared caps.  This handles `let f = Io.println` patterns where
        // `f` is used as a first-class function value.
        Expr::Qualified(_) => {
            // Read the caps of the qualified name from the environment or return
            // PURE if not found (unknown names were already an error in T6).
            caps_of_expr(ctx, b, expr)
        }

        // ── Lambda ────────────────────────────────────────────────────────────
        // §8.1: "Lambda { body } → none from the lambda value itself".
        // The lambda body's caps are inferred here (recursively) but do NOT
        // propagate to the enclosing decl.  They are available via
        // `caps_of_expr(lambda_expr)` when the lambda is called.
        Expr::Lambda { body, .. } => {
            // Recurse into the body to compute its caps (so that callers of
            // lambdas that are passed around can see the right caps).  But we
            // return PURE: the lambda *value* itself doesn't propagate caps.
            let _ = infer_caps(ctx, b, body);
            CapabilitySet::PURE
        }

        // ── Call ──────────────────────────────────────────────────────────────
        // §8.1: caps(Call(f, args)) = caps_of(f) ∪ ⋃ caps_of(args[i])
        Expr::Call { callee, args, .. } => {
            let callee_caps = caps_of_callee(ctx, b, callee, args);
            let args_caps = args.iter().fold(CapabilitySet::PURE, |acc, a| {
                acc.union(&infer_caps(ctx, b, a))
            });
            callee_caps.union(&args_caps)
        }

        // ── Pipe `|>` ─────────────────────────────────────────────────────────
        // §8.1: rewritten as Call; lhs is an arg, rhs is the function.
        // Pipe(lhs, rhs) ≡ Call(rhs, [lhs]).
        //
        // In the AST, `xs |> f arg` is `Pipe { lhs: xs, rhs: Call { f, [arg] } }`.
        // The rhs may itself be a Call node (partially-applied callee).  We handle
        // the rhs by recursing into it (Call handler picks up the callee's caps),
        // and include the lhs caps for completeness (piped-value may be effectful).
        //
        // For HOFs like `xs |> List.forEach Io.println`, rhs = Call(List.forEach,
        // [Io.println]).  `infer_caps(rhs)` goes into the Call arm, which calls
        // `caps_of_callee(List.forEach, [Io.println])` and returns {io}.
        Expr::Pipe { lhs, rhs, .. } => {
            let lhs_caps = infer_caps(ctx, b, lhs);
            let rhs_caps = infer_caps(ctx, b, rhs);
            lhs_caps.union(&rhs_caps)
        }

        // ── Ask ───────────────────────────────────────────────────────────────
        // §8.1 / D018 Model B: Ask absorbs only {time}.
        Expr::Ask { .. } => CapabilitySet::singleton(Capability::Time),

        // ── Spawn ─────────────────────────────────────────────────────────────
        // §8.1 / D061: Spawn absorbs only {spawn}.
        Expr::Spawn { .. } => CapabilitySet::singleton(Capability::Spawn),

        // ── Try block / Block ──────────────────────────────────────────────────
        // §8.1: infer_caps(block).
        Expr::Try { block, .. } | Expr::Block(block) => infer_caps_block(ctx, b, block),

        // ── Guard ─────────────────────────────────────────────────────────────
        // §8.1: infer_caps(cond) ∪ infer_caps(else_branch).
        Expr::Guard {
            cond, else_branch, ..
        } => {
            let cond_caps = infer_caps(ctx, b, cond);
            let else_caps = infer_caps_block(ctx, b, else_branch);
            cond_caps.union(&else_caps)
        }

        // ── If ────────────────────────────────────────────────────────────────
        // Union of condition, then, and else branches.
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            let mut caps = infer_caps(ctx, b, cond);
            caps = caps.union(&infer_caps(ctx, b, then_branch));
            if let Some(else_expr) = else_branch {
                caps = caps.union(&infer_caps(ctx, b, else_expr));
            }
            caps
        }

        // ── Match ─────────────────────────────────────────────────────────────
        // Union of scrutinee and all arm bodies.
        Expr::Match {
            scrutinee, arms, ..
        } => {
            let mut caps = infer_caps(ctx, b, scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    caps = caps.union(&infer_caps(ctx, b, g));
                }
                caps = caps.union(&infer_caps(ctx, b, &arm.body));
            }
            caps
        }

        // ── Let / Var / Return ────────────────────────────────────────────────
        Expr::Let { value, .. } | Expr::Var { value, .. } | Expr::Return { value, .. } => {
            infer_caps(ctx, b, value)
        }

        // ── Assign ───────────────────────────────────────────────────────────
        Expr::Assign { target, value, .. } => {
            infer_caps(ctx, b, target).union(&infer_caps(ctx, b, value))
        }

        // ── Propagate `?` / Paren ─────────────────────────────────────────────
        Expr::Propagate { inner, .. } | Expr::Paren { inner, .. } => infer_caps(ctx, b, inner),

        // ── Binary operators ──────────────────────────────────────────────────
        Expr::Binary { lhs, rhs, .. } => infer_caps(ctx, b, lhs).union(&infer_caps(ctx, b, rhs)),

        // ── Unary operators ───────────────────────────────────────────────────
        Expr::Unary { expr, .. } => infer_caps(ctx, b, expr),

        // ── Tuple / List ──────────────────────────────────────────────────────
        Expr::Tuple { elems, .. } | Expr::List { elems, .. } => {
            elems.iter().fold(CapabilitySet::PURE, |acc, e| {
                acc.union(&infer_caps(ctx, b, e))
            })
        }

        // ── Record / With / FieldAccess ───────────────────────────────────────
        // Walk sub-expressions.
        Expr::Record { fields, .. } => fields.iter().fold(CapabilitySet::PURE, |acc, f| {
            f.value
                .as_ref()
                .map_or(acc, |val| acc.union(&infer_caps(ctx, b, val)))
        }),
        Expr::With { base, fields, .. } => {
            let mut caps = infer_caps(ctx, b, base);
            for f in fields {
                if let Some(ref val) = f.value {
                    caps = caps.union(&infer_caps(ctx, b, val));
                }
            }
            caps
        }
        Expr::FieldAccess { base, .. } => infer_caps(ctx, b, base),

        // ── String interpolation ──────────────────────────────────────────────
        // Walk all interpolation parts that are expressions.
        Expr::Interp { parts, .. } => {
            use ridge_ast::InterpPart;
            parts.iter().fold(CapabilitySet::PURE, |acc, p| match p {
                InterpPart::Expr { expr: e, .. } => acc.union(&infer_caps(ctx, b, e)),
                InterpPart::Text { .. } => acc,
            })
        }
    }
}

/// Walk a `Block` and return the union of all statement caps.
pub fn infer_caps_block(ctx: &mut InferCtx, b: &BuiltinTyCons, block: &Block) -> CapabilitySet {
    block.stmts.iter().fold(CapabilitySet::PURE, |acc, s| {
        acc.union(&infer_caps(ctx, b, s))
    })
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Return the concrete capability set that `callee` contributes at a call site,
/// taking into account the supplied `args` for HOF cap-var resolution (§8.5).
///
/// For stdlib HOFs with `CapRow::Var(c)` on the outer Fn type, this function:
/// 1. Instantiates the callee's scheme with fresh `CapVids`.
/// 2. Computes the callback argument's concrete caps via `caps_of_expr`.
/// 3. Unifies the fresh cap var with the callback's concrete caps.
/// 4. Returns the resolved concrete cap set.
///
/// For regular (non-HOF) callees this simply returns the callee's concrete caps.
fn caps_of_callee(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    callee: &Expr,
    args: &[Expr],
) -> CapabilitySet {
    // Obtain the scheme (if available) to detect cap vars.
    let scheme_opt = scheme_of_callee(ctx, callee);

    match scheme_opt {
        None => {
            // No scheme — fall back to reading direct caps from the expr type.
            caps_of_expr(ctx, b, callee)
        }
        Some(scheme) => {
            if scheme.cap_vars.is_empty() {
                // Non-HOF: read concrete caps directly from the scheme's Fn type.
                caps_from_scheme_fn(&scheme)
            } else {
                // HOF: instantiate with fresh CapVids, resolve against args.
                resolve_hof_caps(ctx, b, &scheme, args)
            }
        }
    }
}

/// Look up the `Scheme` for `callee` from the environment.
///
/// Returns `None` if the callee is not a directly-resolvable name (e.g. a
/// complex expression whose type must be read from the type table).
fn scheme_of_callee(ctx: &InferCtx, callee: &Expr) -> Option<Scheme> {
    match callee {
        Expr::Ident(id) => {
            // Look up in local env (includes stdlib-bound names in tests).
            ctx.env.lookup(&id.text).cloned()
        }
        Expr::Qualified(q) => {
            // Qualified names like "Io.println" or "List.forEach" are stored
            // in the env as "Io.println" / "List.forEach" in the test helpers,
            // or via the full segments joined with ".".
            let full = q
                .segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(".");
            ctx.env.lookup(&full).cloned()
        }
        _ => None,
    }
}

/// Extract the concrete capability set from a scheme whose top-level type is
/// `Type::Fn { caps: CapRow::Concrete(cs) }`.
///
/// Returns PURE for non-function schemes or unresolved cap rows (which means
/// the caps have already been accounted for elsewhere or the fn is pure).
const fn caps_from_scheme_fn(scheme: &Scheme) -> CapabilitySet {
    if let Type::Fn {
        caps: CapRow::Concrete(cs),
        ..
    } = &scheme.ty
    {
        *cs
    } else {
        CapabilitySet::PURE
    }
}

/// Read the capability set that `expr` *currently holds* as a function type.
///
/// For identifiers, look up the env scheme and extract its caps slot.
/// For qualified names, same lookup via dot-joined segments.
/// Falls back to PURE for all other expressions.
fn caps_of_expr(ctx: &InferCtx, _b: &BuiltinTyCons, expr: &Expr) -> CapabilitySet {
    match expr {
        Expr::Ident(id) => ctx
            .env
            .lookup(&id.text)
            .cloned()
            .map_or(CapabilitySet::PURE, |s| caps_from_scheme_fn(&s)),
        Expr::Qualified(q) => {
            let full = q
                .segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(".");
            ctx.env
                .lookup(&full)
                .cloned()
                .map_or(CapabilitySet::PURE, |s| caps_from_scheme_fn(&s))
        }
        // Lambda as value or any other expression: pure.
        _ => CapabilitySet::PURE,
    }
}

/// Resolve capability set for a HOF call site (§8.5 D041).
///
/// Given a HOF scheme with `cap_vars` (e.g. `List.forEach`):
/// 1. Instantiate with fresh `CapVids`.
/// 2. Find the callback parameter (the first `Fn`-typed param with a cap var).
/// 3. Compute the concrete caps of the supplied callback arg.
/// 4. Unify the fresh cap var with the concrete callback caps.
/// 5. Resolve the outer Fn's caps row to a concrete set and return it.
fn resolve_hof_caps(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    scheme: &Scheme,
    args: &[Expr],
) -> CapabilitySet {
    // Instantiate the HOF scheme: replaces all TyVids and CapVids with fresh ones.
    let instantiated = instantiate(ctx, scheme);

    // The instantiated type must be a Fn.
    let (params, outer_caps_row) = match &instantiated {
        Type::Fn { params, caps, .. } => (params.clone(), caps.clone()),
        _ => return CapabilitySet::PURE,
    };

    // For each Fn-typed parameter (callback), find the matching arg and
    // unify the cap var with the arg's concrete caps.
    for (i, param_ty) in params.iter().enumerate() {
        if let Type::Fn {
            caps: CapRow::Var(cv),
            ..
        } = param_ty
        {
            let cv = *cv;
            // Get the caps of the corresponding argument (if supplied).
            let arg_caps = if i < args.len() {
                caps_of_expr(ctx, b, &args[i])
            } else {
                CapabilitySet::PURE
            };
            // Bind the fresh cap var to the arg's concrete caps.
            let _ = unify_caps(ctx, &CapRow::Var(cv), &CapRow::Concrete(arg_caps));
        }
    }

    // Now resolve the outer Fn's caps row — it may be a Var that got bound above,
    // or it may be Concrete already.
    match ctx.shallow_resolve_caps(&outer_caps_row) {
        CapRow::Concrete(cs) => cs,
        // Still unbound (no callback arg supplied) or non-exhaustive variant — treat as pure.
        _ => CapabilitySet::PURE,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{
        Block, Expr, Ident, LambdaParam, Literal, MatchArm, Pattern, QualifiedName, Span,
    };
    use ridge_types::{BuiltinTyCons, CapRow, Scheme, TyConArena, Type};

    fn ds() -> Span {
        Span::point(0)
    }

    fn id(t: &str) -> Ident {
        Ident {
            text: t.to_string(),
            span: ds(),
        }
    }

    fn make_builtins() -> (TyConArena, BuiltinTyCons) {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        (arena, b)
    }

    /// Bind a stdlib-like symbol in the env as a mono scheme with the given type.
    fn bind_mono(ctx: &mut InferCtx, name: &str, ty: Type) {
        ctx.env.bind(name.to_string(), Scheme::mono(ty));
    }

    /// Bind a polymorphic stdlib symbol with cap vars.
    fn bind_poly_cap(ctx: &mut InferCtx, name: &str, scheme: Scheme) {
        ctx.env.bind(name.to_string(), scheme);
    }

    /// Build a block of statements.
    fn block(stmts: Vec<Expr>) -> Block {
        Block { stmts, span: ds() }
    }

    /// A literal integer expression.
    fn int_lit(n: i64) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.to_string(),
            span: ds(),
        })
    }

    /// A text literal expression.
    fn text_lit(s: &str) -> Expr {
        Expr::Literal(Literal::Text {
            raw: format!("\"{s}\""),
            span: ds(),
        })
    }

    // ── Helpers for stdlib Scheme construction ────────────────────────────────

    /// Build `fn {io} Text -> Unit` (Io.println signature).
    fn io_println_scheme(b: &BuiltinTyCons) -> Scheme {
        let io_caps = CapabilitySet::singleton(Capability::Io);
        Scheme::mono(Type::Fn {
            params: vec![Type::Con(b.text, vec![])],
            ret: Box::new(Type::Con(b.unit, vec![])),
            caps: CapRow::Concrete(io_caps),
        })
    }

    /// Build `fn {fs} Text -> Result Text Text` (Fs.readFile signature).
    fn fs_readfile_scheme(b: &BuiltinTyCons) -> Scheme {
        let fs_caps = CapabilitySet::singleton(Capability::Fs);
        Scheme::mono(Type::Fn {
            params: vec![Type::Con(b.text, vec![])],
            ret: Box::new(Type::Con(
                b.result,
                vec![Type::Con(b.text, vec![]), Type::Con(b.text, vec![])],
            )),
            caps: CapRow::Concrete(fs_caps),
        })
    }

    /// Build `forall a c. (fn c (a -> Unit)) -> List a -> Unit` (List.forEach).
    fn list_foreach_scheme(b: &BuiltinTyCons) -> Scheme {
        use ridge_types::{CapVid, TyVid};
        let a = TyVid(0);
        let cap_c = CapVid(0);
        Scheme {
            vars: vec![a],
            cap_vars: vec![cap_c],
            ty: Type::Fn {
                params: vec![
                    // callback: fn c (a -> Unit)
                    Type::Fn {
                        params: vec![Type::Var(a)],
                        ret: Box::new(Type::Con(b.unit, vec![])),
                        caps: CapRow::Var(cap_c),
                    },
                    // list: List a
                    Type::Con(b.list, vec![Type::Var(a)]),
                ],
                ret: Box::new(Type::Con(b.unit, vec![])),
                caps: CapRow::Var(cap_c), // outer fn inherits callback caps
            },
        }
    }

    /// Build `forall a. List a -> Int` (List.length — pure).
    fn list_length_scheme(b: &BuiltinTyCons) -> Scheme {
        use ridge_types::TyVid;
        let a = TyVid(0);
        Scheme {
            vars: vec![a],
            cap_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Con(b.list, vec![Type::Var(a)])],
                ret: Box::new(Type::Con(b.int, vec![])),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
        }
    }

    // ── T13-1: infer_pure_fn ──────────────────────────────────────────────────

    #[test]
    fn infer_pure_fn() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // body: let x = 5; x
        let body = Expr::Block(block(vec![
            Expr::Let {
                pat: Pattern::Var {
                    name: id("x"),
                    span: ds(),
                },
                ty: None,
                value: Box::new(int_lit(5)),
                span: ds(),
            },
            Expr::Ident(id("x")),
        ]));

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.is_pure(),
            "pure body must have no capabilities, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-2: infer_io_println ───────────────────────────────────────────────

    #[test]
    fn infer_io_println() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Bind Io.println in env.
        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b).ty);

        // body: Io.println "hi"
        let body = Expr::Call {
            callee: Box::new(Expr::Qualified(QualifiedName {
                segments: vec![id("Io"), id("println")],
                span: ds(),
            })),
            args: vec![text_lit("hi")],
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Io),
            "Io.println must yield {{io}}, got {caps:?}"
        );
        assert!(
            !caps.contains(Capability::Fs),
            "Io.println must not yield {{fs}}, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-3: infer_fs_readfile ──────────────────────────────────────────────

    #[test]
    fn infer_fs_readfile() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        bind_mono(&mut ctx, "Fs.readFile", fs_readfile_scheme(&b).ty);

        // body: Fs.readFile "a"
        let body = Expr::Call {
            callee: Box::new(Expr::Qualified(QualifiedName {
                segments: vec![id("Fs"), id("readFile")],
                span: ds(),
            })),
            args: vec![text_lit("a")],
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Fs),
            "Fs.readFile must yield {{fs}}, got {caps:?}"
        );
        assert!(
            !caps.contains(Capability::Io),
            "Fs.readFile must not yield {{io}}, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-4: infer_union_two_caps ───────────────────────────────────────────

    #[test]
    fn infer_union_two_caps() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b).ty);
        bind_mono(&mut ctx, "Fs.readFile", fs_readfile_scheme(&b).ty);

        // body: let _ = Io.println "x"; Fs.readFile "y"
        let body = Expr::Block(block(vec![
            Expr::Let {
                pat: Pattern::Wildcard { span: ds() },
                ty: None,
                value: Box::new(Expr::Call {
                    callee: Box::new(Expr::Qualified(QualifiedName {
                        segments: vec![id("Io"), id("println")],
                        span: ds(),
                    })),
                    args: vec![text_lit("x")],
                    span: ds(),
                }),
                span: ds(),
            },
            Expr::Call {
                callee: Box::new(Expr::Qualified(QualifiedName {
                    segments: vec![id("Fs"), id("readFile")],
                    span: ds(),
                })),
                args: vec![text_lit("y")],
                span: ds(),
            },
        ]));

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Io) && caps.contains(Capability::Fs),
            "union of io+fs calls must have both, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-5: infer_send_no_cap ──────────────────────────────────────────────

    #[test]
    fn infer_send_no_cap() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // body: actor ! msg  (Send)
        let body = Expr::Send {
            handle: Box::new(Expr::Ident(id("actor"))),
            message: Box::new(Expr::Ident(id("msg"))),
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.is_pure(),
            "Send must yield no capabilities, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-6: infer_ask_time_cap ─────────────────────────────────────────────

    #[test]
    fn infer_ask_time_cap() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // body: actor ?> handler  (Ask)
        let body = Expr::Ask {
            handle: Box::new(Expr::Ident(id("actor"))),
            message: id("handler"),
            args: vec![],
            timeout: None,
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Time),
            "Ask must yield {{time}}, got {caps:?}"
        );
        assert!(
            caps.len() == 1,
            "Ask must yield only {{time}}, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-7: infer_spawn_cap ────────────────────────────────────────────────

    #[test]
    fn infer_spawn_cap() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // body: spawn MyActor  (Spawn)
        let body = Expr::Spawn {
            actor: id("MyActor"),
            args: vec![],
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Spawn),
            "Spawn must yield {{spawn}}, got {caps:?}"
        );
        assert!(
            caps.len() == 1,
            "Spawn must yield only {{spawn}}, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-8: infer_lambda_cap_isolation ────────────────────────────────────

    #[test]
    fn infer_lambda_cap_isolation() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b).ty);

        // body: let f = fn x -> Io.println x; 1
        // The lambda value should not propagate {io} into the outer caps.
        let lambda = Expr::Lambda {
            params: vec![LambdaParam::Pattern(Pattern::Var {
                name: id("x"),
                span: ds(),
            })],
            body: Box::new(Expr::Call {
                callee: Box::new(Expr::Qualified(QualifiedName {
                    segments: vec![id("Io"), id("println")],
                    span: ds(),
                })),
                args: vec![Expr::Ident(id("x"))],
                span: ds(),
            }),
            span: ds(),
        };

        let body = Expr::Block(block(vec![
            Expr::Let {
                pat: Pattern::Var {
                    name: id("f"),
                    span: ds(),
                },
                ty: None,
                value: Box::new(lambda),
                span: ds(),
            },
            int_lit(1),
        ]));

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.is_pure(),
            "lambda value isolation: outer fn must be pure, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-9: infer_lambda_called_propagates ─────────────────────────────────

    #[test]
    fn infer_lambda_called_propagates() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b).ty);

        // Bind f in env with {io} capability (simulating: f was inferred as io fn).
        let f_ty = io_println_scheme(&b).ty; // fn {io} Text -> Unit
        bind_mono(&mut ctx, "f", f_ty);

        // body: f "hello"  — calling f propagates {io}
        let body = Expr::Call {
            callee: Box::new(Expr::Ident(id("f"))),
            args: vec![text_lit("hello")],
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Io),
            "calling an io fn must propagate {{io}}, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-10: infer_hof_capability_polymorphism ─────────────────────────────
    // DoD keystone: `[1,2,3] |> List.forEach Io.println` → caps = {io}

    #[test]
    fn infer_hof_capability_polymorphism() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Bind stdlib symbols in env.
        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b).ty);
        bind_poly_cap(&mut ctx, "List.forEach", list_foreach_scheme(&b));

        // Build: [1, 2, 3] |> List.forEach Io.println
        //
        // In Ridge AST this is:
        //   Pipe {
        //     lhs: List [1, 2, 3],
        //     rhs: Call { callee: List.forEach,
        //                 args: [Io.println] }
        //   }
        let list_expr = Expr::List {
            elems: vec![int_lit(1), int_lit(2), int_lit(3)],
            span: ds(),
        };
        let foreach_call = Expr::Call {
            callee: Box::new(Expr::Qualified(QualifiedName {
                segments: vec![id("List"), id("forEach")],
                span: ds(),
            })),
            args: vec![Expr::Qualified(QualifiedName {
                segments: vec![id("Io"), id("println")],
                span: ds(),
            })],
            span: ds(),
        };
        let body = Expr::Pipe {
            lhs: Box::new(list_expr),
            rhs: Box::new(foreach_call),
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Io),
            "[1,2,3] |> List.forEach Io.println must yield {{io}}, got {caps:?}"
        );
        assert!(
            !caps.contains(Capability::Fs),
            "must not contain {{fs}}, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-11: infer_pipe_caps_propagate (pure HOF) ─────────────────────────

    #[test]
    fn infer_pipe_caps_pure() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        bind_poly_cap(&mut ctx, "List.length", list_length_scheme(&b));

        // body: [1,2,3] |> List.length
        let list_expr = Expr::List {
            elems: vec![int_lit(1), int_lit(2), int_lit(3)],
            span: ds(),
        };
        let body = Expr::Pipe {
            lhs: Box::new(list_expr),
            rhs: Box::new(Expr::Qualified(QualifiedName {
                segments: vec![id("List"), id("length")],
                span: ds(),
            })),
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.is_pure(),
            "[1,2,3] |> List.length must be pure, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-12: infer_if_branches_union ───────────────────────────────────────

    #[test]
    fn infer_if_branches_union() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b).ty);
        bind_mono(&mut ctx, "Fs.readFile", fs_readfile_scheme(&b).ty);

        // body: if cond then Io.println "x" else Fs.readFile "y"
        let body = Expr::If {
            cond: Box::new(Expr::Ident(id("cond"))),
            then_branch: Box::new(Expr::Call {
                callee: Box::new(Expr::Qualified(QualifiedName {
                    segments: vec![id("Io"), id("println")],
                    span: ds(),
                })),
                args: vec![text_lit("x")],
                span: ds(),
            }),
            else_branch: Some(Box::new(Expr::Call {
                callee: Box::new(Expr::Qualified(QualifiedName {
                    segments: vec![id("Fs"), id("readFile")],
                    span: ds(),
                })),
                args: vec![text_lit("y")],
                span: ds(),
            })),
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Io) && caps.contains(Capability::Fs),
            "if branches must union io+fs, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-13: infer_match_arms_union ────────────────────────────────────────

    #[test]
    fn infer_match_arms_union() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b).ty);
        bind_mono(&mut ctx, "Fs.readFile", fs_readfile_scheme(&b).ty);

        // body: match x { Some _ -> Io.println "x"; None -> Fs.readFile "y" }
        let arms = vec![
            MatchArm {
                pattern: Pattern::Constructor {
                    name: id("Some"),
                    fields: None,
                    args: vec![],
                    span: ds(),
                },
                guard: None,
                body: Expr::Call {
                    callee: Box::new(Expr::Qualified(QualifiedName {
                        segments: vec![id("Io"), id("println")],
                        span: ds(),
                    })),
                    args: vec![text_lit("x")],
                    span: ds(),
                },
                span: ds(),
            },
            MatchArm {
                pattern: Pattern::Constructor {
                    name: id("None"),
                    fields: None,
                    args: vec![],
                    span: ds(),
                },
                guard: None,
                body: Expr::Call {
                    callee: Box::new(Expr::Qualified(QualifiedName {
                        segments: vec![id("Fs"), id("readFile")],
                        span: ds(),
                    })),
                    args: vec![text_lit("y")],
                    span: ds(),
                },
                span: ds(),
            },
        ];

        let body = Expr::Match {
            scrutinee: Box::new(Expr::Ident(id("x"))),
            arms,
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Io) && caps.contains(Capability::Fs),
            "match arms must union io+fs, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T13-14: infer_try_block_propagates ────────────────────────────────────

    #[test]
    fn infer_try_block_propagates() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b).ty);
        bind_mono(&mut ctx, "Fs.readFile", fs_readfile_scheme(&b).ty);

        // body: try { Io.println "x"; Fs.readFile "y" }
        let inner_block = block(vec![
            Expr::Call {
                callee: Box::new(Expr::Qualified(QualifiedName {
                    segments: vec![id("Io"), id("println")],
                    span: ds(),
                })),
                args: vec![text_lit("x")],
                span: ds(),
            },
            Expr::Call {
                callee: Box::new(Expr::Qualified(QualifiedName {
                    segments: vec![id("Fs"), id("readFile")],
                    span: ds(),
                })),
                args: vec![text_lit("y")],
                span: ds(),
            },
        ]);

        let body = Expr::Try {
            block: inner_block,
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Io) && caps.contains(Capability::Fs),
            "try block must propagate io+fs, got {caps:?}"
        );
        ctx.env.pop_frame();
    }
}
