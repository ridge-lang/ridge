//! Immutable visitor trait for the Ridge AST.
//!
//! # Overview
//!
//! [`Visit`] is an immutable traversal trait modelled after Rust's own
//! `rustc_ast::visit::Visitor`.  Every method has a default implementation
//! that delegates to the corresponding `walk_*` free function, which recurses
//! into child nodes.  Override only the methods you care about; the rest of
//! the tree is walked automatically.
//!
//! # Example
//!
//! ```ignore
//! use ridge_ast::visit::{Visit, walk_expr};
//! use ridge_ast::Expr;
//!
//! struct CountExprs { count: usize }
//!
//! impl<'ast> Visit<'ast> for CountExprs {
//!     fn visit_expr(&mut self, e: &'ast Expr) {
//!         self.count += 1;
//!         walk_expr(self, e);   // recurse into children
//!     }
//! }
//! ```
//!
//! # `dyn Visit` compatibility
//!
//! All `walk_*` functions are generic over `V: Visit<'ast> + ?Sized` so the
//! trait can be used behind a `dyn Visit<'ast>` pointer without boxing issues.

use crate::{
    decl::{
        ActorDecl, ActorMember, ConstDecl, Constructor, FieldDecl, FnDecl, ImportDecl, InitDecl,
        MailboxDecl, ModulePath, OnHandler, Param, RecordTypeBody, StateDecl, TypeBody, TypeDecl,
        UnionTypeBody,
    },
    expr::{AskTimeout, FieldInit, InterpPart, LambdaParam, MatchArm, QualifiedName, RecordCtor},
    Block, Expr, FnType, Ident, Item, Module, Pattern, Type,
};


// ── Visit trait ───────────────────────────────────────────────────────────────

