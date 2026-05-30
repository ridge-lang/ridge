//! Capability checking pass (T14).
//!
//! Implements the four verification rules from spec §6.3 / §4.14 / §8.2:
//!
//! - **Rule 1 (pure call check):** If a decl has no declared capabilities,
//!   `infer_caps(body)` must be `∅`; else `T014 CapabilityNotDeclared`.
//!
//! - **Rule 2 (caller ⊇ callee):** At each call site in the body, look up the
//!   callee's caps; if `callee_caps ⊄ enclosing_declared_caps` → `T018`.
//!
//! - **Rule 3 (inference + verification):** If a decl carries declared caps,
//!   `inferred ⊆ declared`; else (file-private, D040), the inferred set becomes
//!   the canonical effective set at all call sites.
//!
//! - **Rule 4 (inner-fn subset, D058):** If an inner `fn` declares its own cap
//!   prefix, that prefix ⊆ the enclosing fn's declared/inferred set.
//!
//! # D040 — file-private decls
//!
//! Decls whose name starts with `_` are file-private.  They skip the
//! declared-vs-inferred check (Rule 3 is a no-op); the inferred set is the
//! canonical effective set used at every call site.  A caller that doesn't
//! have the required caps will still get `T018` (Rule 2 applies normally at
//! the call site in the *caller's* body).

use ridge_ast::{Body, Capability, Expr, Span};
use ridge_types::{BuiltinTyCons, CapabilitySet};

use crate::caps_infer::{infer_caps, infer_caps_block};
use crate::ctx::InferCtx;
use crate::error::TypeError;

// ── Public entry point ────────────────────────────────────────────────────────

/// Check capabilities for one fn / on-handler / init-decl body.
///
/// # Parameters
///
/// - `ctx` — inference context (env holds callee schemes for lookup).
/// - `b` — builtin type-constructor handles.
/// - `decl_name` — the declaration's name (used in diagnostics and for the
///   file-private `_`-prefix detection per D040).
/// - `declared` — `Some(cs)` when the decl carries an explicit cap annotation;
///   `None` for file-private decls where the inferred set is canonical.
/// - `body` — the declaration body expression.
/// - `decl_span` — source span of the declaration's cap-annotation position
///   (used in T014 diagnostics).
///
/// # Returns
///
/// The *effective* capability set (declared if `Some`, otherwise inferred).
/// This is useful for the caller to propagate when checking inner-fn subsets.
///
/// All diagnostics are pushed directly into `ctx.errors`.
pub fn check_caps_decl(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    decl_name: &str,
    declared: Option<CapabilitySet>,
    body: &Expr,
    decl_span: Span,
) -> CapabilitySet {
    // Step 1: Infer caps from the body.
    let inferred = infer_caps(ctx, b, body);

    // Step 2 (Rule 3): If declared is Some, check inferred ⊆ declared.
    // File-private decls (D040) skip this — declared is always None for them.
    if let Some(declared_set) = declared {
        if !inferred.is_subset(&declared_set) {
            let missing = inferred.difference(&declared_set);
            ctx.errors.push(TypeError::CapabilityNotDeclared {
                decl: decl_name.to_owned(),
                declared: declared_set,
                inferred,
                missing,
                span: decl_span,
            });
        }
    }

    // Step 3: Effective set — declared if Some, else inferred (D040).
    let effective = declared.unwrap_or(inferred);

    // Step 4 (Rule 2 + Rule 4): Walk call sites and inner fns in the body.
    check_body(ctx, b, decl_name, effective, body);

    effective
}

