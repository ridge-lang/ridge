//! Core Erlang text printer.
//!
//! Walks a [`CErlModule`] (or any sub-node) and produces deterministic
//! Core Erlang source text.  The output is byte-stable across runs because
//! all ordering follows `Vec` insertion order — no `HashMap` iteration.
//!
//! # Format notes
//!
//! - Atoms are always single-quoted: `'name'`.
//! - Variables are emitted verbatim (must already be uppercase-starting).
//! - Floats use the `ryu` crate for cross-platform determinism (plan R12).
//! - Annotations (`CErlAnn`) are deferred to T8; function bodies print without them.

use std::fmt::Write as FmtWrite;

use crate::core_ast::{
    CErlAnn, CErlAtom, CErlClause, CErlExpr, CErlFn, CErlLit, CErlModule, CErlPat, CErlVar,
};

// ── Module printer ────────────────────────────────────────────────────────────

/// Print a [`CErlModule`] to a Core Erlang source string.
///
/// Output is deterministic and byte-stable across calls with equal inputs.
#[must_use]
pub fn print_module(m: &CErlModule) -> String {
    let mut s = String::new();

    // module 'name' [exports]
    let _ = write!(s, "module {} [", print_atom(&m.name));
    for (i, exp) in m.exports.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        let _ = write!(s, "{}/{}", print_atom(&exp.name), exp.arity);
    }
    s.push_str("]\n");

    // attributes [...]
    s.push_str("  attributes [");
    for (i, attr) in m.attributes.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        let name = print_atom(&attr.name);
        let val = print_lit(&attr.value);
        let _ = write!(s, "{{{name},[{val}]}}");
    }
    s.push_str("]\n");

    // fn defs
    for f in &m.fns {
        s.push_str(&print_fn(f));
        s.push('\n');
    }

    s.push_str("end");
    s
}

/// Print a single top-level function definition.
///
/// Each [`CErlAnn`] in `f.anns` is emitted as a comment line (indented 2 spaces)
/// immediately before the `name/arity =` header, so that `%% File:` and
/// `%% Caps:` annotations are visible in the `.core` text.  See OQ-E011 (§3.11).
///
/// ## `erlc +from_core` annotation wrapper
///
/// The function body is emitted in the `( fun (Params) -> body -| [] )` form
/// required by `erlc +from_core`.  This annotation wrapper (with empty `[]`)
/// serves two purposes:
///
/// 1. **Boundary delimiter**: `erlc +from_core` uses the `)` to find the end of
///    one function definition and the start of the next.  Without it consecutive
///    definitions fail to parse.
/// 2. **`end` avoidance**: in this form the `fun` keyword does **NOT** use an
///    `end` terminator — the annotation wrapper closes the `fun` expression.
///    This prevents the `end end` problem that arises when a `case … end` appears
///    at the tail of the `fun` body (the Core Erlang parser cannot tolerate two
///    consecutive `end` keywords even on different lines).
///
/// The top-level `CErlFn.body` is always `CErlExpr::Fun { params, body }` (set
/// by `item::lower_fn`), so we pattern-match it here and emit the annotation-form
/// directly instead of delegating to [`print_expr`].
#[must_use]
pub fn print_fn(f: &CErlFn) -> String {
    let name = print_atom(&f.name);
    let arity = f.arity;
    let mut s = String::new();
    for CErlAnn(ann) in &f.anns {
        s.push_str("  ");
        s.push_str(ann);
        s.push('\n');
    }
    // Top-level CErlFn.body is always CErlExpr::Fun { params, body } (item::lower_fn).
    // We special-case it here to emit the `( fun (Params) -> body -| [] )` form
    // instead of the `fun … end` form, which avoids the consecutive-`end` parse error.
    match &f.body {
        CErlExpr::Fun { params, body } => {
            let param_list = params.iter().map(print_var).collect::<Vec<_>>().join(", ");
            let body_str = print_expr(body);
            let _ = write!(
                s,
                "{name}/{arity} =\n    ( fun ({param_list}) ->\n          {body_str}\n      -| [] )"
            );
        }
        // Fallback for any non-Fun body (should not occur in practice).
        other => {
            let body_str = print_expr(other);
            let _ = write!(s, "{name}/{arity} =\n    ( {body_str}\n      -| [] )");
        }
    }
    s
}