/// Immutable visitor for the Ridge AST.
///
/// Each method has a default implementation that delegates to the
/// corresponding [`walk_*`] free function.  Override any subset; the rest of
/// the tree is walked automatically.
pub trait Visit<'ast> {
    /// Visit a parsed module (the root of the AST).
    fn visit_module(&mut self, m: &'ast Module) {
        walk_module(self, m);
    }

    /// Visit a top-level item (import, const, type, fn, actor).
    fn visit_item(&mut self, i: &'ast Item) {
        walk_item(self, i);
    }

    /// Visit an expression.
    fn visit_expr(&mut self, e: &'ast Expr) {
        walk_expr(self, e);
    }

    /// Visit a pattern.
    fn visit_pattern(&mut self, p: &'ast Pattern) {
        walk_pattern(self, p);
    }

    /// Visit a type expression.
    fn visit_type(&mut self, t: &'ast Type) {
        walk_type(self, t);
    }

    /// Visit an identifier (leaf — no further descent).
    fn visit_ident(&mut self, _i: &'ast Ident) {}

    /// Visit a block (sequence of expressions).
    fn visit_block(&mut self, b: &'ast Block) {
        walk_block(self, b);
    }

    /// Visit a single arm of a `match` expression.
    fn visit_match_arm(&mut self, arm: &'ast MatchArm) {
        walk_match_arm(self, arm);
    }

    /// Visit a field initialiser in a record or `with` expression.
    fn visit_field_init(&mut self, fi: &'ast FieldInit) {
        walk_field_init(self, fi);
    }

    /// Visit a parameter of a lambda expression.
    fn visit_lambda_param(&mut self, lp: &'ast LambdaParam) {
        walk_lambda_param(self, lp);
    }

    /// Visit a segment of an interpolated string.
    fn visit_interp_part(&mut self, part: &'ast InterpPart) {
        walk_interp_part(self, part);
    }

    /// Visit a qualified dotted name (`Mod.member`).
    fn visit_qualified_name(&mut self, q: &'ast QualifiedName) {
        walk_qualified_name(self, q);
    }

    /// Visit a record constructor (T8, Phase 4 §3.8).
    ///
    /// Default impl walks both arms: `Bare(ident)` calls [`visit_ident`];
    /// `Qualified(qn)` calls [`visit_qualified_name`].
    fn visit_record_ctor(&mut self, ctor: &'ast RecordCtor) {
        walk_record_ctor(self, ctor);
    }

    // ── Declaration-level visitors ────────────────────────────────────────────

    /// Visit an `import` declaration.
    fn visit_import_decl(&mut self, d: &'ast ImportDecl) {
        walk_import_decl(self, d);
    }

    /// Visit a `const` declaration.
    fn visit_const_decl(&mut self, d: &'ast ConstDecl) {
        walk_const_decl(self, d);
    }

    /// Visit a `type` declaration.
    fn visit_type_decl(&mut self, d: &'ast TypeDecl) {
        walk_type_decl(self, d);
    }

    /// Visit a `fn` declaration.
    fn visit_fn_decl(&mut self, d: &'ast FnDecl) {
        walk_fn_decl(self, d);
    }

    /// Visit an `actor` declaration.
    fn visit_actor_decl(&mut self, d: &'ast ActorDecl) {
        walk_actor_decl(self, d);
    }

    /// Visit a member declaration inside an actor body.
    fn visit_actor_member(&mut self, m: &'ast ActorMember) {
        walk_actor_member(self, m);
    }

    /// Visit a `state` field declaration.
    fn visit_state_decl(&mut self, d: &'ast StateDecl) {
        walk_state_decl(self, d);
    }

    /// Visit an `init` block declaration.
    fn visit_init_decl(&mut self, d: &'ast InitDecl) {
        walk_init_decl(self, d);
    }

    /// Visit an `on` message handler declaration.
    fn visit_on_handler(&mut self, h: &'ast OnHandler) {
        walk_on_handler(self, h);
    }

    /// Visit a `mailbox` configuration member.
    fn visit_mailbox_decl(&mut self, d: &'ast MailboxDecl) {
        walk_mailbox_decl(self, d);
    }

    /// Visit a function parameter.
    fn visit_param(&mut self, p: &'ast Param) {
        walk_param(self, p);
    }

    /// Visit a module path (in an `import` declaration).
    fn visit_module_path(&mut self, mp: &'ast ModulePath) {
        walk_module_path(self, mp);
    }

    /// Visit a type body (record, union, or alias).
    fn visit_type_body(&mut self, tb: &'ast TypeBody) {
        walk_type_body(self, tb);
    }

    /// Visit a record type body.
    fn visit_record_type_body(&mut self, rb: &'ast RecordTypeBody) {
        walk_record_type_body(self, rb);
    }

    /// Visit a single field declaration in a record type.
    fn visit_field_decl(&mut self, fd: &'ast FieldDecl) {
        walk_field_decl(self, fd);
    }

    /// Visit a union type body.
    fn visit_union_type_body(&mut self, ub: &'ast UnionTypeBody) {
        walk_union_type_body(self, ub);
    }

    /// Visit a union constructor alternative.
    fn visit_constructor(&mut self, c: &'ast Constructor) {
        walk_constructor(self, c);
    }

    /// Visit a function type payload.
    fn visit_fn_type(&mut self, ft: &'ast FnType) {
        walk_fn_type(self, ft);
    }
}

// ── walk_module ───────────────────────────────────────────────────────────────

/// Walk all items in a [`Module`].
pub fn walk_module<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, m: &'ast Module) {
    for item in &m.items {
        v.visit_item(item);
    }
}

// ── walk_item ─────────────────────────────────────────────────────────────────

/// Walk the inner declaration of a top-level [`Item`].
pub fn walk_item<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, i: &'ast Item) {
    match i {
        Item::Import(d) => v.visit_import_decl(d),
        Item::Const(d) => v.visit_const_decl(d),
        Item::Type(d) => v.visit_type_decl(d),
        Item::Fn(d) => v.visit_fn_decl(d),
        Item::Actor(d) => v.visit_actor_decl(d),
    }
}

