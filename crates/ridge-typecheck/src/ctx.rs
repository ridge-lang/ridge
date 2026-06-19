//! Inference context — per-module mutable state for Algorithm W (T5, T6).
//!
//! [`InferCtx`] owns the two `ena` union-find tables (one for type variables,
//! one for capability-row variables) and provides the core primitives:
//! fresh-variable allocation and shallow resolution.
//!
//! T6 extensions: [`Env`] (scoped type environment), [`Frame`], plus the
//! higher-level fields `env`, `current_caps`, `errors`, and `current_fn_ret`
//! added to [`InferCtx`].

use ena::unify::{InPlaceUnificationTable, NoError, UnifyKey, UnifyValue};
use ridge_resolve::{NodeIdMap, NodeKind};
use ridge_types::{
    AnonRecordTable, CapRow, CapVid, CapabilitySet, Row, RowTail, RowVid, Scheme, TyConDecl,
    TyConId, TyVid, Type,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::error::TypeError;

// ── Value newtypes for ena ────────────────────────────────────────────────────
//
// `ena::UnifyValue` cannot be implemented directly on `Option<Type>` or
// `Option<CapRow>` because neither the trait nor the types are defined in this
// crate (orphan rule). We introduce thin newtype wrappers that are local.

/// Newtype over `Option<Type>` used as the `ena` unification value for type
/// variables.
#[derive(Clone, Debug)]
pub struct TyValue(pub Option<Type>);

/// Newtype over `Option<CapRow>` used as the `ena` unification value for
/// capability-row variables.
#[derive(Clone, Debug)]
pub struct CapValue(pub Option<CapRow>);

/// Newtype over `Option<Row>` used as the `ena` unification value for
/// record-row variables.
#[derive(Clone, Debug)]
pub struct RowValue(pub Option<Row>);

/// [`UnifyValue`] for [`TyValue`].
///
/// The merge rule is intentionally conservative: two `Some` values should
/// never be merged directly — the unifier always calls `union_value` when
/// binding a variable to a concrete type, never unions two bound variables.
/// If somehow two `Some`s land here it indicates a programming error upstream;
/// we propagate the first as a best-effort fallback rather than panicking in
/// release builds.
impl UnifyValue for TyValue {
    type Error = NoError;

    fn unify_values(a: &Self, b: &Self) -> Result<Self, NoError> {
        match (&a.0, &b.0) {
            (None, _) => Ok(b.clone()),
            // Both b-bound or both bound — return the first (a).
            (_, None) | (Some(_), Some(_)) => Ok(a.clone()),
        }
    }
}

/// [`UnifyValue`] for [`CapValue`].
///
/// Same conservative policy as [`TyValue`] above.
impl UnifyValue for CapValue {
    type Error = NoError;

    fn unify_values(a: &Self, b: &Self) -> Result<Self, NoError> {
        match (&a.0, &b.0) {
            (None, _) => Ok(b.clone()),
            (_, None) | (Some(_), Some(_)) => Ok(a.clone()),
        }
    }
}

/// [`UnifyValue`] for [`RowValue`].
///
/// Same conservative policy as [`TyValue`] above: the unifier always peels a
/// tail to an *unbound* row var before binding it, so two `Some`s never legally
/// meet here. If they do it is an upstream bug; keep the first as a fallback.
impl UnifyValue for RowValue {
    type Error = NoError;

    fn unify_values(a: &Self, b: &Self) -> Result<Self, NoError> {
        match (&a.0, &b.0) {
            (None, _) => Ok(b.clone()),
            (_, None) | (Some(_), Some(_)) => Ok(a.clone()),
        }
    }
}

// ── UnifyKey impls ────────────────────────────────────────────────────────────

/// `ena` key wrapping a [`TyVid`] index.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct TyVidKey(pub u32);

impl UnifyKey for TyVidKey {
    type Value = TyValue;

    fn index(&self) -> u32 {
        self.0
    }

    fn from_index(u: u32) -> Self {
        Self(u)
    }

    fn tag() -> &'static str {
        "TyVidKey"
    }
}

/// `ena` key wrapping a [`CapVid`] index.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct CapVidKey(pub u32);

impl UnifyKey for CapVidKey {
    type Value = CapValue;

    fn index(&self) -> u32 {
        self.0
    }

    fn from_index(u: u32) -> Self {
        Self(u)
    }

    fn tag() -> &'static str {
        "CapVidKey"
    }
}

/// `ena` key wrapping a [`RowVid`] index.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct RowVidKey(pub u32);

impl UnifyKey for RowVidKey {
    type Value = RowValue;

    fn index(&self) -> u32 {
        self.0
    }

    fn from_index(u: u32) -> Self {
        Self(u)
    }

    fn tag() -> &'static str {
        "RowVidKey"
    }
}

// ── Env / Frame ───────────────────────────────────────────────────────────────

/// A single scope frame in the lexical environment.
///
/// Each frame maps a local name to the `Scheme` that was bound in that scope.
/// Frames are pushed on function-body entry, `let`-binding, match-arm entry,
/// etc., and popped on scope exit.
#[derive(Debug, Default, Clone)]
pub struct Frame {
    /// Name → scheme bindings for this scope.
    pub bindings: FxHashMap<String, Scheme>,
}

/// Lexically-scoped type environment: a stack of [`Frame`]s.
///
/// The innermost (top) frame is checked first, mirroring Phase 3's `ScopeStack`.
#[derive(Debug, Default, Clone)]
pub struct Env {
    /// Stack of frames; the last element is the innermost (active) scope.
    pub frames: Vec<Frame>,
}

impl Env {
    /// Creates an empty environment.
    #[must_use]
    pub const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    /// Pushes a new, empty scope frame.
    pub fn push_frame(&mut self) {
        self.frames.push(Frame::default());
    }

    /// Pops the innermost scope frame.
    ///
    /// # Panics (debug only)
    ///
    /// Panics in debug builds if the frame stack is already empty.
    pub fn pop_frame(&mut self) {
        debug_assert!(!self.frames.is_empty(), "Env::pop_frame on empty stack");
        self.frames.pop();
    }

    /// Inserts a binding in the innermost frame.
    ///
    /// # Panics (debug only)
    ///
    /// Panics in debug builds if there is no active frame.
    pub fn bind(&mut self, name: String, scheme: Scheme) {
        debug_assert!(
            !self.frames.is_empty(),
            "Env::bind called with no active frame"
        );
        if let Some(frame) = self.frames.last_mut() {
            frame.bindings.insert(name, scheme);
        }
    }