// ── Atom / var / lit ─────────────────────────────────────────────────────────

/// Print an atom with surrounding single-quotes.
#[must_use]
pub fn print_atom(a: &CErlAtom) -> String {
    let s = &a.0;
    format!("'{s}'")
}

/// Print a variable (emitted verbatim).
#[must_use]
pub fn print_var(v: &CErlVar) -> String {
    v.0.clone()
}

/// Print a literal value.
#[must_use]
pub fn print_lit(l: &CErlLit) -> String {
    match l {
        CErlLit::Int(n) => n.to_string(),
        CErlLit::Float(f) => {
            let mut buf = ryu::Buffer::new();
            buf.format(*f).to_string()
        }
        CErlLit::Atom(a) => print_atom(a),
        CErlLit::Binary(bytes) => {
            // Core Erlang +from_core requires the #{#<N>(8,1,'integer',['unsigned'|['big']])}#
            // bit-syntax form in BOTH expression and pattern position.  The `<<"...">>` form
            // is rejected by erlc with "illegal expression".  Use the same format as
            // print_binary_pat().
            print_binary_lit(bytes)
        }
        CErlLit::Nil => "[]".into(),
        CErlLit::EmptyTuple => "{}".into(),
    }
}

// ── Expression printer ────────────────────────────────────────────────────────

