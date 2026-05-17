//! Typed Rust model of a subset of Core Erlang (Carlsson 2004) sufficient for 0.1.0.
//!
//! This is the IR-of-Core-Erlang — the printer ([`crate::printer`]) walks it to
//! produce text.  Two-stage emission: IR → `core_ast` → printer string (§3.1).

// Within an enum definition the variants reference the enum's own name, which
// clippy::use_self would ask us to write as `Self` — but that is unidiomatic
// for recursive enum ADTs and not what the plan specifies.
#![allow(clippy::use_self)]

/// A whole Core Erlang module.
#[derive(Debug, Clone)]
pub struct CErlModule {
    /// The module atom, e.g. `'ridge_examples_log_analyzer'`.
    pub name: CErlAtom,
    /// Exported name/arity pairs, e.g. `[main/1, parseLine/1, ...]`.
    pub exports: Vec<CErlExport>,
    /// Module attributes: `attributes [{file, ...}, {capabilities, ...}]`.
    pub attributes: Vec<CErlAttribute>,
    /// Top-level function definitions.
    pub fns: Vec<CErlFn>,
}

/// A Core Erlang export: `<NAME, ARITY>`.
#[derive(Debug, Clone)]
pub struct CErlExport {
    /// The exported function name atom.
    pub name: CErlAtom,
    /// The exported function arity.
    pub arity: u32,
}

/// An attribute on the module (`'<name>' = <literal>`).
#[derive(Debug, Clone)]
pub struct CErlAttribute {
    /// Attribute name atom.
    pub name: CErlAtom,
    /// Attribute value literal.
    pub value: CErlLit,
}

/// A top-level function definition.
#[derive(Debug, Clone)]
pub struct CErlFn {
    /// Function name atom.
    pub name: CErlAtom,
    /// Function arity.
    pub arity: u32,
    /// Annotations, e.g. `( -| [{file, "log_analyzer.rg"}, {line, 47}] )`.
    pub anns: Vec<CErlAnn>,
    /// Body: a `fun (X1, ..., XN) -> ... end` expression.
    pub body: CErlExpr,
}

/// A Core Erlang expression.  Mirrors the subset Phase 6 needs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CErlExpr {
    /// Literal: `42`, `1.5`, `'true'`, `<<"hi">>`, `[]`, `{}`, atom.
    Lit(CErlLit),
    /// Variable: `X`, `_Foo`.
    Var(CErlVar),
    /// Anonymous fun: `fun (X1, ..., XN) -> Body end`.
    Fun {
        /// Parameter variable names.
        params: Vec<CErlVar>,
        /// Body expression.
        body: Box<CErlExpr>,
    },
    /// Apply to a fun-valued expression: `apply Fn (A1, ..., AN)`.
    Apply {
        /// The callee expression (must evaluate to a fun).
        callee: Box<CErlExpr>,
        /// Argument expressions.
        args: Vec<CErlExpr>,
    },
    /// Call to a known module:fn/arity: `call 'mod':'fn' (A1, ..., AN)`.
    Call {
        /// The module atom.
        module: CErlAtom,
        /// The function name atom.
        fn_name: CErlAtom,
        /// Argument expressions.
        args: Vec<CErlExpr>,
    },
    /// Primary local-fn reference: `'fname'/3`.
    LocalFnRef {
        /// The local function name atom.
        name: CErlAtom,
        /// The function arity.
        arity: u32,
    },
    /// `let <Var> = <E1> in <E2>` (the only binding form in Core Erlang).
    Let {
        /// The variable being bound.
        var: CErlVar,
        /// The value expression.
        value: Box<CErlExpr>,
        /// The body expression in which `var` is in scope.
        body: Box<CErlExpr>,
    },
    /// `letrec` for recursive lets (only inner-fn recursion uses this).
    ///
    /// Each binding is `(name, arity, fun_expr)` so that the printer can emit
    /// the required Core Erlang form `'name'/N = fun (params) -> body -| []`.
    LetRec {
        /// `(atom_name, arity, fun_expr)` bindings.
        ///
        /// The `fun_expr` must be a `CErlExpr::Fun` at lowering time.
        /// `arity` must equal `fun_expr.params.len()`.
        defs: Vec<(CErlAtom, u32, CErlExpr)>,
        /// The body expression.
        body: Box<CErlExpr>,
    },
    /// `case Scr of P1 when G1 -> B1; P2 when G2 -> B2; ... end`.
    Case {
        /// The scrutinee expression.
        scrutinee: Box<CErlExpr>,
        /// Ordered clauses; each has a pattern, guard, and body.
        clauses: Vec<CErlClause>,
    },
    /// `do E1 E2` (Core Erlang sequencing — for side-effecting blocks).
    Do {
        /// First expression (evaluated for side effects).
        first: Box<CErlExpr>,
        /// Second expression (result of the `do`).
        then: Box<CErlExpr>,
    },
    /// Tuple literal: `{E1, E2, ..., EN}`.
    Tuple(Vec<CErlExpr>),
    /// List cons: `[H | T]`.
    Cons {
        /// The head expression.
        head: Box<CErlExpr>,
        /// The tail expression.
        tail: Box<CErlExpr>,
    },
    /// List literal `[E1, E2, ..., EN]` (sugar for nested cons; printer handles).
    ListLit(Vec<CErlExpr>),
    /// Map literal: `~{ K1 => V1, K2 => V2, ... }~`.
    MapLit(Vec<(CErlExpr, CErlExpr)>),
    /// Map update: `~{ K => V | M }~` (used for `with` updates).
    MapUpdate {
        /// The base map expression.
        base: Box<CErlExpr>,
        /// Key-value update pairs.
        updates: Vec<(CErlExpr, CErlExpr)>,
    },
    /// Receive expression — emitted from `gen_server` clauses, not from `IrExpr`.
    Receive {
        /// Pattern clauses to receive.
        clauses: Vec<CErlClause>,
        /// Optional `after Timeout -> Body` clause.
        after: Option<(Box<CErlExpr>, Box<CErlExpr>)>,
    },
    /// `try E of Pat -> B catch Class:Reason:Stk -> Body end`.
    ///
    /// Used for `?` propagate when the surrounding return scope is `Result a e`
    /// and short-circuit is needed.  Only emitted when the IR shape demands it
    /// (rare in 0.1.0).
    Try {
        /// The body to attempt.
        body: Box<CErlExpr>,
        /// Success clauses (`of` arm).
        of: Vec<CErlClause>,
        /// Catch clauses.
        catch: Vec<CErlClause>,
    },
}