    /// Looks up a name in the environment, searching from the innermost frame
    /// outward (standard lexical scoping).
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&Scheme> {
        for frame in self.frames.iter().rev() {
            if let Some(s) = frame.bindings.get(name) {
                return Some(s);
            }
        }
        None
    }
}

// ── InferCtx ──────────────────────────────────────────────────────────────────

/// Per-module mutable inference state.
///
/// Owns the two `ena` union-find tables plus the higher-level Algorithm-W
/// state introduced in T6: lexical type environment, capability tracking,
/// error accumulator, and the enclosing-function return type.
pub struct InferCtx {
    /// Union-find table for type unification variables.
    pub tyvids: InPlaceUnificationTable<TyVidKey>,
    /// Union-find table for capability-row variables.
    pub capvids: InPlaceUnificationTable<CapVidKey>,
    /// Union-find table for record-row variables (the open tail of a
    /// [`Type::Record`]).
    pub rowvids: InPlaceUnificationTable<RowVidKey>,

    // ── T6 additions ─────────────────────────────────────────────────────────
    /// Lexically-scoped type environment (name → Scheme).
    pub env: Env,
    /// Capability set inferred for the *current* enclosing function.
    ///
    /// T6 uses `CapabilitySet::PURE` as the placeholder (caps inference is T13).
    /// The value is snapshotted when building a `Type::Fn` for a lambda.
    pub current_caps: CapabilitySet,
    /// Accumulated `T###` diagnostics for the current module.
    pub errors: Vec<TypeError>,
    /// The declared/inferred return type of the innermost enclosing function.
    ///
    /// Set to `Some(ty)` when entering an fn-body; reset to the outer value
    /// on exit.  Used by `Expr::Return` (verbatim return — unify the
    /// return value's type with this, not with `Result`/`Option`).
    pub current_fn_ret: Option<Type>,

    /// The propagation target type for `?` operators (T10).
    ///
    /// - Inside a `try { … }` block, this is set to the try-expression's
    ///   synthesised `Result a e` type, overriding `current_fn_ret` for `?`.
    /// - Outside any `try`, this is `None` and `infer_propagate` falls back to
    ///   `current_fn_ret`.
    /// - Saved and restored on `try`-block entry/exit so nesting works correctly.
    pub current_propagate_target: Option<Type>,

    /// User-defined `TyCon` name → `TyConId` map (T17 pipeline wiring).
    ///
    /// Populated by `tycon_collect::collect_user_tycons` before type inference
    /// begins.  Used by `ast_type_to_type` (in `infer.rs`) to resolve named
    /// types like `Level`, `LogEntry`, etc.
    pub user_tycon_names: FxHashMap<String, TyConId>,

    /// Snapshot of all `TyConDecls` in the arena at the start of module inference
    /// (T17 pipeline wiring).
    ///
    /// Used by `infer_expr` (and `records.rs`) to look up record schemas and
    /// union variant lists at construction/access/pattern sites.
    /// Populated from the arena before `typecheck_module_decls` is called.
    pub tycon_decls: Vec<TyConDecl>,

    // ── Phase 4.5 T3/T4/T5 additions ─────────────────────────────────────────
    /// Optional reference to the module's `NodeIdMap`, set before inference
    /// begins.  When `Some`, `infer_expr_outer` writes each resolved `Type`
    /// to `node_types_accum` indexed by the expression's `NodeId`.
    ///
    /// `None` when running in contexts without a node-id map (tests, LSP
    /// incremental re-check without prior resolution pass).
    // OQ-PHASE45-005: span-keyed lookup kept; no NodeId fields on FnDecl/ConstDecl.
    pub node_id_map: Option<NodeIdMap>,

    /// Accumulator for per-expression types written back by `infer_expr_outer`.
    ///
    /// Indexed by `NodeId.0`; slots below the first written entry are `None`.
    /// Moved into `TypedModule.node_types` at module-end.
    pub node_types_accum: Vec<Option<Type>>,

    /// Accumulator for generalised top-level decl schemes written back by the
    /// SCC pass (`typecheck_module_decls`).
    ///
    /// Keyed by `NodeId` (the decl's own node id looked up via
    /// `node_id_map.get(decl.span, NodeKind::Expr)`).
    /// Moved into `TypedModule.schemes` at module-end.
    // OQ-PHASE45-003: top-level decl schemes only; let-bound locals excluded.
    pub schemes_accum: FxHashMap<ridge_resolve::NodeId, Scheme>,

    /// Map from anonymous record shape to the [`ridge_types::TyConId`] interned
    /// for it during the collect pre-scan.
    ///
    /// Populated by `tycon_collect::prescan_inline_records` BEFORE inference
    /// begins.  Read by `ast_type_to_ridge_type` and the `Expr::RecordLit` /
    /// `Pattern::Record` inference arms (T5).
    pub anon_records: AnonRecordTable,

    /// Records whose `Row` instance was demanded while discharging constraints
    /// in this module.
    ///
    /// A structurally-synthesised (implicit) `Row` instance is registered in
    /// `InstanceEnv` for every eligible record, but its dictionary IR is only
    /// emitted for records that are actually used as rows. The solver records
    /// each demanded record's `TyConId` here as it discharges a `Row`
    /// constraint; the workspace driver unions these across modules and moves
    /// the matching stashed instances into the emitted set.
    pub demanded_rows: FxHashSet<TyConId>,

    // ── Constraint solving (0.2.13 typeclasses) ──────────────────────────────
    /// Class constraints deferred during inference, waiting to be solved.
    ///
    /// When a constrained scheme is instantiated, each of its constraints is
    /// remapped through the same fresh-`TyVid` substitution used for the type
    /// variables and pushed here. After all bodies in an SCC are inferred,
    /// [`crate::solve::solve_constraints`] drains this list and either
    /// discharges each constraint (concrete type → instance lookup), retains
    /// it for generalisation (still-polymorphic variable), or reports an error
    /// (ambiguous / missing instance).
    ///
    /// Pre-typeclass code never adds to this list; the solver is a no-op when
    /// it is empty, so unconstrained modules are completely unaffected.
    pub deferred_constraints: Vec<ridge_types::Constraint>,

    /// Per-constraint dictionary resolution plan accumulated across all SCCs.
    ///
    /// Each SCC's solver run merges its `DictResolution` into this map.
    /// Moved into `TypedModule.dict_resolution` at module-end.
    ///
    /// Empty for modules with no constrained functions (the common pre-typeclass
    /// case) — reading it is always safe.
    pub dict_resolution_accum: crate::solve::DictResolution,