// ── walk_expr ─────────────────────────────────────────────────────────────────

/// Walk all sub-expressions of an [`Expr`].
///
/// The match is exhaustive: adding a new `Expr` variant will cause a compile
/// error here, reminding the implementor to update the visitor.
#[allow(clippy::too_many_lines)] // exhaustive match over every Expr variant — cannot be split
pub fn walk_expr<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, e: &'ast Expr) {
    match e {
        Expr::Literal(_) | Expr::Unit(_) => {}
        Expr::Ident(id) => v.visit_ident(id),
        Expr::Qualified(q) => v.visit_qualified_name(q),
        Expr::Interp { parts, .. } => {
            for part in parts {
                v.visit_interp_part(part);
            }
        }
        Expr::List { elems, .. } | Expr::Tuple { elems, .. } => {
            for elem in elems {
                v.visit_expr(elem);
            }
        }
        Expr::Paren { inner, .. } | Expr::Propagate { inner, .. } => v.visit_expr(inner),
        Expr::FieldAccessorFn { field, .. } => v.visit_ident(field),
        Expr::Binary { lhs, rhs, .. } | Expr::Pipe { lhs, rhs, .. } => {
            v.visit_expr(lhs);
            v.visit_expr(rhs);
        }
        Expr::Unary { expr, .. } => v.visit_expr(expr),
        Expr::Call { callee, args, .. } => {
            v.visit_expr(callee);
            for arg in args {
                v.visit_expr(arg);
            }
        }
        Expr::FieldAccess { base, field, .. } => {
            v.visit_expr(base);
            v.visit_ident(field);
        }
        Expr::Lambda { params, body, .. } => {
            for param in params {
                v.visit_lambda_param(param);
            }
            v.visit_expr(body);
        }
        Expr::InnerFn { decl, .. } => v.visit_fn_decl(decl),
        Expr::Record {
            constructor,
            fields,
            ..
        } => {
            v.visit_record_ctor(constructor);
            for fi in fields {
                v.visit_field_init(fi);
            }
        }
        Expr::RecordLit { fields, .. } => {
            for fi in fields {
                v.visit_field_init(fi);
            }
        }
        Expr::With { base, fields, .. } => {
            v.visit_expr(base);
            for fi in fields {
                v.visit_field_init(fi);
            }
        }
        Expr::Ask {
            handle,
            message,
            args,
            timeout,
            ..
        } => {
            v.visit_expr(handle);
            v.visit_ident(message);
            for arg in args {
                v.visit_expr(arg);
            }
            // Walk the timeout expression if present (OQ-E001 T0).
            if let Some(AskTimeout::Millis(ms_expr)) = timeout {
                v.visit_expr(ms_expr);
            }
        }
        Expr::Send {
            handle, message, ..
        } => {
            v.visit_expr(handle);
            v.visit_expr(message);
        }
        Expr::Spawn { actor, args, .. } => {
            v.visit_ident(actor);
            for arg in args {
                v.visit_expr(arg);
            }
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            v.visit_expr(cond);
            v.visit_expr(then_branch);
            if let Some(eb) = else_branch {
                v.visit_expr(eb);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            v.visit_expr(scrutinee);
            for arm in arms {
                v.visit_match_arm(arm);
            }
        }
        Expr::Try { block, .. } => v.visit_block(block),
        Expr::Guard {
            cond, else_branch, ..
        } => {
            v.visit_expr(cond);
            v.visit_block(else_branch);
        }
        Expr::Return { value, .. } => v.visit_expr(value),
        Expr::Let { pat, ty, value, .. } => {
            v.visit_pattern(pat);
            if let Some(t) = ty {
                v.visit_type(t);
            }
            v.visit_expr(value);
        }
        Expr::Var {
            name, ty, value, ..
        } => {
            v.visit_ident(name);
            if let Some(t) = ty {
                v.visit_type(t);
            }
            v.visit_expr(value);
        }
        Expr::Assign { target, value, .. } => {
            v.visit_expr(target);
            v.visit_expr(value);
        }
        Expr::Block(b) => v.visit_block(b),
    }
}