/// A Core Erlang case clause.
#[derive(Debug, Clone)]
pub struct CErlClause {
    /// The clause pattern.
    pub pattern: CErlPat,
    /// The clause guard (`'true'` if no guard).
    pub guard: CErlExpr,
    /// The clause body.
    pub body: CErlExpr,
}

/// A Core Erlang pattern.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CErlPat {
    /// Variable pattern: binds on match.
    Var(CErlVar),
    /// Literal pattern: matches a specific value.
    Lit(CErlLit),
    /// Tuple pattern: `{P1, P2, ..., PN}`.
    Tuple(Vec<CErlPat>),
    /// Cons pattern: `[H | T]`.
    Cons {
        /// Head pattern.
        head: Box<CErlPat>,
        /// Tail pattern.
        tail: Box<CErlPat>,
    },
    /// Alias pattern `Pat = Var` — used for `as`-patterns.
    Alias {
        /// The alias variable.
        var: CErlVar,
        /// The inner pattern.
        inner: Box<CErlPat>,
    },
    /// Map pattern: `~{ K1 := P1, K2 := P2, ... }~`.
    MapPat(Vec<(CErlExpr, CErlPat)>),
    /// `_` — anonymous match (wildcard).
    Wild,
}

/// A Core Erlang literal — what the printer emits without quoting decisions.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CErlLit {
    /// Integer literal, e.g. `42`.
    Int(i64),
    /// Float literal, e.g. `1.5`.
    Float(f64),
    /// Atom literal, e.g. `'ok'`.
    Atom(CErlAtom),
    /// BEAM binary: `<<"...">>` for UTF-8 text.
    Binary(Vec<u8>),
    /// `[]` empty list.
    Nil,
    /// `{}` empty tuple — used for `Unit` only if we choose `{}` over `'ok'`
    /// (we choose `'ok'` per §4 row `IrLit::Unit`).
    EmptyTuple,
}

/// A Core Erlang variable.  Always uppercase-starting.
#[derive(Debug, Clone)]
pub struct CErlVar(pub String);

/// A Core Erlang atom (printable; quote-decisions deferred to printer).
#[derive(Debug, Clone)]
pub struct CErlAtom(pub String);

/// A Core Erlang annotation (`-| [{file,"..."},{line,N}]`).
#[derive(Debug, Clone)]
pub struct CErlAnn(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cerlmodule_constructible() {
        let m = CErlModule {
            name: CErlAtom("ridge_test".into()),
            exports: vec![CErlExport {
                name: CErlAtom("main".into()),
                arity: 1,
            }],
            attributes: vec![CErlAttribute {
                name: CErlAtom("file".into()),
                value: CErlLit::Atom(CErlAtom("test.rg".into())),
            }],
            fns: vec![CErlFn {
                name: CErlAtom("main".into()),
                arity: 1,
                anns: vec![],
                body: CErlExpr::Fun {
                    params: vec![CErlVar("Args".into())],
                    body: Box::new(CErlExpr::Lit(CErlLit::Atom(CErlAtom("ok".into())))),
                },
            }],
        };
        assert_eq!(m.exports.len(), 1);
        assert_eq!(m.attributes.len(), 1);
        assert_eq!(m.fns.len(), 1);
    }
}