/// Convenience wrapper for `Block`-bodied decls (e.g. `InitDecl`).
pub fn check_caps_block(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    decl_name: &str,
    declared: Option<CapabilitySet>,
    block: &ridge_ast::Block,
    decl_span: Span,
) -> CapabilitySet {
    let inferred = infer_caps_block(ctx, b, block);

    if let Some(declared_set) = declared {
        if !inferred.is_subset(&declared_set) {
            let missing = inferred.difference(&declared_set);
            ctx.errors.push(TypeError::CapabilityNotDeclared {
                decl: decl_name.to_owned(),
                declared: declared_set,
                inferred,
                missing,
                span: decl_span,
            });
        }
    }

    let effective = declared.unwrap_or(inferred);

    // Walk statements for call-site and inner-fn checks.
    for stmt in &block.stmts {
        check_body(ctx, b, decl_name, effective, stmt);
    }

    effective
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Returns `true` when `name` is file-private per D040 (starts with `_`).
#[must_use]
pub fn is_file_private(name: &str) -> bool {
    name.starts_with('_')
}

/// Build a `CapabilitySet` from a slice of `Capability` values from the AST.
#[must_use]
pub fn caps_from_ast_slice(caps: &[Capability]) -> CapabilitySet {
    let mut cs = CapabilitySet::PURE;
    for &c in caps {
        cs.insert(c);
    }
    cs
}

/// Recursively walk `expr`, applying Rule 2 (call-site subset check) and
/// Rule 4 (inner-fn subset check) against `enclosing_effective`.
///
/// This does NOT re-run `infer_caps` — that was already done at the top.
/// It only needs to find: (a) call sites to check callee caps ⊆ enclosing, and
/// (b) `InnerFn` nodes to enforce the inner-fn subset rule.
#[expect(
    clippy::too_many_lines,
    reason = "flat exhaustive match over all Expr variants"
)]
fn check_body(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    enclosing_name: &str,
    enclosing_effective: CapabilitySet,
    expr: &Expr,
) {
    match expr {
        // ── Pure leaves / pure accessor ──────────────────────────────────────
        Expr::Literal(_)
        | Expr::Unit(_)
        | Expr::Ident(_)
        | Expr::Qualified(_)
        | Expr::FieldAccessorFn { .. } => {}

        // ── Call (Rule 2) ─────────────────────────────────────────────────────
        // caps_of_callee returns the capability contribution of this call site.
        // We check that contribution ⊆ enclosing effective set.
        Expr::Call { callee, args, span } => {
            let callee_cs = call_site_caps(ctx, callee, args);
            if !callee_cs.is_subset(&enclosing_effective) {
                let missing = callee_cs.difference(&enclosing_effective);
                let callee_name = expr_name(callee);
                ctx.errors.push(TypeError::CallerCapabilityInsufficient {
                    caller: enclosing_name.to_owned(),
                    callee: callee_name,
                    missing,
                    span: *span,
                });
            }
            // Recurse into callee and args.
            check_body(ctx, b, enclosing_name, enclosing_effective, callee);
            for a in args {
                check_body(ctx, b, enclosing_name, enclosing_effective, a);
            }
        }

        // ── Pipe `|>` ─────────────────────────────────────────────────────────
        // Pipe(lhs, rhs) — the rhs is the function (possibly a Call node).
        // We handle rhs specially: if it's a Call node, we do the same
        // check as above for the callee (the pipe feeds lhs as the last arg).
        Expr::Pipe { lhs, rhs, .. } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, lhs);
            // For a Pipe the effective callee is the rhs.  If rhs is itself a
            // Call node the Call arm will recurse into it and check the callee.
            check_body(ctx, b, enclosing_name, enclosing_effective, rhs);
        }

        // ── Send (no cap contribution per §8.1) ──────────────────────────────
        Expr::Send {
            handle, message, ..
        } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, handle);
            check_body(ctx, b, enclosing_name, enclosing_effective, message);
        }

        // ── Ask ({time} contribution per §8.1 / D018 Model B) ────────────────
        // The Ask expression itself introduces {time}; Rule 2 applies.
        Expr::Ask {
            handle, args, span, ..
        } => {
            let ask_caps = CapabilitySet::singleton(ridge_ast::Capability::Time);
            if !ask_caps.is_subset(&enclosing_effective) {
                let missing = ask_caps.difference(&enclosing_effective);
                ctx.errors.push(TypeError::CallerCapabilityInsufficient {
                    caller: enclosing_name.to_owned(),
                    callee: "ask".to_owned(),
                    missing,
                    span: *span,
                });
            }
            check_body(ctx, b, enclosing_name, enclosing_effective, handle);
            for a in args {
                check_body(ctx, b, enclosing_name, enclosing_effective, a);
            }
        }

        // ── Spawn ({spawn} contribution per §8.1 / D061) ─────────────────────
        Expr::Spawn { args, span, .. } => {
            let spawn_caps = CapabilitySet::singleton(ridge_ast::Capability::Spawn);
            if !spawn_caps.is_subset(&enclosing_effective) {
                let missing = spawn_caps.difference(&enclosing_effective);
                ctx.errors.push(TypeError::CallerCapabilityInsufficient {
                    caller: enclosing_name.to_owned(),
                    callee: "spawn".to_owned(),
                    missing,
                    span: *span,
                });
            }
            for a in args {
                check_body(ctx, b, enclosing_name, enclosing_effective, a);
            }
        }

        // ── InnerFn (Rule 4 — D058) ───────────────────────────────────────────
        // The inner fn's declared caps must be ⊆ the enclosing effective set.
        // After that we recurse into the inner fn's body with its own effective
        // set (so that the inner fn's call sites are also validated).
        Expr::InnerFn { decl, span } => {
            let inner_declared: CapabilitySet = caps_from_ast_slice(&decl.caps);

            // Rule 4: inner declared ⊆ enclosing effective.
            if !inner_declared.is_subset(&enclosing_effective) {
                let missing = inner_declared.difference(&enclosing_effective);
                ctx.errors.push(TypeError::CapabilityNotDeclared {
                    decl: decl.name.text.clone(),
                    declared: enclosing_effective, // enclosing is the constraint
                    inferred: inner_declared,
                    missing,
                    span: *span,
                });
            }

            // Recurse into the inner fn's body using its own effective set.
            // Inner fns always have Body::Expr; Body::Ffi is top-level stdlib only.
            let inner_effective = if let Body::Expr(e) = &decl.body {
                if decl.caps.is_empty() {
                    // Pure inner fn — no declared caps; infer for its body checks.
                    infer_caps(ctx, b, e)
                } else {
                    inner_declared
                }
            } else {
                inner_declared
            };
            if let Body::Expr(e) = &decl.body {
                check_body(ctx, b, &decl.name.text, inner_effective, e);
            }
        }

        // ── Lambda ────────────────────────────────────────────────────────────
        // Lambda body caps are isolated from the enclosing decl (§8.1).
        // We do NOT propagate enclosing_effective into the lambda body — the
        // lambda body's own call-site checks use whatever caps that lambda
        // has (which are checked at lambda-call sites, not here).
        // Per the plan T14 scope: we don't check lambda bodies against
        // the enclosing decl's caps.
        Expr::Lambda { body, .. } => {
            // Recurse purely to check any nested inner-fns / calls inside the
            // lambda body using the lambda's own (future) effective set.
            // For T14 we pass the enclosing effective as a conservative bound —
            // lambda bodies are allowed to use the enclosing caps transitively.
            check_body(ctx, b, enclosing_name, enclosing_effective, body);
        }

        // ── Block / Try block ────────────────────────────────────────────────
        Expr::Block(block) | Expr::Try { block, .. } => {
            for stmt in &block.stmts {
                check_body(ctx, b, enclosing_name, enclosing_effective, stmt);
            }
        }

        // ── Guard ─────────────────────────────────────────────────────────────
        Expr::Guard {
            cond, else_branch, ..
        } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, cond);
            for stmt in &else_branch.stmts {
                check_body(ctx, b, enclosing_name, enclosing_effective, stmt);
            }
        }

        // ── If ────────────────────────────────────────────────────────────────
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, cond);
            check_body(ctx, b, enclosing_name, enclosing_effective, then_branch);
            if let Some(e) = else_branch {
                check_body(ctx, b, enclosing_name, enclosing_effective, e);
            }
        }

        // ── Match ─────────────────────────────────────────────────────────────
        Expr::Match {
            scrutinee, arms, ..
        } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    check_body(ctx, b, enclosing_name, enclosing_effective, g);
                }
                check_body(ctx, b, enclosing_name, enclosing_effective, &arm.body);
            }
        }

        // ── Let / Var / Return ────────────────────────────────────────────────
        Expr::Let { value, .. } | Expr::Var { value, .. } | Expr::Return { value, .. } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, value);
        }

        // ── Assign ───────────────────────────────────────────────────────────
        Expr::Assign { target, value, .. } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, target);
            check_body(ctx, b, enclosing_name, enclosing_effective, value);
        }

        // ── Propagate `?` / Paren ─────────────────────────────────────────────
        Expr::Propagate { inner, .. } | Expr::Paren { inner, .. } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, inner);
        }

        // ── Binary operators ──────────────────────────────────────────────────
        Expr::Binary { lhs, rhs, .. } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, lhs);
            check_body(ctx, b, enclosing_name, enclosing_effective, rhs);
        }

        // ── Unary operators ───────────────────────────────────────────────────
        Expr::Unary { expr, .. } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, expr);
        }

        // ── Tuple / List ──────────────────────────────────────────────────────
        Expr::Tuple { elems, .. } | Expr::List { elems, .. } => {
            for e in elems {
                check_body(ctx, b, enclosing_name, enclosing_effective, e);
            }
        }

        // ── Record / With / FieldAccess ───────────────────────────────────────
        Expr::Record { fields, .. } => {
            for f in fields {
                if let Some(ref val) = f.value {
                    check_body(ctx, b, enclosing_name, enclosing_effective, val);
                }
            }
        }
        Expr::With { base, fields, .. } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, base);
            for f in fields {
                if let Some(ref val) = f.value {
                    check_body(ctx, b, enclosing_name, enclosing_effective, val);
                }
            }
        }
        Expr::FieldAccess { base, .. } => {
            check_body(ctx, b, enclosing_name, enclosing_effective, base);
        }

        // ── String interpolation ──────────────────────────────────────────────
        Expr::Interp { parts, .. } => {
            use ridge_ast::InterpPart;
            for p in parts {
                if let InterpPart::Expr { expr: e, .. } = p {
                    check_body(ctx, b, enclosing_name, enclosing_effective, e);
                }
            }
        }
    }
}