// ── walk_pattern ──────────────────────────────────────────────────────────────

/// Walk all sub-patterns and identifiers of a [`Pattern`].
///
/// The match is exhaustive so that new variants force a compile error here.
pub fn walk_pattern<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, p: &'ast Pattern) {
    use crate::pattern::ListPatElem;
    match p {
        Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::ListNil { .. } => {}
        Pattern::Var { name, .. } => v.visit_ident(name),
        Pattern::Constructor {
            name, fields, args, ..
        } => {
            v.visit_ident(name);
            if let Some(fps) = fields {
                for fp in fps {
                    v.visit_ident(&fp.name);
                    if let Some(inner) = &fp.pattern {
                        v.visit_pattern(inner);
                    }
                }
            }
            for arg in args {
                v.visit_pattern(arg);
            }
        }
        Pattern::Tuple { elems, .. } => {
            for elem in elems {
                v.visit_pattern(elem);
            }
        }
        Pattern::Cons { head, tail, .. } => {
            v.visit_pattern(head);
            v.visit_pattern(tail);
        }
        Pattern::As { name, inner, .. } => {
            v.visit_ident(name);
            v.visit_pattern(inner);
        }
        Pattern::Paren { inner, .. } => v.visit_pattern(inner),
        Pattern::List { elements, .. } => {
            for elem in elements {
                match elem {
                    ListPatElem::Elem(pat) => v.visit_pattern(pat),
                    ListPatElem::Rest {
                        bind: Some(name), ..
                    } => v.visit_ident(name),
                    ListPatElem::Rest { bind: None, .. } => {}
                }
            }
        }
        Pattern::Record { fields, .. } => {
            for fp in fields {
                v.visit_ident(&fp.name);
                if let Some(inner) = &fp.pattern {
                    v.visit_pattern(inner);
                }
            }
        }
    }
}

// ── walk_type ─────────────────────────────────────────────────────────────────

/// Walk all sub-types of a [`Type`] expression.
///
/// The match is exhaustive so that new variants force a compile error here.
pub fn walk_type<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, t: &'ast Type) {
    match t {
        Type::Primitive { .. } => {}
        Type::Named { name, .. } | Type::Var { name, .. } => v.visit_ident(name),
        Type::App { head, args, .. } => {
            v.visit_ident(head);
            for arg in args {
                v.visit_type(arg);
            }
        }
        Type::Tuple { elems, .. } => {
            for elem in elems {
                v.visit_type(elem);
            }
        }
        Type::List { elem, .. } => v.visit_type(elem),
        Type::Fn { fn_ty, .. } => v.visit_fn_type(fn_ty),
        Type::Paren { inner, .. } => v.visit_type(inner),
        Type::Record { fields, .. } => {
            for field in fields {
                v.visit_ident(&field.name);
                v.visit_type(&field.ty);
            }
        }
    }
}

// ── walk_block ────────────────────────────────────────────────────────────────

/// Walk all statement expressions in a [`Block`].
pub fn walk_block<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, b: &'ast Block) {
    for stmt in &b.stmts {
        v.visit_expr(stmt);
    }
}

// ── walk_match_arm ────────────────────────────────────────────────────────────

/// Walk the pattern, optional guard, and body of a [`MatchArm`].
pub fn walk_match_arm<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, arm: &'ast MatchArm) {
    v.visit_pattern(&arm.pattern);
    if let Some(guard) = &arm.guard {
        v.visit_expr(guard);
    }
    v.visit_expr(&arm.body);
}

// ── walk_field_init ───────────────────────────────────────────────────────────