    /// Set of `TyConId`s that have a registered `ToText` instance.
    ///
    /// Populated from the workspace instance registry before per-body inference
    /// begins (set by the SCC pass). Used by interpolation-hole type-checking to
    /// perform an O(1) membership test instead of the old built-in closed-set
    /// comparison.
    ///
    /// `None` in unit-test scaffolding that bypasses the full pipeline; the
    /// interpolation pass falls back to the built-in closed set in that case.
    pub to_text_tycons: Option<rustc_hash::FxHashSet<TyConId>>,

    /// Raw `ModuleId` of the module currently being inferred.
    ///
    /// Compared against [`ridge_types::TyConDecl::def_module_raw`] to enforce the
    /// opaque-type field boundary (T036): field access (`.field`) and
    /// `with`-updates of an opaque type are rejected outside the module that
    /// declares it. `None` in unit-test scaffolding that bypasses the per-module
    /// driver — the gate then no-ops.
    pub current_module_raw: Option<u32>,

    /// Top-level `fn`/`const` schemes generalised for this module, keyed by name.
    ///
    /// Captured as each declaration's scheme is written back so the workspace
    /// driver can expose them to importing modules (cross-module value seeding).
    /// Mirrors `schemes_accum` but keyed by name rather than body `NodeId`.
    pub name_schemes_accum: FxHashMap<String, Scheme>,

    /// Quoted lambdas discovered during inference, keyed by the lambda's span.
    ///
    /// Populated by [`crate::quote::check_quote`] when a lambda flows into a
    /// `Quote` parameter. Moved into `TypedModule.quoted_lambdas` at module-end
    /// so the lowering pass knows which lambda bodies to reify into `QExpr`
    /// trees. Empty for any module that uses no quotation.
    pub quoted_lambdas_accum: FxHashMap<ridge_ast::Span, crate::quote::QuoteInfo>,

    /// Reconciled tycon ids the `Rows q` projection reduces against — the decode
    /// terminals' receivers (`Query`/`Join`/`LeftJoin`) and `Option`.
    ///
    /// `Rows q` reduces to the row a receiver decodes into (`Rows (Query e a)` to
    /// `e`, `Rows (Join e f a)` to `(e, f)`, `Rows (LeftJoin e f a)` to
    /// `(e, Option f)`), so the reduction in [`crate::unify`] and [`Self::deep_resolve`]
    /// must recognise those receivers by id. Populated once per module from the
    /// reconciled stdlib names; `None` in scaffolding (or a workspace without the
    /// query builder), where the projection simply stays stuck.
    pub rows_tycons: Option<RowsTycons>,
}

/// The reconciled tycon ids the `Rows q` projection reduces against. `Query`,
/// `Join`, and `LeftJoin` are the decode terminals' receivers; `option` builds a
/// left join's optional right side (`Option f`).
#[derive(Debug, Clone, Copy)]
pub struct RowsTycons {
    /// `Query`'s reconciled tycon id — `Rows (Query e a)` reduces to `e`.
    pub query: TyConId,
    /// `Join`'s reconciled tycon id — `Rows (Join e f a)` reduces to `(e, f)`.
    pub join: TyConId,
    /// `LeftJoin`'s reconciled tycon id — `Rows (LeftJoin e f a)` reduces to
    /// `(e, Option f)`.
    pub left_join: TyConId,
    /// `RightJoin`'s reconciled tycon id — `Rows (RightJoin e f a)` reduces to
    /// `(Option e, f)`.
    pub right_join: TyConId,
    /// `FullJoin`'s reconciled tycon id — `Rows (FullJoin e f a)` reduces to
    /// `(Option e, Option f)`.
    pub full_join: TyConId,
    /// `Joined`'s reconciled tycon id — the nested N-ary inner join. `Rows
    /// (Joined q f a)` reduces to `(Rows q, f)`, and the `JoinCond`/`JoinResult`
    /// projections key on it for a composite receiver. `None` until std.repo's
    /// `Joined` is reconciled (a workspace without the N-ary builder leaves the
    /// projections stuck on it).
    pub joined: Option<TyConId>,
    /// `LeftJoined`'s reconciled tycon id — the nested N-ary LEFT outer join.
    /// `Rows (LeftJoined q f a)` reduces to `(Rows q, Option f)` — the composite's
    /// own row paired with the newly left-joined entity made optional. The
    /// `JoinCond`/`LeftJoinResult`/`Rows` projections key on it. `None` until
    /// std.repo's `LeftJoined` is reconciled.
    pub left_joined: Option<TyConId>,
    /// `RightJoined`'s reconciled tycon id — the nested N-ary RIGHT outer join.
    /// `Rows (RightJoined q f a)` reduces to `(Option (Rows q), f)` — the composite's
    /// own row made optional as a unit, paired with the always-present new entity.
    /// The `JoinCond`/`RightJoinResult`/`Rows` projections key on it. `None` until
    /// std.repo's `RightJoined` is reconciled.
    pub right_joined: Option<TyConId>,
    /// `FullJoined`'s reconciled tycon id — the nested N-ary FULL outer join.
    /// `Rows (FullJoined q f a)` reduces to `(Option (Rows q), Option f)` — both the
    /// composite (optional as a unit) and the new entity optional. The
    /// `JoinCond`/`FullJoinResult`/`Rows` projections key on it. `None` until
    /// std.repo's `FullJoined` is reconciled.
    pub full_joined: Option<TyConId>,
    /// `Option`'s tycon id, wrapping an outer join's nullable side.
    pub option: TyConId,
    /// `Bool`'s tycon id, the result of a `JoinCond` condition. Carried here so
    /// the projection reductions can build `… -> Bool` without a `BuiltinTyCons`
    /// handle (the context holds none).
    pub bool: TyConId,
}