/// Compute the capability contribution of a *call site* (`Expr::Call` callee)
/// for the T18 per-call-site check.
///
/// This is a deliberately conservative simplification of
/// `caps_infer::caps_of_callee`: it reads the callee's concrete caps from the
/// scheme but does **not** perform HOF cap resolution.  For a higher-order
/// callee whose own `Type::Fn` carries a `CapRow::Var` (e.g. `List.forEach`),
/// the effect arrives only through the callback argument, so this function
/// reports `PURE` — it under-attributes such call sites.
///
/// That under-attribution is sound because T14 (`caps_infer::infer_caps`),
/// which *does* resolve HOF caps via D041, is the enforcing pass: a function
/// whose body leaks a capability through a HOF callback has that capability in
/// its inferred set and is rejected by T14 unless it declares it.  The
/// `cap_d041_hof_callback_leak` fixture locks this invariant in.  A full
/// reconciliation so T18 attributes HOF call sites precisely is deferred to the
/// capability audit (it needs the mutable instantiation context that T14 has
/// and this later, immutable pass does not).
fn call_site_caps(ctx: &InferCtx, callee: &Expr, _args: &[Expr]) -> CapabilitySet {
    match callee {
        Expr::Ident(id) => ctx
            .env
            .lookup(&id.text)
            .cloned()
            .map_or(CapabilitySet::PURE, |s| caps_from_scheme(&s)),
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
                .map_or(CapabilitySet::PURE, |s| caps_from_scheme(&s))
        }
        // Lambda called inline — caps of the lambda body are not visible here
        // (they are isolated per §8.1); calling a lambda contributes nothing.
        _ => CapabilitySet::PURE,
    }
}