/// Print a Core Erlang expression.
// This function has one branch per CErlExpr variant — the length is inherent to
// the exhaustive match over a 17-variant enum.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn print_expr(e: &CErlExpr) -> String {
    match e {
        CErlExpr::Lit(l) => print_lit(l),
        CErlExpr::Var(v) => print_var(v),
        CErlExpr::Fun { params, body } => {
            // Use `( fun (Params) -> body -| [] )` instead of `fun … end` for ALL
            // lambda expressions (not just top-level functions).  The annotation
            // wrapper replaces the `end` keyword, eliminating the ambiguous `end`
            // problem that arises when a lambda appears as a call argument (e.g.
            // `lists:foldl(fun(…) -> … end, Acc, List)` fails to parse because
            // `erlc +from_core` cannot determine whether `end` closes the `fun` or
            // some outer construct).  `( fun … -| [] )` is unambiguous in all contexts.
            let param_list = params.iter().map(print_var).collect::<Vec<_>>().join(", ");
            let body = print_expr(body);
            format!("( fun ({param_list}) -> {body} -| [] )")
        }
        CErlExpr::Apply { callee, args } => {
            let callee = print_expr(callee);
            let arg_list = args.iter().map(print_expr).collect::<Vec<_>>().join(", ");
            format!("apply {callee} ({arg_list})")
        }
        CErlExpr::Call {
            module,
            fn_name,
            args,
        } => {
            let m = print_atom(module);
            let f = print_atom(fn_name);
            let arg_list = args.iter().map(print_expr).collect::<Vec<_>>().join(", ");
            format!("call {m}:{f} ({arg_list})")
        }
        CErlExpr::LocalFnRef { name, arity } => {
            let name = print_atom(name);
            format!("{name}/{arity}")
        }
        CErlExpr::Let { var, value, body } => {
            let v = print_var(var);
            let val = print_expr(value);
            let b = print_expr(body);
            format!("let {v} = {val} in {b}")
        }
        CErlExpr::LetRec { defs, body } => {
            // Core Erlang letrec requires `'name'/N = fun (params) -> body -| []`
            // NOT `VarName = fun ...`.  We emit the atom/arity form here.
            let def_strs = defs
                .iter()
                .map(|(atom_name, arity, fun_expr)| {
                    let name = print_atom(atom_name);
                    match fun_expr {
                        CErlExpr::Fun { params, body: fun_body } => {
                            let param_list =
                                params.iter().map(print_var).collect::<Vec<_>>().join(", ");
                            let body_str = print_expr(fun_body);
                            format!(
                                "{name}/{arity} =\n        ( fun ({param_list}) ->\n              {body_str}\n          -| [] )"
                            )
                        }
                        // Fallback: the def should always be a Fun; emit a comment
                        // so the file is invalid but diagnosable rather than silently wrong.
                        other => {
                            let expr_str = print_expr(other);
                            format!("{name}/{arity} = {expr_str}")
                        }
                    }
                })
                .collect::<Vec<_>>()
                .join("\n    ");
            let body = print_expr(body);
            format!("letrec {def_strs} in {body}")
        }
        CErlExpr::Case { scrutinee, clauses } => {
            // Core Erlang `case` clauses are separated by whitespace (NOT `;`).
            // Each clause pattern is wrapped in `<…>` angle brackets.
            let scr = print_expr(scrutinee);
            let clause_strs = clauses
                .iter()
                .map(print_clause)
                .collect::<Vec<_>>()
                .join(" ");
            format!("case {scr} of {clause_strs} end")
        }
        CErlExpr::Do { first, then } => {
            let first = print_expr(first);
            let then = print_expr(then);
            format!("do {first} {then}")
        }
        CErlExpr::Tuple(elems) => {
            let elem_strs = elems.iter().map(print_expr).collect::<Vec<_>>().join(", ");
            format!("{{{elem_strs}}}")
        }
        CErlExpr::Cons { head, tail } => {
            let h = print_expr(head);
            let t = print_expr(tail);
            format!("[{h}|{t}]")
        }
        CErlExpr::ListLit(elems) => {
            let elem_strs = elems.iter().map(print_expr).collect::<Vec<_>>().join(", ");
            format!("[{elem_strs}]")
        }
        CErlExpr::MapLit(pairs) => {
            let pair_strs = pairs
                .iter()
                .map(|(k, v)| {
                    let ks = print_expr(k);
                    let vs = print_expr(v);
                    format!("{ks}=>{vs}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("~{{{pair_strs}}}~")
        }
        CErlExpr::MapUpdate { base, updates } => {
            let base = print_expr(base);
            let update_strs = updates
                .iter()
                .map(|(k, v)| {
                    let ks = print_expr(k);
                    let vs = print_expr(v);
                    format!("{ks}=>{vs}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("~{{{update_strs}|{base}}}~")
        }
        CErlExpr::Receive { clauses, after } => {
            let clause_strs = clauses
                .iter()
                .map(print_clause)
                .collect::<Vec<_>>()
                .join("; ");
            if let Some((timeout, body)) = after {
                let t = print_expr(timeout);
                let b = print_expr(body);
                format!("receive {clause_strs} after {t} -> {b} end")
            } else {
                format!("receive {clause_strs} end")
            }
        }
        CErlExpr::Try { body, of, catch } => {
            let b = print_expr(body);
            // `erlc +from_core` imposes constraints on try clause syntax:
            //   - `of` clauses: `<Pattern> ->` only (NO `when` guard).
            //   - `catch` clauses: MUST be three bare variables `<C, V, S> ->`
            //     (no atoms/tuples in the head, no `when` guard).
            // Both differ from the case/receive clause format (`<Pat> when Guard ->`).
            let of_strs = of
                .iter()
                .map(print_of_clause)
                .collect::<Vec<_>>()
                .join("; ");
            let catch_strs = catch
                .iter()
                .map(print_catch_clause)
                .collect::<Vec<_>>()
                .join("; ");
            // NOTE: `try … of … catch …` does NOT emit a trailing `end`.
            // The `erlc +from_core` parser terminates a `try` expression at
            // the end of the last catch clause body — an explicit `end` keyword
            // causes a "syntax error before: 'end'" when the catch body itself
            // ends with a `case … end`.  Without `end`, the `( fun … -| [] )`
            // annotation wrapper or the enclosing `let … in` serves as the
            // structural delimiter, which the parser handles correctly.
            format!("try {b} of {of_strs} catch {catch_strs}")
        }
    }
}

// ── Clause printer ────────────────────────────────────────────────────────────

/// Print a single case/receive clause.
///
/// Core Erlang case/receive clause patterns are wrapped in `<…>` angle
/// brackets as required by `erlc +from_core`.  The guard `when 'true'` is
/// emitted explicitly (it is required by the Core Erlang grammar even when
/// trivial).
///
/// Each wildcard (`CErlPat::Wild`) within a clause pattern is assigned a
/// unique anonymous variable name (`_Wc0`, `_Wc1`, …) scoped to this clause.
/// Core Erlang `erlc +from_core` rejects duplicate uses of `_` in a single
/// clause (e.g. `[_|_]` triggers "duplicate variable '_'").
#[must_use]
pub fn print_clause(c: &CErlClause) -> String {
    let mut wc = 0u32;
    let pat = print_pat_with_wc(&c.pattern, &mut wc);
    // Core Erlang always requires an explicit `when` guard — emit `when 'true'`
    // even for unconditional clauses (the grammar mandates it).
    let guard = print_expr(&c.guard);
    let body = print_expr(&c.body);
    // Pattern wrapped in `<…>` per Core Erlang syntax.
    format!("<{pat}> when {guard} -> {body}")
}

/// Print a single `try … of` clause.
///
/// `erlc +from_core` does NOT accept a `when` guard in `of` clauses — the
/// format is `<Pattern> -> Body` (no `when` keyword).  This differs from
/// case/receive clauses which require `when Guard`.
///
/// The `guard` field of the [`CErlClause`] is intentionally ignored here;
/// `wrap_with_return_catch` stores `Lit(Atom("true"))` there for AST
/// completeness but the printer must not emit it.
#[must_use]
fn print_of_clause(c: &CErlClause) -> String {
    let mut wc = 0u32;
    let pat = print_pat_with_wc(&c.pattern, &mut wc);
    let body = print_expr(&c.body);
    format!("<{pat}> -> {body}")
}

/// Print a single `try … catch` clause.
///
/// `erlc +from_core` imposes two constraints on catch clause heads:
///
/// 1. **No `when` guard** — `<P1, P2, P3> -> Body` only.
/// 2. **Three bare variables** — atom literals or tuple patterns in the catch
///    head cause a syntax error.  All pattern dispatch must happen inside the
///    catch body via an ordinary `case` expression.
///
/// The [`CErlClause`] `pattern` field is expected to be a
/// `CErlPat::Tuple` with exactly 3 `CErlPat::Var` elements
/// (the exception class, value, and stacktrace variables).  We unwrap the
/// tuple and emit `<Class, Value, Stk> -> Body`.
///
/// If the pattern is not a 3-element tuple (defensive: should not occur for
/// `wrap_with_return_catch`-generated clauses), we fall back to
/// `<Pattern> -> Body` which will likely be rejected by `erlc` but at least
/// produces diagnosable output.
#[must_use]
fn print_catch_clause(c: &CErlClause) -> String {
    let body = print_expr(&c.body);
    // Unwrap the 3-element tuple into comma-separated bare variables.
    // `guard` is intentionally ignored — erlc +from_core does not allow `when`
    // in catch clause heads.
    if let CErlPat::Tuple(elems) = &c.pattern {
        if elems.len() == 3 {
            let mut wc = 0u32;
            let cls = print_pat_with_wc(&elems[0], &mut wc);
            let val = print_pat_with_wc(&elems[1], &mut wc);
            let stk = print_pat_with_wc(&elems[2], &mut wc);
            return format!("<{cls}, {val}, {stk}> -> {body}");
        }
    }
    // Fallback (should not occur for ridge_return clauses).
    let mut wc = 0u32;
    let pat = print_pat_with_wc(&c.pattern, &mut wc);
    format!("<{pat}> -> {body}")
}

// ── Pattern printer ───────────────────────────────────────────────────────────

/// Print a Core Erlang pattern.
///
/// ## Binary literal patterns
///
/// In Core Erlang `+from_core`, binary/bitstring patterns use the `#{...}#`
/// bit-syntax form rather than the `<<"...">>` expression form.  Each byte is
/// encoded as `#<N>(8,1,'integer',['unsigned'|['big']])`.
/// The expression form `<<"...">>` is only valid in expression position.
///
/// ## Wildcards
///
/// When a pattern appears inside a match/case clause, use [`print_clause`]
/// (or [`print_pat_with_wc`] directly) so that multiple wildcards in the same
/// clause get unique names (`_Wc0`, `_Wc1`, …).  This function emits plain
/// `"_"` and must only be called in contexts where at most one wildcard can
/// appear (e.g. single-pattern positions, unit tests).
#[must_use]
pub fn print_pat(p: &CErlPat) -> String {
    let mut wc = 0u32;
    print_pat_with_wc(p, &mut wc)
}

/// Print a Core Erlang pattern, assigning unique names to wildcards.
///
/// `wc_counter` is incremented each time a `CErlPat::Wild` is encountered,
/// producing `_Wc0`, `_Wc1`, … per clause.  Callers must reset the counter
/// between clauses to ensure clause-scoped uniqueness.
///
/// `erlc +from_core` rejects duplicate uses of `_` in a single clause
/// (e.g. `[_|_]` triggers "duplicate variable '_'").
#[must_use]
fn print_pat_with_wc(p: &CErlPat, wc_counter: &mut u32) -> String {
    match p {
        CErlPat::Var(v) => print_var(v),
        // Binary literals in pattern position require the #{...}# bit-syntax form.
        CErlPat::Lit(CErlLit::Binary(bytes)) => print_binary_pat(bytes),
        CErlPat::Lit(l) => print_lit(l),
        CErlPat::Tuple(pats) => {
            let pat_strs = pats
                .iter()
                .map(|p| print_pat_with_wc(p, wc_counter))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{pat_strs}}}")
        }
        CErlPat::Cons { head, tail } => {
            let h = print_pat_with_wc(head, wc_counter);
            let t = print_pat_with_wc(tail, wc_counter);
            format!("[{h}|{t}]")
        }
        CErlPat::Alias { var, inner } => {
            let inner = print_pat_with_wc(inner, wc_counter);
            let v = print_var(var);
            format!("{inner} = {v}")
        }
        CErlPat::MapPat(pairs) => {
            let pair_strs = pairs
                .iter()
                .map(|(k, v)| {
                    let ks = print_expr(k);
                    let vs = print_pat_with_wc(v, wc_counter);
                    format!("{ks}:={vs}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("~{{{pair_strs}}}~")
        }
        CErlPat::Wild => {
            let n = *wc_counter;
            *wc_counter += 1;
            format!("_Wc{n}")
        }
    }
}

/// Print a binary/bitstring in the Core Erlang `#{...}#` bit-syntax form.
///
/// Required by `erlc +from_core` in **both expression and pattern position**.
/// Each byte is encoded as `#<N>(8,1,'integer',['unsigned'|['big']])`:
/// - `N` = the byte value as an integer literal.
/// - `8` = 8 bits per element.
/// - `1` = unit size 1.
/// - `'integer'` = element type.
/// - `['unsigned'|['big']]` = unsigned big-endian.
///
/// Empty binary → `#{}#`.
#[must_use]
fn print_binary_lit(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "#{}#".into();
    }
    let segments: Vec<String> = bytes
        .iter()
        .map(|b| format!("#<{b}>(8,1,'integer',['unsigned'|['big']])"))
        .collect();
    // `#{seg1,seg2,...}#` — the outer `#{ }#` is the bit-syntax wrapper.
    let mut s = String::from("#{");
    s.push_str(&segments.join(","));
    s.push_str("}#");
    s
}

/// Print a binary/bitstring literal in **pattern position**.
///
/// Delegates to [`print_binary_lit`] — the `#{...}#` form is required in both
/// expression and pattern position by `erlc +from_core`.
#[must_use]
fn print_binary_pat(bytes: &[u8]) -> String {
    print_binary_lit(bytes)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAnn, CErlAttribute, CErlClause, CErlExport, CErlFn, CErlModule};

    fn atom(s: &str) -> CErlExpr {
        CErlExpr::Lit(CErlLit::Atom(CErlAtom(s.into())))
    }

    fn var(s: &str) -> CErlExpr {
        CErlExpr::Var(CErlVar(s.into()))
    }

    fn true_guard() -> CErlExpr {
        atom("true")
    }

    // ── Test 2 ────────────────────────────────────────────────────────────────

    #[test]
    fn printer_lit_variants_roundtrip() {
        assert_eq!(print_lit(&CErlLit::Int(42)), "42");
        assert_eq!(print_lit(&CErlLit::Int(-7)), "-7");

        let float_str = print_lit(&CErlLit::Float(1.5));
        assert!(float_str.contains('1'), "float: {float_str}");

        let atom_str = print_lit(&CErlLit::Atom(CErlAtom("ok".into())));
        assert_eq!(atom_str, "'ok'");

        // Binary literals must use the #{#<N>(...)}# bit-syntax form (not <<"..">>).
        // erlc +from_core rejects <<"...">> in expression position.
        let bin_str = print_lit(&CErlLit::Binary(b"hello".to_vec()));
        assert!(
            bin_str.starts_with("#{"),
            "binary must use bit-syntax form: {bin_str}"
        );
        assert!(bin_str.contains("#<104>"), "binary 'h'=104: {bin_str}");
        // Empty binary
        let empty_bin = print_lit(&CErlLit::Binary(vec![]));
        assert_eq!(empty_bin, "#{}#", "empty binary: {empty_bin}");

        assert_eq!(print_lit(&CErlLit::Nil), "[]");
        assert_eq!(print_lit(&CErlLit::EmptyTuple), "{}");
    }

    // ── Test 3 ────────────────────────────────────────────────────────────────

    #[test]
    fn printer_var_roundtrip() {
        let e = CErlExpr::Var(CErlVar("X".into()));
        assert_eq!(print_expr(&e), "X");
    }

    // ── Test 4 ────────────────────────────────────────────────────────────────

    #[test]
    fn printer_tuple_and_list_roundtrip() {
        // Tuple
        let t = CErlExpr::Tuple(vec![
            CErlExpr::Lit(CErlLit::Int(1)),
            CErlExpr::Lit(CErlLit::Int(2)),
        ]);
        let ts = print_expr(&t);
        assert!(ts.starts_with('{'), "tuple: {ts}");
        assert!(ts.contains('1'), "tuple has 1: {ts}");
        assert!(ts.contains('2'), "tuple has 2: {ts}");

        // ListLit
        let l = CErlExpr::ListLit(vec![
            CErlExpr::Lit(CErlLit::Int(10)),
            CErlExpr::Lit(CErlLit::Int(20)),
        ]);
        let ls = print_expr(&l);
        assert!(ls.starts_with('['), "list: {ls}");
        assert!(ls.contains("10"), "list has 10: {ls}");

        // Cons
        let c = CErlExpr::Cons {
            head: Box::new(CErlExpr::Lit(CErlLit::Int(1))),
            tail: Box::new(CErlExpr::Lit(CErlLit::Nil)),
        };
        let cs = print_expr(&c);
        assert!(cs.contains('|'), "cons: {cs}");
    }

    // ── Test 5 ────────────────────────────────────────────────────────────────

    #[test]
    fn printer_let_and_case_roundtrip() {
        // Let
        let let_e = CErlExpr::Let {
            var: CErlVar("X".into()),
            value: Box::new(CErlExpr::Lit(CErlLit::Int(42))),
            body: Box::new(var("X")),
        };
        let ls = print_expr(&let_e);
        assert!(ls.contains("let"), "let: {ls}");
        assert!(ls.contains("42"), "let has 42: {ls}");

        // Case with no-guard clause
        let case_e = CErlExpr::Case {
            scrutinee: Box::new(var("X")),
            clauses: vec![CErlClause {
                pattern: CErlPat::Wild,
                guard: true_guard(),
                body: atom("ok"),
            }],
        };
        let cs = print_expr(&case_e);
        assert!(cs.contains("case"), "case: {cs}");
        assert!(cs.contains("end"), "case end: {cs}");
        assert!(cs.contains('_'), "wildcard: {cs}");
        // Core Erlang +from_core requires explicit `when` even for trivial guards.
        assert!(cs.contains("when 'true'"), "must have when true: {cs}");
        // Pattern must be wrapped in angle brackets.
        // Wildcards are now emitted as _Wc0, _Wc1, … (unique per clause) to
        // satisfy erlc +from_core's "duplicate variable '_'" constraint.
        assert!(
            cs.contains("<_Wc0>"),
            "pattern must be in angle brackets: {cs}"
        );
    }

    // ── Test 6 ────────────────────────────────────────────────────────────────

    #[test]
    fn printer_apply_call_fun_roundtrip() {
        // Fun
        let fun_e = CErlExpr::Fun {
            params: vec![CErlVar("X".into()), CErlVar("Y".into())],
            body: Box::new(var("X")),
        };
        let fs = print_expr(&fun_e);
        assert!(fs.contains("fun"), "fun: {fs}");
        // Lambdas now use `( fun … -| [] )` annotation form instead of `fun … end`.
        // The `end` keyword is replaced by `)` — check for the annotation wrapper.
        assert!(fs.contains("-| []"), "fun annotation wrapper: {fs}");
        assert!(fs.contains("X, Y"), "params: {fs}");

        // Apply
        let apply_e = CErlExpr::Apply {
            callee: Box::new(CErlExpr::LocalFnRef {
                name: CErlAtom("foo".into()),
                arity: 1,
            }),
            args: vec![CErlExpr::Lit(CErlLit::Int(0))],
        };
        let ap = print_expr(&apply_e);
        assert!(ap.contains("apply"), "apply: {ap}");
        assert!(ap.contains("'foo'/1"), "localfnref: {ap}");

        // Call
        let call_e = CErlExpr::Call {
            module: CErlAtom("erlang".into()),
            fn_name: CErlAtom("length".into()),
            args: vec![CErlExpr::Lit(CErlLit::Nil)],
        };
        let cs = print_expr(&call_e);
        assert!(cs.contains("call"), "call: {cs}");
        assert!(cs.contains("'erlang'"), "module: {cs}");
        assert!(cs.contains("'length'"), "fn: {cs}");

        // LocalFnRef standalone
        let lfr = CErlExpr::LocalFnRef {
            name: CErlAtom("bar".into()),
            arity: 2,
        };
        assert_eq!(print_expr(&lfr), "'bar'/2");
    }

    // ── Test 7 ────────────────────────────────────────────────────────────────

    #[test]
    fn printer_map_and_letrec_and_do_roundtrip() {
        // MapLit
        let map_e = CErlExpr::MapLit(vec![(atom("key"), CErlExpr::Lit(CErlLit::Int(1)))]);
        let ms = print_expr(&map_e);
        assert!(ms.contains("~{"), "maplit: {ms}");
        assert!(ms.contains("=>"), "maplit arrow: {ms}");

        // MapUpdate
        let upd_e = CErlExpr::MapUpdate {
            base: Box::new(var("M")),
            updates: vec![(atom("k"), CErlExpr::Lit(CErlLit::Int(2)))],
        };
        let us = print_expr(&upd_e);
        assert!(us.contains("~{"), "mapupdate: {us}");
        assert!(us.contains('|'), "mapupdate pipe: {us}");

        // LetRec
        let letrec_e = CErlExpr::LetRec {
            defs: vec![(
                CErlAtom("f".into()),
                1u32,
                CErlExpr::Fun {
                    params: vec![CErlVar("X".into())],
                    body: Box::new(atom("ok")),
                },
            )],
            body: Box::new(atom("done")),
        };
        let lr = print_expr(&letrec_e);
        assert!(lr.contains("letrec"), "letrec: {lr}");

        // Do
        let do_e = CErlExpr::Do {
            first: Box::new(atom("side")),
            then: Box::new(atom("result")),
        };
        let ds = print_expr(&do_e);
        assert!(ds.contains("do"), "do: {ds}");

        // Receive
        let recv_e = CErlExpr::Receive {
            clauses: vec![CErlClause {
                pattern: CErlPat::Var(CErlVar("Msg".into())),
                guard: true_guard(),
                body: atom("ok"),
            }],
            after: None,
        };
        let rs = print_expr(&recv_e);
        assert!(rs.contains("receive"), "receive: {rs}");

        // Try
        let try_e = CErlExpr::Try {
            body: Box::new(atom("risky")),
            of: vec![CErlClause {
                pattern: CErlPat::Var(CErlVar("R".into())),
                guard: true_guard(),
                body: var("R"),
            }],
            catch: vec![CErlClause {
                pattern: CErlPat::Wild,
                guard: true_guard(),
                body: atom("error"),
            }],
        };
        let ts = print_expr(&try_e);
        assert!(ts.contains("try"), "try: {ts}");
        assert!(ts.contains("catch"), "catch: {ts}");
    }

    // ── Test 8 ────────────────────────────────────────────────────────────────

    #[test]
    fn printer_determinism() {
        let m = build_sample_module();
        let a = print_module(&m);
        let b = print_module(&m);
        assert_eq!(a, b, "printer output must be byte-identical across calls");
    }

    // ── Test 9 ────────────────────────────────────────────────────────────────

    #[test]
    fn printer_smoke_roundtrip_snapshot() {
        let m = build_sample_module();
        let printed = print_module(&m);
        insta::assert_snapshot!(printed);
    }

    // ── Helper ────────────────────────────────────────────────────────────────

    fn build_sample_module() -> CErlModule {
        CErlModule {
            name: CErlAtom("ridge_smoke".into()),
            exports: vec![
                CErlExport {
                    name: CErlAtom("main".into()),
                    arity: 1,
                },
                CErlExport {
                    name: CErlAtom("helper".into()),
                    arity: 0,
                },
            ],
            attributes: vec![CErlAttribute {
                name: CErlAtom("file".into()),
                value: CErlLit::Atom(CErlAtom("smoke.ridge".into())),
            }],
            fns: vec![
                CErlFn {
                    name: CErlAtom("main".into()),
                    arity: 1,
                    anns: vec![CErlAnn("-| []".into())],
                    body: CErlExpr::Fun {
                        params: vec![CErlVar("Args".into())],
                        body: Box::new(CErlExpr::Let {
                            var: CErlVar("Result".into()),
                            value: Box::new(CErlExpr::Call {
                                module: CErlAtom("erlang".into()),
                                fn_name: CErlAtom("length".into()),
                                args: vec![var("Args")],
                            }),
                            body: Box::new(var("Result")),
                        }),
                    },
                },
                CErlFn {
                    name: CErlAtom("helper".into()),
                    arity: 0,
                    anns: vec![],
                    body: CErlExpr::Fun {
                        params: vec![],
                        body: Box::new(atom("ok")),
                    },
                },
            ],
        }
    }
}