impl InferCtx {
    /// Creates a fresh, empty inference context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tyvids: InPlaceUnificationTable::new(),
            capvids: InPlaceUnificationTable::new(),
            rowvids: InPlaceUnificationTable::new(),
            env: Env::new(),
            current_caps: CapabilitySet::PURE,
            errors: Vec::new(),
            current_fn_ret: None,
            current_propagate_target: None,
            user_tycon_names: FxHashMap::default(),
            tycon_decls: Vec::new(),
            node_id_map: None,
            node_types_accum: Vec::new(),
            schemes_accum: FxHashMap::default(),
            anon_records: AnonRecordTable::default(),
            demanded_rows: FxHashSet::default(),
            deferred_constraints: Vec::new(),
            dict_resolution_accum: rustc_hash::FxHashMap::default(),
            to_text_tycons: None,
            current_module_raw: None,
            name_schemes_accum: FxHashMap::default(),
            quoted_lambdas_accum: FxHashMap::default(),
            rows_tycons: None,
        }
    }

    /// Populate the `ToText` instance set from the workspace instance registry.
    ///
    /// Called by the SCC pass before per-body inference so that
    /// interpolation-hole type-checking can query the set with O(1) membership
    /// tests. Must be called with the same `InstanceEnv` that will remain
    /// valid for the lifetime of this context.
    pub fn set_to_text_instances(&mut self, env: &crate::class_env::InstanceEnv) {
        use ridge_types::TOTEXT_CLASS;
        let set: rustc_hash::FxHashSet<TyConId> = env
            .instances
            .keys()
            .filter_map(|(class, head)| {
                if *class == TOTEXT_CLASS {
                    head.first().copied()
                } else {
                    None
                }
            })
            .collect();
        self.to_text_tycons = Some(set);
    }

    /// Returns `true` when `tycon_id` has a registered `ToText` instance.
    ///
    /// When the set is not populated (unit-test scaffolding without the full
    /// pipeline), returns `None` so callers can fall back to the built-in
    /// closed set.
    #[must_use]
    pub fn has_to_text(&self, tycon_id: TyConId) -> Option<bool> {
        self.to_text_tycons.as_ref().map(|s| s.contains(&tycon_id))
    }

    /// Write back the shallow-resolved type for an expression position to
    /// `node_types_accum`, keyed by the `NodeId` for `(span, kind)`.
    ///
    /// This is the single write-back helper used by `infer_expr_outer` (T3)
    /// and `infer_block` / `infer_try` (T3/OQ-PHASE45-004).
    ///
    /// No-op when `node_id_map` is `None` or the span is not stamped.
    // OQ-PHASE45-004: shared helper for Expr, Block, and Try write-back.
    pub fn write_node_type(&mut self, span: ridge_ast::Span, kind: NodeKind, ty: &Type) {
        let nid = match &self.node_id_map {
            Some(map) => match map.get(span, kind) {
                Some(id) => id,
                None => return,
            },
            None => return,
        };
        let idx = nid.0 as usize;
        if self.node_types_accum.len() <= idx {
            self.node_types_accum.resize(idx + 1, None);
        }
        let resolved = self.shallow_resolve(ty);
        self.node_types_accum[idx] = Some(resolved);
    }

    /// Allocates a fresh type unification variable and returns it as a [`TyVid`].
    pub fn fresh_tyvid(&mut self) -> TyVid {
        let key = self.tyvids.new_key(TyValue(None));
        TyVid(key.0)
    }

    /// Allocates a fresh capability-row variable and returns it as a [`CapVid`].
    pub fn fresh_capvid(&mut self) -> CapVid {
        let key = self.capvids.new_key(CapValue(None));
        CapVid(key.0)
    }

    /// Allocates a fresh record-row variable and returns it as a [`RowVid`].
    pub fn fresh_rowvid(&mut self) -> RowVid {
        let key = self.rowvids.new_key(RowValue(None));
        RowVid(key.0)
    }

    /// Peels bound row variables off a record row, gathering every known field.
    ///
    /// Walks the tail while it is a *bound* row var: each bound row's fields are
    /// appended and its own tail followed, until the tail is `Closed` or an
    /// *unbound* row var. The returned tail's row var (if any) is canonicalised
    /// to its union-find root. Field *types* are not resolved here — the unifier
    /// unifies them itself; this only exposes the field set for the label split.
    #[must_use]
    pub fn resolve_row(
        &mut self,
        fields: &[(String, Type)],
        tail: &RowTail,
    ) -> (Vec<(String, Type)>, RowTail) {
        let mut out: Vec<(String, Type)> = fields.to_vec();
        let mut cur = tail.clone();
        while let RowTail::Open(rv) = cur.clone() {
            let root = self.rowvids.find(RowVidKey(rv.0));
            let Some(row) = self.rowvids.probe_value(root).0 else {
                // Unbound row var — canonicalise to its root and stop.
                cur = RowTail::Open(RowVid(root.0));
                break;
            };
            out.extend(row.fields.iter().cloned());
            cur = row.tail;
        }
        (out, cur)
    }

    /// Shallow-resolves a type:
    ///
    /// - `Type::Var(v)` — looks up `v` in the unification table. If bound,
    ///   recursively shallow-resolves the bound type (one step of path
    ///   compression). If unbound, returns `Type::Var(v')` with the canonical
    ///   representative.
    /// - `Type::Alias { body, .. }` — **transparently peeks through** the
    ///   alias: returns `shallow_resolve(*body)`. Aliases are
    ///   never structural; they exist only for rendering.
    /// - All other variants — returned as-is.
    #[must_use]
    pub fn shallow_resolve(&mut self, t: &Type) -> Type {
        match t {
            Type::Var(v) => {
                let key = TyVidKey(v.0);
                let root = self.tyvids.find(key);
                // Probe for a bound value at the root.
                match self.tyvids.probe_value(root).0 {
                    Some(bound) => self.shallow_resolve(&bound),
                    None => Type::Var(TyVid(root.0)),
                }
            }
            // Alias is transparent — resolve the body.
            Type::Alias { body, .. } => self.shallow_resolve(body),
            other => other.clone(),
        }
    }

    /// Reduce `Rows q`'s already-resolved argument `q` to the row its receiver
    /// decodes into: `Rows (Query e a)` to `e`, `Rows (Join e f a)` to `(e, f)`,
    /// `Rows (LeftJoin e f a)` to `(e, Option f)`, `Rows (RightJoin e f a)` to
    /// `(Option e, f)`, and `Rows (FullJoin e f a)` to `(Option e, Option f)`.
    /// Returns `None` when `q` is not one of those receivers (or still a variable, or
    /// the receiver ids are unknown), leaving the projection stuck.
    #[must_use]
    pub fn reduce_rows_arg(&self, q: &Type) -> Option<Type> {
        let rt = self.rows_tycons?;
        let Type::Con(qid, qargs) = q else {
            return None;
        };
        if *qid == rt.query {
            return qargs.first().cloned();
        }
        if *qid == rt.join {
            let e = qargs.first()?.clone();
            let f = qargs.get(1)?.clone();
            return Some(Type::Tuple(vec![e, f]));
        }
        if *qid == rt.left_join {
            let e = qargs.first()?.clone();
            let f = qargs.get(1)?.clone();
            return Some(Type::Tuple(vec![e, Type::Con(rt.option, vec![f])]));
        }
        if *qid == rt.right_join {
            let e = qargs.first()?.clone();
            let f = qargs.get(1)?.clone();
            return Some(Type::Tuple(vec![Type::Con(rt.option, vec![e]), f]));
        }
        if *qid == rt.full_join {
            let e = qargs.first()?.clone();
            let f = qargs.get(1)?.clone();
            return Some(Type::Tuple(vec![
                Type::Con(rt.option, vec![e]),
                Type::Con(rt.option, vec![f]),
            ]));
        }
        // `Rows (Joined q' f a)` reduces to `(Rows q', f)` — the left composite's
        // own row paired with the newly joined entity, nesting recursively: a
        // depth-3 inner join `Joined (Join e f a) g a` decodes to `((e, f), g)`.
        // The inner `Rows q'` is reduced here so the result is fully normalised.
        if rt.joined == Some(*qid) {
            let q_inner = qargs.first()?;
            let f = qargs.get(1)?.clone();
            let inner_rows = self.reduce_rows_arg(q_inner)?;
            return Some(Type::Tuple(vec![inner_rows, f]));
        }
        // `Rows (LeftJoined q' f a)` reduces to `(Rows q', Option f)` — the left
        // composite's own row paired with the newly left-joined entity made
        // optional, since a composite row that matched no new row keeps it `None`.
        // The composite itself is always present (a left join keeps every left row),
        // so only the new entity is wrapped.
        if rt.left_joined == Some(*qid) {
            let q_inner = qargs.first()?;
            let f = qargs.get(1)?.clone();
            let inner_rows = self.reduce_rows_arg(q_inner)?;
            return Some(Type::Tuple(vec![inner_rows, Type::Con(rt.option, vec![f])]));
        }
        // `Rows (RightJoined q' f a)` reduces to `(Option (Rows q'), f)` — the left
        // composite's own row made optional AS A UNIT (a right join null-extends the
        // whole composite when a new row matched none), paired with the new entity a
        // right join always keeps.
        if rt.right_joined == Some(*qid) {
            let q_inner = qargs.first()?;
            let f = qargs.get(1)?.clone();
            let inner_rows = self.reduce_rows_arg(q_inner)?;
            return Some(Type::Tuple(vec![Type::Con(rt.option, vec![inner_rows]), f]));
        }
        // `Rows (FullJoined q' f a)` reduces to `(Option (Rows q'), Option f)` — a full
        // join keeps every row of both sides and null-extends whichever matched none,
        // so both the composite (as a unit) and the new entity are optional.
        if rt.full_joined == Some(*qid) {
            let q_inner = qargs.first()?;
            let f = qargs.get(1)?.clone();
            let inner_rows = self.reduce_rows_arg(q_inner)?;
            return Some(Type::Tuple(vec![
                Type::Con(rt.option, vec![inner_rows]),
                Type::Con(rt.option, vec![f]),
            ]));
        }
        None
    }

    /// The ordered leaf entities a join receiver `q` carries, left to right — the
    /// list a `JoinCond q f` condition ranges over before the new right entity.
    /// `Query e a` carries `[e]`, a binary `Join e g a` carries `[e, g]`, and a
    /// nested `Joined q' g a` carries `q'`'s entities followed by `g`. Returns
    /// `None` when `q` is not a recognised inner-join receiver (or still a
    /// variable), leaving the projection stuck.
    pub(crate) fn join_entities(&self, q: &Type) -> Option<Vec<Type>> {
        let rt = self.rows_tycons?;
        let Type::Con(qid, qargs) = q else {
            return None;
        };
        if *qid == rt.query {
            return Some(vec![qargs.first()?.clone()]);
        }
        if *qid == rt.join {
            let e = qargs.first()?.clone();
            let g = qargs.get(1)?.clone();
            return Some(vec![e, g]);
        }
        // A nested receiver — inner `Joined` or the outer `LeftJoined` — carries its
        // composite's entities followed by the newly joined one. The condition ranges
        // over every leaf positionally, present or null-extended, so a left join's
        // optional new entity still contributes its (plain) entity to the leaf list.
        if rt.joined == Some(*qid)
            || rt.left_joined == Some(*qid)
            || rt.right_joined == Some(*qid)
            || rt.full_joined == Some(*qid)
        {
            let q_inner = qargs.first()?;
            let g = qargs.get(1)?.clone();
            let mut es = self.join_entities(q_inner)?;
            es.push(g);
            return Some(es);
        }
        None
    }

    /// Whether `tycon` is one of the nested-join composite constructors
    /// (`Joined`/`LeftJoined`/`RightJoined`/`FullJoined`). These are the
    /// recursively-nested receivers whose terminal predicate ranges over an
    /// unbounded number of leaves, so a fundep terminal class keys its instance
    /// (and dictionary) by the receiver alone rather than by the predicate's
    /// per-depth arity. `false` until the rows-tycon reconciliation has run.
    #[must_use]
    pub(crate) fn is_composite_join_tycon(&self, tycon: TyConId) -> bool {
        let Some(rt) = self.rows_tycons else {
            return false;
        };
        rt.joined == Some(tycon)
            || rt.left_joined == Some(tycon)
            || rt.right_joined == Some(tycon)
            || rt.full_joined == Some(tycon)
    }

    /// Whether `q` is a nested-join composite receiver — the type-level dual of
    /// [`Self::is_composite_join_tycon`], reading the head constructor of `q`.
    #[must_use]
    pub(crate) fn is_composite_join_receiver(&self, q: &Type) -> bool {
        matches!(q, Type::Con(id, _) if self.is_composite_join_tycon(*id))
    }

    /// The adapter `a` a join receiver `q` threads — the last type argument of
    /// `Query e a`, and the third of a binary `Join e g a` or a nested
    /// `Joined q' g a`. Used to rebuild the receiver-determined result type.
    fn join_adapter(&self, q: &Type) -> Option<Type> {
        let rt = self.rows_tycons?;
        let Type::Con(qid, qargs) = q else {
            return None;
        };
        if *qid == rt.query {
            return qargs.get(1).cloned();
        }
        if *qid == rt.join
            || rt.joined == Some(*qid)
            || rt.left_joined == Some(*qid)
            || rt.right_joined == Some(*qid)
            || rt.full_joined == Some(*qid)
        {
            return qargs.get(2).cloned();
        }
        None
    }

    /// Reduces `JoinCond q f` to the curried condition a `joinOn` over receiver
    /// `q` adding right entity `f` accepts: the receiver's leaf entities followed
    /// by `f`, returning `Bool`. `JoinCond (Query e a) f` becomes `e -> f -> Bool`,
    /// `JoinCond (Join e g a) f` becomes `e -> g -> f -> Bool`. Returns `None`
    /// while `q` is not yet a recognised receiver, leaving the projection stuck.
    #[must_use]
    pub fn reduce_joincond_arg(&self, q: &Type, f: &Type) -> Option<Type> {
        let rt = self.rows_tycons?;
        let mut params = self.join_entities(q)?;
        params.push(f.clone());
        Some(Type::Fn {
            params,
            ret: Box::new(Type::Con(rt.bool, vec![])),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        })
    }

    /// Reduces `JoinResult q f` to the type a `joinOn` over receiver `q` adding
    /// right entity `f` produces: the binary `Join e f a` from a `Query e a` (the
    /// depth-2 inner join keeps its existing vocabulary), and the nested
    /// `Joined q f a` from any composite receiver (`Join`/`Joined`). Returns
    /// `None` while `q` is not yet a recognised receiver, or when `Joined` is not
    /// reconciled in this workspace.
    #[must_use]
    pub fn reduce_joinresult_arg(&self, q: &Type, f: &Type) -> Option<Type> {
        let rt = self.rows_tycons?;
        let Type::Con(qid, _) = q else {
            return None;
        };
        let a = self.join_adapter(q)?;
        if *qid == rt.query {
            let e = match q {
                Type::Con(_, args) => args.first()?.clone(),
                _ => return None,
            };
            return Some(Type::Con(rt.join, vec![e, f.clone(), a]));
        }
        if *qid == rt.join
            || rt.joined == Some(*qid)
            || rt.left_joined == Some(*qid)
            || rt.right_joined == Some(*qid)
            || rt.full_joined == Some(*qid)
        {
            let joined = rt.joined?;
            return Some(Type::Con(joined, vec![q.clone(), f.clone(), a]));
        }
        None
    }

    /// Reduces `LeftJoinResult q f` to the type a `leftJoinOn` over receiver `q`
    /// adding right entity `f` produces: the binary `LeftJoin e f a` from a
    /// `Query e a`, and the nested `LeftJoined q f a` from any composite receiver
    /// (`Join`/`Joined`/`LeftJoined`). The dual of [`Self::reduce_joinresult_arg`]
    /// for the left outer verb. Returns `None` while `q` is not yet a recognised
    /// receiver, or when `LeftJoined` is not reconciled in this workspace.
    #[must_use]
    pub fn reduce_leftjoinresult_arg(&self, q: &Type, f: &Type) -> Option<Type> {
        let rt = self.rows_tycons?;
        let Type::Con(qid, _) = q else {
            return None;
        };
        let a = self.join_adapter(q)?;
        if *qid == rt.query {
            let e = match q {
                Type::Con(_, args) => args.first()?.clone(),
                _ => return None,
            };
            return Some(Type::Con(rt.left_join, vec![e, f.clone(), a]));
        }
        if *qid == rt.join
            || rt.joined == Some(*qid)
            || rt.left_joined == Some(*qid)
            || rt.right_joined == Some(*qid)
            || rt.full_joined == Some(*qid)
        {
            let left_joined = rt.left_joined?;
            return Some(Type::Con(left_joined, vec![q.clone(), f.clone(), a]));
        }
        None
    }

    /// Reduces `RightJoinResult q f` to the type a `rightJoinOn` over receiver `q`
    /// adding right entity `f` produces: the binary `RightJoin e f a` from a
    /// `Query e a`, and the nested `RightJoined q f a` from any composite receiver.
    /// The RIGHT dual of [`Self::reduce_leftjoinresult_arg`]. Returns `None` while
    /// `q` is not a recognised receiver, or when `RightJoined` is not reconciled.
    #[must_use]
    pub fn reduce_rightjoinresult_arg(&self, q: &Type, f: &Type) -> Option<Type> {
        let rt = self.rows_tycons?;
        let Type::Con(qid, _) = q else {
            return None;
        };
        let a = self.join_adapter(q)?;
        if *qid == rt.query {
            let e = match q {
                Type::Con(_, args) => args.first()?.clone(),
                _ => return None,
            };
            return Some(Type::Con(rt.right_join, vec![e, f.clone(), a]));
        }
        if *qid == rt.join
            || rt.joined == Some(*qid)
            || rt.left_joined == Some(*qid)
            || rt.right_joined == Some(*qid)
            || rt.full_joined == Some(*qid)
        {
            let right_joined = rt.right_joined?;
            return Some(Type::Con(right_joined, vec![q.clone(), f.clone(), a]));
        }
        None
    }

    /// Reduces `FullJoinResult q f` to the type a `fullJoinOn` over receiver `q`
    /// adding right entity `f` produces: the binary `FullJoin e f a` from a
    /// `Query e a`, and the nested `FullJoined q f a` from any composite receiver.
    /// The FULL dual of [`Self::reduce_rightjoinresult_arg`]. Returns `None` while
    /// `q` is not a recognised receiver, or when `FullJoined` is not reconciled.
    #[must_use]
    pub fn reduce_fulljoinresult_arg(&self, q: &Type, f: &Type) -> Option<Type> {
        let rt = self.rows_tycons?;
        let Type::Con(qid, _) = q else {
            return None;
        };
        let a = self.join_adapter(q)?;
        if *qid == rt.query {
            let e = match q {
                Type::Con(_, args) => args.first()?.clone(),
                _ => return None,
            };
            return Some(Type::Con(rt.full_join, vec![e, f.clone(), a]));
        }
        if *qid == rt.join
            || rt.joined == Some(*qid)
            || rt.left_joined == Some(*qid)
            || rt.right_joined == Some(*qid)
            || rt.full_joined == Some(*qid)
        {
            let full_joined = rt.full_joined?;
            return Some(Type::Con(full_joined, vec![q.clone(), f.clone(), a]));
        }
        None
    }

    /// Deep-resolves a type: like [`Self::shallow_resolve`] but walks recursively into
    /// all sub-terms.  Every [`Type::Var`] encountered is replaced by its
    /// shallow-resolved representative (or left as a free var if unbound).
    /// [`Type::Alias`] bodies are walked but the alias wrapper is preserved so
    /// that diagnostic names survive.
    #[must_use]
    pub fn deep_resolve(&mut self, t: &Type) -> Type {
        match t {
            // Var: shallow-resolve first; if that produced another Var it is free.
            Type::Var(_) => {
                let resolved = self.shallow_resolve(t);
                match &resolved {
                    Type::Var(_) => resolved,
                    other => self.deep_resolve(other),
                }
            }
            Type::Con(id, args) => {
                let new_args: Vec<Type> = args.iter().map(|a| self.deep_resolve(a)).collect();
                // `Ret (fn … -> r)` normalises to `r`. The argument is already
                // deep-resolved, so its return is fully resolved too. While the
                // argument is still a variable (`Ret ?p`), the projection is left
                // intact — it reduces once `p` is pinned to a function type.
                if id.0 == ridge_types::RET_TYCON_ID && new_args.len() == 1 {
                    if let Type::Fn { ret, .. } = &new_args[0] {
                        return (**ret).clone();
                    }
                }
                // `Rows q` normalises to the row its receiver decodes into — `e`
                // for a query, `(e, f)` for an inner join, `(e, Option f)` for a
                // left join. The argument is already deep-resolved, so reducing a
                // query exposes its (possibly still-free) entity for the declared
                // result type to pin. While the argument is a variable (`Rows ?q`),
                // the projection is left intact, reducing once `q` is a receiver.
                if id.0 == ridge_types::ROWS_TYCON_ID && new_args.len() == 1 {
                    if let Some(reduced) = self.reduce_rows_arg(&new_args[0]) {
                        return reduced;
                    }
                }
                // `JoinCond q f` normalises to the curried condition `joinOn`
                // accepts over `q` and the new right entity `f` — the receiver's
                // leaf entities then `f`, returning `Bool`. While `q` is a variable
                // the projection is left intact, reducing once `q` is a receiver.
                if id.0 == ridge_types::JOINCOND_TYCON_ID && new_args.len() == 2 {
                    if let Some(reduced) = self.reduce_joincond_arg(&new_args[0], &new_args[1]) {
                        return reduced;
                    }
                }
                // `JoinResult q f` normalises to the type `joinOn` produces — a
                // binary `Join` from a query, the nested `Joined` from a composite.
                // Stuck while `q` is a variable or `Joined` is unreconciled.
                if id.0 == ridge_types::JOINRESULT_TYCON_ID && new_args.len() == 2 {
                    if let Some(reduced) = self.reduce_joinresult_arg(&new_args[0], &new_args[1]) {
                        return reduced;
                    }
                }
                // `LeftJoinResult q f` normalises to the type `leftJoinOn` produces —
                // a binary `LeftJoin` from a query, the nested `LeftJoined` from a
                // composite. Stuck while `q` is a variable or `LeftJoined` is
                // unreconciled.
                if id.0 == ridge_types::LEFTJOINRESULT_TYCON_ID && new_args.len() == 2 {
                    if let Some(reduced) =
                        self.reduce_leftjoinresult_arg(&new_args[0], &new_args[1])
                    {
                        return reduced;
                    }
                }
                // `RightJoinResult q f` normalises to the type `rightJoinOn` produces —
                // a binary `RightJoin` from a query, the nested `RightJoined` from a
                // composite. Stuck while `q` is a variable or `RightJoined` is
                // unreconciled.
                if id.0 == ridge_types::RIGHTJOINRESULT_TYCON_ID && new_args.len() == 2 {
                    if let Some(reduced) =
                        self.reduce_rightjoinresult_arg(&new_args[0], &new_args[1])
                    {
                        return reduced;
                    }
                }
                // `FullJoinResult q f` normalises to the type `fullJoinOn` produces —
                // a binary `FullJoin` from a query, the nested `FullJoined` from a
                // composite. Stuck while `q` is a variable or `FullJoined` is
                // unreconciled.
                if id.0 == ridge_types::FULLJOINRESULT_TYCON_ID && new_args.len() == 2 {
                    if let Some(reduced) =
                        self.reduce_fulljoinresult_arg(&new_args[0], &new_args[1])
                    {
                        return reduced;
                    }
                }
                Type::Con(*id, new_args)
            }
            Type::Fn { params, ret, caps } => {
                let new_params: Vec<Type> = params.iter().map(|p| self.deep_resolve(p)).collect();
                let new_ret = Box::new(self.deep_resolve(ret));
                let new_caps = self.shallow_resolve_caps(caps);
                Type::Fn {
                    params: new_params,
                    ret: new_ret,
                    caps: new_caps,
                }
            }
            Type::Tuple(ts) => {
                let new_ts: Vec<Type> = ts.iter().map(|t| self.deep_resolve(t)).collect();
                Type::Tuple(new_ts)
            }
            // Alias: transparent for resolution; walk body, preserve wrapper.
            Type::Alias { name, body } => {
                let new_body = self.deep_resolve(body);
                Type::Alias {
                    name: *name,
                    body: Box::new(new_body),
                }
            }
            Type::Record { fields, tail } => {
                // Peel bound row vars off the tail, then deep-resolve every
                // (gathered) field type. The tail ends `Closed` or at an
                // unbound root row var.
                let (peeled_fields, peeled_tail) = self.resolve_row(fields, tail);
                let resolved_fields: Vec<(String, Type)> = peeled_fields
                    .iter()
                    .map(|(label, t)| (label.clone(), self.deep_resolve(t)))
                    .collect();
                Type::record(resolved_fields, peeled_tail)
            }
            Type::Error => Type::Error,
            // Non-exhaustive wildcard: future Type variants are deep-resolved
            // by returning them as-is (conservative; no vars to substitute).
            _ => t.clone(),
        }
    }

    /// Collects all free [`TyVid`]s from every scheme in every environment frame.
    ///
    /// A `TyVid` is "in the environment" if it appears free (not generalised) in
    /// any scheme currently in scope.  Used by [`crate::instantiate::generalise`] to compute
    /// `free_in_env` — vars that must NOT be quantified over.
    #[must_use]
    pub fn env_free_tyvids(&self) -> rustc_hash::FxHashSet<ridge_types::TyVid> {
        use rustc_hash::FxHashSet;
        let mut result: FxHashSet<ridge_types::TyVid> = FxHashSet::default();
        for frame in &self.env.frames {
            for scheme in frame.bindings.values() {
                let (free_ty, _) = scheme.free_vars();
                result.extend(free_ty);
            }
        }
        result
    }

    /// Collects all free [`CapVid`]s from every scheme in every environment frame.
    ///
    /// Counterpart to [`Self::env_free_tyvids`] for capability-row variables.
    #[must_use]
    pub fn env_free_capvids(&self) -> rustc_hash::FxHashSet<ridge_types::CapVid> {
        use rustc_hash::FxHashSet;
        let mut result: FxHashSet<ridge_types::CapVid> = FxHashSet::default();
        for frame in &self.env.frames {
            for scheme in frame.bindings.values() {
                let (_, free_cap) = scheme.free_vars();
                result.extend(free_cap);
            }
        }
        result
    }

    /// Collects all free [`RowVid`]s from every scheme in every environment frame.
    ///
    /// Counterpart to [`Self::env_free_tyvids`] for record-row variables.
    /// Generalisation uses this to avoid quantifying a row var that is still
    /// live in an outer binding.
    #[must_use]
    pub fn env_free_rowvids(&self) -> rustc_hash::FxHashSet<ridge_types::RowVid> {
        use rustc_hash::FxHashSet;
        let mut result: FxHashSet<ridge_types::RowVid> = FxHashSet::default();
        for frame in &self.env.frames {
            for scheme in frame.bindings.values() {
                result.extend(scheme.free_row_vars());
            }
        }
        result
    }

    /// Shallow-resolves a capability row:
    ///
    /// - `CapRow::Var(v)` — probes the cap unification table. If bound,
    ///   recursively resolves the bound row. If unbound, returns `CapRow::Var(v')`.
    /// - `CapRow::Concrete(_)` — returned as-is.
    #[must_use]
    pub fn shallow_resolve_caps(&mut self, c: &CapRow) -> CapRow {
        match c {
            CapRow::Var(v) => {
                let key = CapVidKey(v.0);
                let root = self.capvids.find(key);
                match self.capvids.probe_value(root).0 {
                    Some(bound) => self.shallow_resolve_caps(&bound),
                    None => CapRow::Var(CapVid(root.0)),
                }
            }
            other => other.clone(),
        }
    }
}