/// Walk the name and optional value of a [`FieldInit`].
pub fn walk_field_init<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, fi: &'ast FieldInit) {
    v.visit_ident(&fi.name);
    if let Some(val) = &fi.value {
        v.visit_expr(val);
    }
}

// ── walk_lambda_param ─────────────────────────────────────────────────────────

/// Walk the pattern and optional type annotation of a [`LambdaParam`].
pub fn walk_lambda_param<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, lp: &'ast LambdaParam) {
    match lp {
        LambdaParam::Pattern(p) => v.visit_pattern(p),
        LambdaParam::Annotated { pat, ty, .. } => {
            v.visit_pattern(pat);
            v.visit_type(ty);
        }
    }
}

// ── walk_interp_part ──────────────────────────────────────────────────────────

/// Walk an expression hole in an [`InterpPart`]; text segments are leaves.
pub fn walk_interp_part<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, part: &'ast InterpPart) {
    match part {
        InterpPart::Text { .. } => {}
        InterpPart::Expr { expr, .. } => v.visit_expr(expr),
    }
}

// ── walk_qualified_name ───────────────────────────────────────────────────────

/// Walk all segments of a [`QualifiedName`].
pub fn walk_qualified_name<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, q: &'ast QualifiedName) {
    for seg in &q.segments {
        v.visit_ident(seg);
    }
}

// ── walk_record_ctor ──────────────────────────────────────────────────────────

/// Walk a [`RecordCtor`] (T8, Phase 4 §3.8).
///
/// - `Bare(ident)` → calls [`Visit::visit_ident`] on the identifier.
/// - `Qualified(qn)` → calls [`Visit::visit_qualified_name`] on the path.
pub fn walk_record_ctor<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, ctor: &'ast RecordCtor) {
    match ctor {
        RecordCtor::Bare(id) => v.visit_ident(id),
        RecordCtor::Qualified(qn) => v.visit_qualified_name(qn),
    }
}

// ── Declaration walks ─────────────────────────────────────────────────────────

/// Walk an [`ImportDecl`].
pub fn walk_import_decl<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, d: &'ast ImportDecl) {
    v.visit_module_path(&d.path);
    if let Some(alias) = &d.alias {
        v.visit_ident(alias);
    }
    if let Some(items) = &d.items {
        for item in items {
            v.visit_ident(item);
        }
    }
}

/// Walk a [`ConstDecl`]: type annotation and initialiser expression.
pub fn walk_const_decl<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, d: &'ast ConstDecl) {
    v.visit_ident(&d.name);
    v.visit_type(&d.ty);
    v.visit_expr(&d.value);
}

/// Walk a [`TypeDecl`]: name, type params, and body.
pub fn walk_type_decl<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, d: &'ast TypeDecl) {
    v.visit_ident(&d.name);
    for tp in &d.params {
        v.visit_ident(tp);
    }
    v.visit_type_body(&d.body);
}

/// Walk a [`FnDecl`]: name, params, optional return type, and body.
///
/// For `Body::Expr(e)` the expression is visited normally.
/// For `Body::Ffi { .. }` there is no expression child to visit — the FFI
/// passthrough is a leaf from the visitor's perspective.
pub fn walk_fn_decl<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, d: &'ast FnDecl) {
    use crate::Body;
    v.visit_ident(&d.name);
    for param in &d.params {
        v.visit_param(param);
    }
    if let Some(ret) = &d.ret {
        v.visit_type(ret);
    }
    match &d.body {
        Body::Expr(e) => v.visit_expr(e),
        Body::Ffi { .. } => {
            // FFI passthrough — no expression child to visit.
        }
    }
}

/// Walk an [`ActorDecl`]: name and all member declarations.
pub fn walk_actor_decl<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, d: &'ast ActorDecl) {
    v.visit_ident(&d.name);
    for member in &d.members {
        v.visit_actor_member(member);
    }
}

