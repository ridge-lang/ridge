//! The range test that makes `Int` mean what the language says it means.
//!
//! The rule is one sentence: `Int` is a 64-bit signed integer, and an operation
//! whose result leaves that range raises. Nothing in that sentence names a
//! runtime. This file is the BEAM's answer to it, and the only place in the
//! compiler that knows how wide this runtime's immediate integers are.
//!
//! ## Why a test is needed at all
//!
//! `erlang:'+'` is wider than Ridge's `+`: it answers an arbitrary-precision
//! integer where the language promises 64 bits. The BEAM spelling of the
//! primitive therefore does not, by itself, deliver the Ridge type, so codegen
//! narrows it here. That mismatch is a fact about this host and no other: a
//! backend whose addition is already 64 bits wide reads the overflow bit out of
//! the instruction instead, and writes that in its own crate.
//!
//! ## Why the test is tiered
//!
//! The largest immediate integer on this VM is `2^59 - 1`, so
//! `9223372036854775807` is itself boxed: comparing against it allocates rather
//! than comparing a machine word. Testing the immediate range first is a word
//! comparison, and any value inside it is trivially inside 64 bits, so the
//! boxed test runs only past `2^59`. The same remedy is applied to the ingress
//! guards in `ridge_rt.erl`, where it was measured first.
//!
//! ## Why it is expanded here rather than called
//!
//! Measured on OTP 28, 20M iterations of a tail-recursive loop, against the
//! same loop with no test:
//!
//! | shape | cost |
//! |---|---|
//! | expanded at the operation (this file) | 1.25x, and 1.82x with every operation in the loop tested |
//! | a helper function in the emitted module | 2.4x |
//! | a helper function in the runtime shim | 3.7x |
//! | untiered comparison, expanded | 15x |
//!
//! A clause body holding a call that can *return* forces the enclosing function
//! to build a stack frame, which costs the loop its tail call — that is the
//! whole of the difference between the first row and the next two. `erlang:error/1`
//! does not cost it, because the compiler knows it never returns. So the test may
//! end in a raise, but it must not end in a call that comes back.
//!
//! ## The failure
//!
//! `{ridge_int_out_of_range, <<"Int.add">>, Value}` — the term `ridge_rt`
//! already describes, and the same one the ingress guards raise, so an overflow
//! reads the same whether it came from `+` or from `Int.parse`.

#![allow(clippy::redundant_pub_crate)]

use crate::core_ast::{CErlAtom, CErlClause, CErlExpr, CErlLit, CErlPat, CErlVar};

/// The bounds of `Int`. These are the language's, not the runtime's.
const INT_MIN: i64 = i64::MIN;
/// See [`INT_MIN`].
const INT_MAX: i64 = i64::MAX;

/// The widest integer this VM holds as an immediate rather than a boxed bignum,
/// `±2^59`. A fact about the BEAM, load-bearing only for speed: the tier below
/// is what the test would cost without it.
const FIXNUM_MIN: i64 = -576_460_752_303_423_488;
/// See [`FIXNUM_MIN`].
const FIXNUM_MAX: i64 = 576_460_752_303_423_487;

/// The variable the tested result is bound to inside the emitted `case`.
///
/// Reused at every site on purpose. Core Erlang scopes a clause's pattern to
/// that clause, so two of these nesting shadows rather than collides; a
/// nested pair was compiled with `erlc +from_core` and run to confirm it.
const RESULT_VAR: &str = "V_IntRange";

/// The language's own spelling of a stdlib symbol: `("std.int", "add")` reads
/// back as `Int.add`.
///
/// This is what the failure names, and it is deliberately neither the operator
/// (`+`, which the caller may not have written — `Int.add a b` lowers to the
/// same place) nor the host's (`erlang:'+'`, which is not part of the language
/// at all). The ingress guards shipped with this convention already: an
/// overflow reads the same whether it arrived through arithmetic or through
/// `Int.parse`.
pub(crate) fn ridge_label(module: &str, name: &str) -> String {
    let tail = module.rsplit('.').next().unwrap_or(module);
    let mut chars = tail.chars();
    let head = match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>(),
        None => return name.to_owned(),
    };
    format!("{head}{rest}.{name}", rest = chars.as_str())
}

/// `call 'erlang':'<op>' (lhs, rhs)`.
fn bif(op: &str, args: Vec<CErlExpr>) -> CErlExpr {
    CErlExpr::Call {
        module: CErlAtom("erlang".into()),
        fn_name: CErlAtom(op.into()),
        args,
    }
}