impl Default for InferCtx {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_types::{CapabilitySet, TyConId};

    fn cid(n: u32) -> TyConId {
        TyConId(n)
    }

    // ── fresh_tyvid allocation ────────────────────────────────────────────────

    #[test]
    fn fresh_tyvid_increments() {
        let mut ctx = InferCtx::new();
        let v0 = ctx.fresh_tyvid();
        let v1 = ctx.fresh_tyvid();
        let v2 = ctx.fresh_tyvid();
        assert_eq!(v0, TyVid(0));
        assert_eq!(v1, TyVid(1));
        assert_eq!(v2, TyVid(2));
    }

    // ── fresh_capvid allocation ───────────────────────────────────────────────

    #[test]
    fn fresh_capvid_increments() {
        let mut ctx = InferCtx::new();
        let c0 = ctx.fresh_capvid();
        let c1 = ctx.fresh_capvid();
        assert_eq!(c0, CapVid(0));
        assert_eq!(c1, CapVid(1));
    }

    // ── shallow_resolve: unbound var stays as Var ────────────────────────────

    #[test]
    fn shallow_resolve_unbound_var() {
        let mut ctx = InferCtx::new();
        let v = ctx.fresh_tyvid();
        let resolved = ctx.shallow_resolve(&Type::Var(v));
        assert!(matches!(resolved, Type::Var(_)));
    }