/// Extract the concrete capability set from the top-level `Type::Fn` of a scheme.
const fn caps_from_scheme(scheme: &ridge_types::Scheme) -> CapabilitySet {
    use ridge_types::{CapRow, Type};
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

/// Return a human-readable name for a callee expression (used in diagnostics).
fn expr_name(e: &Expr) -> String {
    match e {
        Expr::Ident(id) => id.text.clone(),
        Expr::Qualified(q) => q
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("."),
        _ => "<expr>".to_owned(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{
        Block, Capability, Expr, FnDecl, Ident, Literal, QualifiedName, Span, Visibility,
    };
    use ridge_types::{BuiltinTyCons, CapRow, Scheme, TyConArena, Type};

    // ── Test helpers ──────────────────────────────────────────────────────────

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

    fn bind_mono(ctx: &mut InferCtx, name: &str, ty: Type) {
        ctx.env.bind(name.to_string(), Scheme::mono(ty));
    }

    fn int_lit(n: i64) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.to_string(),
            span: ds(),
        })
    }

    fn text_lit(s: &str) -> Expr {
        Expr::Literal(Literal::Text {
            raw: format!("\"{s}\""),
            span: ds(),
        })
    }

    fn block(stmts: Vec<Expr>) -> Block {
        Block { stmts, span: ds() }
    }

    fn block_expr(stmts: Vec<Expr>) -> Expr {
        Expr::Block(block(stmts))
    }

    /// Build `fn {io} Text -> Unit` (Io.println).
    fn io_println_scheme(b: &BuiltinTyCons) -> Type {
        let io_caps = CapabilitySet::singleton(Capability::Io);
        Type::Fn {
            params: vec![Type::Con(b.text, vec![])],
            ret: Box::new(Type::Con(b.unit, vec![])),
            caps: CapRow::Concrete(io_caps),
        }
    }

    /// Build `fn {fs} Text -> Result Text Text` (Fs.readFile).
    fn fs_readfile_scheme(b: &BuiltinTyCons) -> Type {
        let fs_caps = CapabilitySet::singleton(Capability::Fs);
        Type::Fn {
            params: vec![Type::Con(b.text, vec![])],
            ret: Box::new(Type::Con(
                b.result,
                vec![Type::Con(b.text, vec![]), Type::Con(b.text, vec![])],
            )),
            caps: CapRow::Concrete(fs_caps),
        }
    }

    /// Build a call `Io.println "hi"`.
    fn io_call() -> Expr {
        Expr::Call {
            callee: Box::new(Expr::Qualified(QualifiedName {
                segments: vec![id("Io"), id("println")],
                span: ds(),
            })),
            args: vec![text_lit("hi")],
            span: ds(),
        }
    }

    /// Build a call `Fs.readFile "path"`.
    fn fs_call() -> Expr {
        Expr::Call {
            callee: Box::new(Expr::Qualified(QualifiedName {
                segments: vec![id("Fs"), id("readFile")],
                span: ds(),
            })),
            args: vec![text_lit("path")],
            span: ds(),
        }
    }

    // Helper: collect error codes from ctx
    fn error_codes(ctx: &InferCtx) -> Vec<&'static str> {
        ctx.errors
            .iter()
            .map(super::super::error::TypeError::code)
            .collect()
    }

    // ── T14-1: pure_fn_pure_body_ok ───────────────────────────────────────────
    // Decl with no declared caps, body uses no caps → no errors.
    #[test]
    fn pure_fn_pure_body_ok() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let body = block_expr(vec![int_lit(1)]);
        // declared = PURE (no caps declared)
        check_caps_decl(
            &mut ctx,
            &b,
            "pureFunc",
            Some(CapabilitySet::PURE),
            &body,
            ds(),
        );

        assert!(
            ctx.errors.is_empty(),
            "pure body with pure decl should have no errors, got {:?}",
            ctx.errors
        );
        ctx.env.pop_frame();
    }

    // ── T14-2: pure_fn_io_body_T014 ───────────────────────────────────────────
    // Decl with no declared caps, body calls Io.println → T014 {io}.
    #[test]
    fn pure_fn_io_body_t014() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b));

        let body = io_call();
        check_caps_decl(
            &mut ctx,
            &b,
            "pureFunc",
            Some(CapabilitySet::PURE),
            &body,
            ds(),
        );

        let codes = error_codes(&ctx);
        assert!(
            codes.contains(&"T014"),
            "pure fn calling Io.println must emit T014, got {codes:?}"
        );
        // Verify the missing set contains io.
        let t014 = ctx.errors.iter().find(|e| e.code() == "T014").unwrap();
        if let TypeError::CapabilityNotDeclared { missing, .. } = t014 {
            assert!(
                missing.contains(Capability::Io),
                "missing must include {{io}}, got {missing:?}"
            );
        } else {
            panic!("expected T014");
        }
        ctx.env.pop_frame();
    }

    // ── T14-3: decl_io_body_io_ok ─────────────────────────────────────────────
    // Decl declared `{io}`, body calls Io.println → no errors.
    #[test]
    fn decl_io_body_io_ok() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b));

        let body = io_call();
        let declared = CapabilitySet::singleton(Capability::Io);
        check_caps_decl(&mut ctx, &b, "ioFunc", Some(declared), &body, ds());

        assert!(
            ctx.errors.is_empty(),
            "io decl with io body must have no errors, got {:?}",
            ctx.errors
        );
        ctx.env.pop_frame();
    }

    // ── T14-4: decl_io_body_io_and_fs_T014 ───────────────────────────────────
    // Decl declared `{io}`, body calls Io and Fs → T014 with missing = {fs}.
    #[test]
    fn decl_io_body_io_and_fs_t014() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b));
        bind_mono(&mut ctx, "Fs.readFile", fs_readfile_scheme(&b));

        let body = block_expr(vec![io_call(), fs_call()]);
        let declared = CapabilitySet::singleton(Capability::Io);
        check_caps_decl(&mut ctx, &b, "partialFunc", Some(declared), &body, ds());

        let codes = error_codes(&ctx);
        assert!(
            codes.contains(&"T014"),
            "decl with only {{io}} calling {{io, fs}} must emit T014, got {codes:?}"
        );
        let t014 = ctx.errors.iter().find(|e| e.code() == "T014").unwrap();
        if let TypeError::CapabilityNotDeclared { missing, .. } = t014 {
            assert!(
                missing.contains(Capability::Fs),
                "missing must include {{fs}}, got {missing:?}"
            );
            assert!(
                !missing.contains(Capability::Io),
                "missing must NOT include {{io}}, got {missing:?}"
            );
        } else {
            panic!("expected T014");
        }
        ctx.env.pop_frame();
    }

    // ── T14-5: caller_callee_subset_ok ────────────────────────────────────────
    // Decl A declared `{io, fs}`, calls decl B declared `{io}` → no errors.
    #[test]
    fn caller_callee_subset_ok() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Bind B as a fn with {io} caps.
        let b_ty = io_println_scheme(&b);
        bind_mono(&mut ctx, "funcB", b_ty);

        // A body: calls funcB with one arg.
        let body = Expr::Call {
            callee: Box::new(Expr::Ident(id("funcB"))),
            args: vec![text_lit("x")],
            span: ds(),
        };
        let declared = CapabilitySet::singleton(Capability::Io)
            .union(&CapabilitySet::singleton(Capability::Fs));
        check_caps_decl(&mut ctx, &b, "funcA", Some(declared), &body, ds());

        assert!(
            ctx.errors.is_empty(),
            "caller {{io, fs}} calling callee {{io}} should have no errors, got {:?}",
            ctx.errors
        );
        ctx.env.pop_frame();
    }

    // ── T14-6: caller_callee_subset_T018 ─────────────────────────────────────
    // Decl A declared `{io}`, calls decl B declared `{io, fs}` → T018 missing={fs}.
    #[test]
    fn caller_callee_subset_t018() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Bind B as a fn with {io, fs} caps.
        let io_fs = CapabilitySet::singleton(Capability::Io)
            .union(&CapabilitySet::singleton(Capability::Fs));
        let b_ty = Type::Fn {
            params: vec![Type::Con(b.text, vec![])],
            ret: Box::new(Type::Con(b.unit, vec![])),
            caps: CapRow::Concrete(io_fs),
        };
        bind_mono(&mut ctx, "funcB", b_ty);

        // A body: calls funcB.
        let body = Expr::Call {
            callee: Box::new(Expr::Ident(id("funcB"))),
            args: vec![text_lit("x")],
            span: ds(),
        };
        let declared = CapabilitySet::singleton(Capability::Io);
        check_caps_decl(&mut ctx, &b, "funcA", Some(declared), &body, ds());

        let codes = error_codes(&ctx);
        assert!(
            codes.contains(&"T018"),
            "caller {{io}} calling callee {{io, fs}} must emit T018, got {codes:?}"
        );
        let t018 = ctx.errors.iter().find(|e| e.code() == "T018").unwrap();
        if let TypeError::CallerCapabilityInsufficient { missing, .. } = t018 {
            assert!(
                missing.contains(Capability::Fs),
                "missing must include {{fs}}, got {missing:?}"
            );
        } else {
            panic!("expected T018");
        }
        ctx.env.pop_frame();
    }

    // ── T14-7: file_private_decl_skips_declared_check ────────────────────────
    // D040: decl name starts with `_`, no declared caps, body calls Io → no T014.
    #[test]
    fn file_private_decl_skips_declared_check() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b));

        let body = io_call();
        // File-private: declared = None (D040 — caller passes None for _-prefixed decls).
        check_caps_decl(&mut ctx, &b, "_helper", None, &body, ds());

        // No T014 must be emitted (no declared-vs-inferred check for file-private).
        let t014_errors: Vec<_> = ctx.errors.iter().filter(|e| e.code() == "T014").collect();
        assert!(
            t014_errors.is_empty(),
            "file-private decl must NOT emit T014, got {t014_errors:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T14-8: file_private_decl_used_at_caller ───────────────────────────────
    // Decl A declared `{io}`, calls _helper (file-private, body uses {io}) → no T018.
    #[test]
    fn file_private_decl_used_at_caller() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // _helper is bound in the env with {io} caps (as if computed by T13 earlier).
        bind_mono(&mut ctx, "_helper", io_println_scheme(&b));

        let body = Expr::Call {
            callee: Box::new(Expr::Ident(id("_helper"))),
            args: vec![text_lit("msg")],
            span: ds(),
        };
        let declared = CapabilitySet::singleton(Capability::Io);
        check_caps_decl(&mut ctx, &b, "funcA", Some(declared), &body, ds());

        assert!(
            ctx.errors.is_empty(),
            "caller {{io}} calling _helper {{io}} should have no T018, got {:?}",
            ctx.errors
        );
        ctx.env.pop_frame();
    }

    // ── T14-9: file_private_decl_caller_insufficient_T018 ────────────────────
    // Decl A declared PURE, calls _helper (file-private, body uses {io}) → T018.
    #[test]
    fn file_private_decl_caller_insufficient_t018() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // _helper has {io} caps in the env.
        bind_mono(&mut ctx, "_helper", io_println_scheme(&b));

        let body = Expr::Call {
            callee: Box::new(Expr::Ident(id("_helper"))),
            args: vec![text_lit("msg")],
            span: ds(),
        };
        // Pure caller tries to call _helper which has {io}.
        check_caps_decl(
            &mut ctx,
            &b,
            "pureFunc",
            Some(CapabilitySet::PURE),
            &body,
            ds(),
        );

        let codes = error_codes(&ctx);
        assert!(
            codes.contains(&"T018"),
            "pure caller calling _helper {{io}} must emit T018, got {codes:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T14-10: inner_fn_subset_T014 ─────────────────────────────────────────
    // Outer fn declared `{io}`, inner fn declared `{fs}` → T014 (D058: inner ⊄ outer).
    #[test]
    fn inner_fn_subset_t014() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Build inner fn with {fs} declared.
        let inner_decl = FnDecl {
            attrs: vec![],
            vis: Visibility::Private,
            caps: vec![Capability::Fs],
            name: id("innerHelper"),
            params: vec![],
            ret: None,
            body: ridge_ast::Body::Expr(int_lit(1)),
            span: ds(),
            doc: None,
        };
        let body = Expr::InnerFn {
            decl: Box::new(inner_decl),
            span: ds(),
        };

        let declared = CapabilitySet::singleton(Capability::Io);
        check_caps_decl(&mut ctx, &b, "outerFunc", Some(declared), &body, ds());

        let codes = error_codes(&ctx);
        assert!(
            codes.contains(&"T014"),
            "inner fn with {{fs}} inside outer {{io}} must emit T014 (D058), got {codes:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T14-11: inner_fn_subset_ok ────────────────────────────────────────────
    // Outer fn declared `{io, fs}`, inner fn declared `{io}` → no error.
    #[test]
    fn inner_fn_subset_ok() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let inner_decl = FnDecl {
            attrs: vec![],
            vis: Visibility::Private,
            caps: vec![Capability::Io],
            name: id("innerHelper"),
            params: vec![],
            ret: None,
            body: ridge_ast::Body::Expr(int_lit(1)),
            span: ds(),
            doc: None,
        };
        let body = Expr::InnerFn {
            decl: Box::new(inner_decl),
            span: ds(),
        };

        let declared = CapabilitySet::singleton(Capability::Io)
            .union(&CapabilitySet::singleton(Capability::Fs));
        check_caps_decl(&mut ctx, &b, "outerFunc", Some(declared), &body, ds());

        assert!(
            ctx.errors.is_empty(),
            "inner fn {{io}} inside outer {{io, fs}} must have no errors, got {:?}",
            ctx.errors
        );
        ctx.env.pop_frame();
    }

    // ── T14-12: hof_polymorphic_propagates ───────────────────────────────────
    // Outer declared `{io}`, body = `[1,2,3] |> List.forEach Io.println` → no error.
    // The HOF cap resolves to {io}, which ⊆ outer's {io}.
    #[test]
    fn hof_polymorphic_propagates() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Bind Io.println with {io} caps.
        bind_mono(&mut ctx, "Io.println", io_println_scheme(&b));

        // Bind List.forEach as a HOF scheme with cap var c.
        // For the call-site check we bind it as a concrete {io} scheme
        // (simulating what T6 would produce after unification resolves the cap var).
        // This is the simplest correct approach for T14's scope.
        let io_caps = CapabilitySet::singleton(Capability::Io);
        let foreach_ty = Type::Fn {
            params: vec![
                Type::Fn {
                    params: vec![Type::Con(b.int, vec![])],
                    ret: Box::new(Type::Con(b.unit, vec![])),
                    caps: CapRow::Concrete(io_caps),
                },
                Type::Con(b.list, vec![Type::Con(b.int, vec![])]),
            ],
            ret: Box::new(Type::Con(b.unit, vec![])),
            caps: CapRow::Concrete(io_caps),
        };
        bind_mono(&mut ctx, "List.forEach", foreach_ty);

        // Build `[1, 2, 3] |> List.forEach Io.println`.
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

        let declared = CapabilitySet::singleton(Capability::Io);
        check_caps_decl(&mut ctx, &b, "processItems", Some(declared), &body, ds());

        assert!(
            ctx.errors.is_empty(),
            "outer {{io}} with List.forEach Io.println must have no errors, got {:?}",
            ctx.errors
        );
        ctx.env.pop_frame();
    }

    // ── T14-bonus: is_file_private helper ─────────────────────────────────────
    #[test]
    fn is_file_private_detects_underscore_prefix() {
        assert!(is_file_private("_helper"));
        assert!(is_file_private("_"));
        assert!(is_file_private("_privateImpl"));
        assert!(!is_file_private("publicFn"));
        assert!(!is_file_private("helper"));
        assert!(!is_file_private(""));
    }

    // ── T14-bonus2: ask_requires_time_T018 ───────────────────────────────────
    // A pure fn containing an Ask expression → T018 (Ask contributes {time}).
    #[test]
    fn ask_requires_time_t018() {
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

        // Pure caller — does not declare {time}.
        check_caps_decl(
            &mut ctx,
            &b,
            "pureFunc",
            Some(CapabilitySet::PURE),
            &body,
            ds(),
        );

        let codes = error_codes(&ctx);
        assert!(
            codes.contains(&"T018"),
            "pure fn using Ask must emit T018 for {{time}}, got {codes:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T14-bonus3: spawn_requires_spawn_T018 ────────────────────────────────
    // A pure fn containing a Spawn expression → T018 (Spawn contributes {spawn}).
    #[test]
    fn spawn_requires_spawn_t018() {
        let (_arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let body = Expr::Spawn {
            actor: id("MyActor"),
            args: vec![],
            span: ds(),
        };

        check_caps_decl(
            &mut ctx,
            &b,
            "pureFunc",
            Some(CapabilitySet::PURE),
            &body,
            ds(),
        );

        let codes = error_codes(&ctx);
        assert!(
            codes.contains(&"T018"),
            "pure fn using Spawn must emit T018 for {{spawn}}, got {codes:?}"
        );
        ctx.env.pop_frame();
    }
}