/// `V >= lo and V =< hi`, as a clause guard.
///
/// `erlang:and/2` rather than a short-circuiting form: both sides are word
/// comparisons on a value already in a register, and `and/2` is one of the BIFs
/// admitted in guard position.
fn within(lo: i64, hi: i64) -> CErlExpr {
    let var = || CErlExpr::Var(CErlVar(RESULT_VAR.into()));
    bif(
        "and",
        vec![
            bif("=<", vec![CErlExpr::Lit(CErlLit::Int(lo)), var()]),
            bif("=<", vec![var(), CErlExpr::Lit(CErlLit::Int(hi))]),
        ],
    )
}

/// One clause of the emitted test: `<V> when <guard> -> <body>`.
fn clause(guard: CErlExpr, body: CErlExpr) -> CErlClause {
    CErlClause {
        pattern: CErlPat::Var(CErlVar(RESULT_VAR.into())),
        guard,
        body,
    }
}

/// Wrap `result` — the emitted call to the host's operation — in the test that
/// decides whether it is a value of type `Int`.
///
/// `op_label` is the name the failure reports, in the language's own spelling
/// (`Int.add`, not `+` and not `erlang:'+'`), matching what the ingress guards
/// already raise.
pub(crate) fn narrow_to_int(result: CErlExpr, op_label: &str) -> CErlExpr {
    let var = CErlExpr::Var(CErlVar(RESULT_VAR.into()));
    CErlExpr::Case {
        scrutinee: Box::new(result),
        clauses: vec![
            // Inside the immediate range, therefore inside `Int`. One word
            // comparison each way, and the answer for all but a rounding error's
            // worth of real arithmetic.
            clause(within(FIXNUM_MIN, FIXNUM_MAX), var.clone()),
            // Boxed, but still inside `Int`.
            clause(within(INT_MIN, INT_MAX), var.clone()),
            // Outside. The value exists — it is the one the host produced — and
            // reporting it is most of what makes the failure legible.
            clause(
                CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                bif(
                    "error",
                    vec![CErlExpr::Tuple(vec![
                        CErlExpr::Lit(CErlLit::Atom(CErlAtom("ridge_int_out_of_range".into()))),
                        CErlExpr::Lit(CErlLit::Binary(op_label.as_bytes().to_vec())),
                        var,
                    ])],
                ),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::print_expr;

    fn add() -> CErlExpr {
        bif(
            "+",
            vec![
                CErlExpr::Var(CErlVar("V_A".into())),
                CErlExpr::Var(CErlVar("V_B".into())),
            ],
        )
    }

    #[test]
    fn the_ridge_label_is_the_qualified_language_name() {
        assert_eq!(ridge_label("std.int", "add"), "Int.add");
        assert_eq!(ridge_label("std.int", "neg"), "Int.neg");
        // Same shape as the labels the ingress guards already raise.
        assert_eq!(ridge_label("std.float", "round"), "Float.round");
    }

    #[test]
    fn the_untested_call_survives_as_the_scrutinee() {
        // The test wraps the operation; it must not replace it. If this ever
        // stops holding, arithmetic silently stops happening.
        let printed = print_expr(&narrow_to_int(add(), "Int.add"));
        assert!(
            printed.contains("call 'erlang':'+' (V_A, V_B)"),
            "the guarded form must still perform the operation; got: {printed}"
        );
    }

    #[test]
    fn both_tiers_and_the_raise_are_present() {
        let printed = print_expr(&narrow_to_int(add(), "Int.add"));
        for needle in [
            "576460752303423487",
            "9223372036854775807",
            "-9223372036854775808",
            "ridge_int_out_of_range",
            "erlang':'error'",
        ] {
            assert!(
                printed.contains(needle),
                "guarded form is missing {needle}; got: {printed}"
            );
        }
    }

    #[test]
    fn the_immediate_tier_is_tested_before_the_boxed_one() {
        // The order is the entire performance argument: a value inside the
        // immediate range must never reach a comparison against a bignum.
        let printed = print_expr(&narrow_to_int(add(), "Int.add"));
        let fixnum = printed
            .find("576460752303423487")
            .expect("immediate bound must appear");
        let boxed = printed
            .find("9223372036854775807")
            .expect("64-bit bound must appear");
        assert!(
            fixnum < boxed,
            "the immediate-range clause must come first; got: {printed}"
        );
    }

    #[test]
    fn the_label_is_the_ridge_name() {
        // `Int.add`, not `+` and not `erlang:'+'` — the failure names the
        // operation the way the language does, as the ingress guards already do.
        let printed = print_expr(&narrow_to_int(add(), "Int.add"));
        // `Int.add` as a binary literal, byte by byte.
        let expected: String = "Int.add"
            .bytes()
            .map(|b| format!("#<{b}>(8,1,'integer',['unsigned'|['big']])"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            printed.contains(&expected),
            "failure term must carry the Ridge name; got: {printed}"
        );
    }
}