    // ── shallow_resolve: bound var returns its type ───────────────────────────

    #[test]
    fn shallow_resolve_bound_var() {
        let mut ctx = InferCtx::new();
        let v = ctx.fresh_tyvid();
        // Bind v → Int (Con(0, []))
        let int_ty = Type::Con(cid(0), vec![]);
        ctx.tyvids.union_value(TyVidKey(v.0), TyValue(Some(int_ty)));
        let resolved = ctx.shallow_resolve(&Type::Var(v));
        assert!(matches!(resolved, Type::Con(TyConId(0), _)));
    }

    // ── shallow_resolve: Alias peels through to body ─────────────────────────

    #[test]
    fn shallow_resolve_alias_transparent() {
        let mut ctx = InferCtx::new();
        let alias = Type::Alias {
            name: cid(7),
            body: Box::new(Type::Con(cid(0), vec![])),
        };
        let resolved = ctx.shallow_resolve(&alias);
        // Should resolve to the body, not the alias wrapper.
        assert!(matches!(resolved, Type::Con(TyConId(0), _)));
    }

    // ── shallow_resolve: Con returned as-is ──────────────────────────────────

    #[test]
    fn shallow_resolve_con_unchanged() {
        let mut ctx = InferCtx::new();
        let t = Type::Con(cid(3), vec![]);
        let resolved = ctx.shallow_resolve(&t);
        assert!(matches!(resolved, Type::Con(TyConId(3), _)));
    }