/// Walk an [`ActorMember`]: dispatches to state, init, on-handler, or mailbox.
pub fn walk_actor_member<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, m: &'ast ActorMember) {
    match m {
        ActorMember::State(s) => v.visit_state_decl(s),
        ActorMember::Init(i) => v.visit_init_decl(i),
        ActorMember::On(h) => v.visit_on_handler(h),
        ActorMember::Mailbox(mb) => v.visit_mailbox_decl(mb),
    }
}

/// Walk a [`MailboxDecl`]: no inner nodes traversed (config is a value type).
pub const fn walk_mailbox_decl<'ast, V: Visit<'ast> + ?Sized>(_v: &mut V, _d: &'ast MailboxDecl) {}

/// Walk a [`StateDecl`]: name, type, and optional default expression.
pub fn walk_state_decl<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, d: &'ast StateDecl) {
    v.visit_ident(&d.name);
    v.visit_type(&d.ty);
    if let Some(default) = &d.default {
        v.visit_expr(default);
    }
}

/// Walk an [`InitDecl`]: params and block body.
pub fn walk_init_decl<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, d: &'ast InitDecl) {
    for param in &d.params {
        v.visit_param(param);
    }
    v.visit_block(&d.body);
}

/// Walk an [`OnHandler`]: name, params, optional return type, and body expression.
pub fn walk_on_handler<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, h: &'ast OnHandler) {
    v.visit_ident(&h.name);
    for param in &h.params {
        v.visit_param(param);
    }
    if let Some(ret) = &h.ret {
        v.visit_type(ret);
    }
    v.visit_expr(&h.body);
}

/// Walk a function [`Param`]: visit the inner identifier and optional type.
pub fn walk_param<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, p: &'ast Param) {
    match p {
        Param::Bare(id) => v.visit_ident(id),
        Param::Annotated { name, ty, .. } => {
            v.visit_ident(name);
            v.visit_type(ty);
        }
    }
}

/// Walk a [`ModulePath`]: visit each path segment identifier.
pub fn walk_module_path<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, mp: &'ast ModulePath) {
    for seg in &mp.segments {
        v.visit_ident(seg);
    }
}

/// Walk a [`TypeBody`]: dispatches to record, union, or alias.
pub fn walk_type_body<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, tb: &'ast TypeBody) {
    match tb {
        TypeBody::Record(rb) => v.visit_record_type_body(rb),
        TypeBody::Union(ub) => v.visit_union_type_body(ub),
        TypeBody::Alias(ty) => v.visit_type(ty),
    }
}

/// Walk a [`RecordTypeBody`]: visit each field declaration.
pub fn walk_record_type_body<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, rb: &'ast RecordTypeBody) {
    for fd in &rb.fields {
        v.visit_field_decl(fd);
    }
}

/// Walk a [`FieldDecl`]: name and type annotation.
pub fn walk_field_decl<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, fd: &'ast FieldDecl) {
    v.visit_ident(&fd.name);
    v.visit_type(&fd.ty);
}

/// Walk a [`UnionTypeBody`]: visit each constructor alternative.
pub fn walk_union_type_body<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, ub: &'ast UnionTypeBody) {
    for alt in &ub.alternatives {
        v.visit_constructor(alt);
    }
}

/// Walk a union [`Constructor`]: name and argument types.
pub fn walk_constructor<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, c: &'ast Constructor) {
    match c {
        Constructor::Positional { name, args, .. } => {
            v.visit_ident(name);
            for arg in args {
                v.visit_type(arg);
            }
        }
        Constructor::Record { name, body, .. } => {
            v.visit_ident(name);
            v.visit_record_type_body(body);
        }
    }
}

/// Walk a [`FnType`]: parameter types and return type.
pub fn walk_fn_type<'ast, V: Visit<'ast> + ?Sized>(v: &mut V, ft: &'ast FnType) {
    for param in &ft.params {
        v.visit_type(param);
    }
    v.visit_type(&ft.ret);
}