    // ── shallow_resolve: Error returned as-is ────────────────────────────────

    #[test]
    fn shallow_resolve_error_unchanged() {
        let mut ctx = InferCtx::new();
        let resolved = ctx.shallow_resolve(&Type::Error);
        assert!(matches!(resolved, Type::Error));
    }

    // ── shallow_resolve_caps: unbound cap stays as Var ───────────────────────

    #[test]
    fn shallow_resolve_caps_unbound() {
        let mut ctx = InferCtx::new();
        let c = ctx.fresh_capvid();
        let resolved = ctx.shallow_resolve_caps(&CapRow::Var(c));
        assert!(matches!(resolved, CapRow::Var(_)));
    }

    // ── shallow_resolve_caps: Concrete returned as-is ────────────────────────

    #[test]
    fn shallow_resolve_caps_concrete() {
        let mut ctx = InferCtx::new();
        let row = CapRow::Concrete(CapabilitySet::PURE);
        let resolved = ctx.shallow_resolve_caps(&row);
        assert_eq!(resolved, CapRow::Concrete(CapabilitySet::PURE));
    }

    // ── TyVidKey / CapVidKey round-trips ─────────────────────────────────────

    #[test]
    fn tyvid_key_round_trip() {
        let key = TyVidKey(42);
        assert_eq!(TyVidKey::from_index(key.index()), key);
        assert_eq!(TyVidKey::tag(), "TyVidKey");
    }

    #[test]
    fn capvid_key_round_trip() {
        let key = CapVidKey(7);
        assert_eq!(CapVidKey::from_index(key.index()), key);
        assert_eq!(CapVidKey::tag(), "CapVidKey");
    }
}
