//! [`BuiltinTyCons`] — the table of built-in type-constructor identifiers.
//!
//! # T3 implementation
//!
//! `BuiltinTyCons::allocate(&mut TyConArena)` registers the 12 built-in
//! `TyCons` (indices 0..11) and returns populated struct fields.
//!
//! Calling `unallocated()` is still available for tests and scaffolding that
//! hasn't wired the real arena yet.

use crate::{
    capability_set::CapabilitySet,
    ty::{TyVid, Type},
    tycon::{
        RecordField, RecordSchema, TyConArena, TyConDecl, TyConId, TyConKind, UnionSchema,
        UnionVariant, VariantPayload,
    },
};

// ── Synthetic function-type constructors (Fn/0 … Fn/15) ────────────────────────

/// Number of synthetic per-arity function-type constructors (`Fn/0 … Fn/15`).
///
/// Function types are structural ([`Type::Fn`]) and carry no nominal `TyCon`.
/// Typeclass dispatch, however, keys on `(ClassId, TyConId)`. These synthetic
/// ids give a function-type instance head (`instance Handler (fn a -> R)`) a
/// stable dispatch key, chosen by **arity alone** — the capability row is *not*
/// part of the key. Sixteen covers every realistic function arity.
pub const FN_ARITY_COUNT: usize = 16;

/// `TyConId` of `Fn/0` — the base of the reserved function-type block.
///
/// The block `Fn/0 … Fn/15` is interned immediately after the last nominal
/// builtin (`Quote` = 26) and before any user/stdlib `TyCon`, so user
/// allocation never collides with it. [`BuiltinTyCons::allocate`] asserts this
/// layout.
pub const FN_TYCON_BASE: u32 = 27;

/// Maps a function arity to its synthetic `Fn/arity` [`TyConId`], or `None` when
/// the arity exceeds [`FN_ARITY_COUNT`].
#[must_use]
pub fn fn_tycon_id(arity: usize) -> Option<TyConId> {
    let arity = u32::try_from(arity)
        .ok()
        .filter(|&a| (a as usize) < FN_ARITY_COUNT)?;
    Some(TyConId(FN_TYCON_BASE + arity))
}

/// Inverse of [`fn_tycon_id`]: recovers the arity from a synthetic `Fn/arity`
/// id, or `None` when `id` is not in the reserved function-type block.
#[must_use]
pub fn fn_tycon_arity(id: TyConId) -> Option<usize> {
    let offset = id.0.checked_sub(FN_TYCON_BASE)? as usize;
    (offset < FN_ARITY_COUNT).then_some(offset)
}

/// `TyConId` of `Ret/1` — the built-in return-type extractor.
///
/// `Ret p` is a type-level projection that reduces to the return type of a
/// concrete function type: `Ret (fn a -> r)` normalises to `r`. It exists so a
/// receiver-polymorphic builder method can name "the return of the projection"
/// in its result type (`Result (List (Ret p))`) without an associated-type
/// family — the one place a determined function's return must flow to a result.
/// Reserved immediately after the `Fn/N` block, so user allocation never
/// collides; the reduction lives in unification and `deep_resolve`, and the
/// constructor is internal (never written in surface syntax).
#[expect(
    clippy::cast_possible_truncation,
    reason = "FN_ARITY_COUNT is 16 — far within u32"
)]
pub const RET_TYCON_ID: u32 = FN_TYCON_BASE + FN_ARITY_COUNT as u32;

/// `TyConId` of `Rows/1` — the built-in row-shape extractor for the query
/// builder's decode terminals.
///
/// `Rows q` is a type-level projection that reduces to the row a receiver decodes
/// into: `Rows (Query e a)` to the entity `e`, `Rows (Join e f a)` to the pair
/// `(e, f)`, and `Rows (LeftJoin e f a)` to `(e, Option f)`. It is the result
/// linkage the unified `toList`/`first` use — one pair of terminals over a query,
/// an inner join, or a left join — naming "the row of the receiver" in their result
/// (`Result (List (Rows q))`) without an associated-type family. Unlike `Ret`,
/// whose reduction is structural over a function type, `Rows` reduces by the
/// receiver's own type constructor, so the reduction reads the reconciled
/// `Query`/`Join`/`LeftJoin` ids from the inference context. Reserved immediately
/// after `Ret/1`; the constructor is internal (never written in user surface
/// syntax) and the reduction lives in unification and `deep_resolve`.
pub const ROWS_TYCON_ID: u32 = RET_TYCON_ID + 1;

/// `TyConId` of `JoinCond/2` — the join-condition shape extractor for the N-ary
/// join builder.
///
/// `JoinCond q f` is a type-level projection that reduces to the curried
/// condition a `joinOn` over receiver `q` adding right entity `f` accepts:
/// `JoinCond (Query e a) f` to `e -> f -> Bool`, `JoinCond (Join e g a) f` to
/// `e -> g -> f -> Bool`, and `JoinCond (Joined q' g a) f` to the left
/// composite's entities followed by `g` and `f`. It lets the single `Joinable`
/// method name "the condition over this receiver's leaves plus the new table"
/// without an associated-type family, so the lambda's arity and per-leaf
/// entities are fixed at compile time. Reserved immediately after `Rows/1`; the
/// reduction reads the reconciled receiver ids from the context and lives in
/// unification and `deep_resolve`. Internal — never written in surface syntax.
pub const JOINCOND_TYCON_ID: u32 = ROWS_TYCON_ID + 1;

/// `TyConId` of `JoinResult/2` — the result-type extractor for the N-ary join
/// builder.
///
/// `JoinResult q f` reduces to the type `joinOn` produces from receiver `q` and
/// new right entity `f`: `JoinResult (Query e a) f` to the binary `Join e f a`
/// (the depth-2 inner join keeps its existing vocabulary), and any composite
/// receiver (`Join`/`Joined`) to `Joined q f a`, the nested form. It lets the
/// single `Joinable` method return the receiver-determined shape without an
/// associated-type family. Reserved immediately after `JoinCond/2`; same
/// reduction sites as the others. Internal — never written in surface syntax.
pub const JOINRESULT_TYCON_ID: u32 = JOINCOND_TYCON_ID + 1;

/// `TyConId` of `LeftJoinResult/2` — the LEFT outer-join result extractor.
///
/// `LeftJoinResult q f` reduces to the type `leftJoinOn` produces from receiver
/// `q` and the new right entity `f`: a binary `LeftJoin e f a` from a query, the
/// nested `LeftJoined q f a` from a composite. Reserved immediately after
/// `JoinResult/2`; same reduction sites. Internal — never written in surface
/// syntax.
pub const LEFTJOINRESULT_TYCON_ID: u32 = JOINRESULT_TYCON_ID + 1;

/// `TyConId` of `RightJoinResult/2` — the RIGHT outer-join result extractor.
///
/// `RightJoinResult q f` reduces to the type `rightJoinOn` produces from receiver
/// `q` and the new right entity `f`: a binary `RightJoin e f a` from a query, the
/// nested `RightJoined q f a` from a composite. Reserved immediately after
/// `LeftJoinResult/2`; same reduction sites. Internal — never written in surface
/// syntax.
pub const RIGHTJOINRESULT_TYCON_ID: u32 = LEFTJOINRESULT_TYCON_ID + 1;

/// `TyConId` of `FullJoinResult/2` — the FULL outer-join result extractor.
///
/// `FullJoinResult q f` reduces to the type `fullJoinOn` produces from receiver
/// `q` and the new right entity `f`: a binary `FullJoin e f a` from a query, the
/// nested `FullJoined q f a` from a composite. Reserved immediately after
/// `RightJoinResult/2`; same reduction sites. Internal — never written in surface
/// syntax.
pub const FULLJOINRESULT_TYCON_ID: u32 = RIGHTJOINRESULT_TYCON_ID + 1;

/// `TyConId` of `InsertShape/1` — the insert-input shape extractor.
///
/// `InsertShape e` reduces to the record a typed insert accepts for entity `e`:
/// the entity minus its database-generated columns. For an entity whose schema
/// marks generated columns (a serial/identity `id`, a `DEFAULT` column) it
/// reduces to a synthesized companion record `<Entity>Insert` carrying only the
/// caller-supplied fields, so writing a generated column by hand is a
/// compile-time type error; for an entity with none it reduces to the entity
/// itself, so an insert of such an entity is unchanged (backward-compatible).
/// Unlike the receiver-driven `Rows`/`JoinResult` projections, this one is
/// **invertible**: a stuck `InsertShape ?e` unified against a concrete companion
/// recovers and binds `?e` to that companion's entity, so the entity flows from
/// the repository argument or the shaped value in either order. Reserved
/// immediately after `FullJoinResult/2`; the reduction reads the per-entity
/// shape table from the context and lives in unification and `deep_resolve`.
/// Internal — never written in user surface syntax (the stdlib names it).
pub const INSERTSHAPE_TYCON_ID: u32 = FULLJOINRESULT_TYCON_ID + 1;

/// The arena/dictionary name of the synthetic `Fn/arity` constructor.
///
/// Returns `"Fn0"`, `"Fn1"`, … . This name is the bridge that keeps the
/// generated dictionary constant consistent across the pipeline: the arena decl
/// name, the instance-definition lowering, and the call-site reference all
/// derive `$inst_{Class}_Fn{arity}` from it.
#[must_use]
pub fn fn_tycon_name(arity: usize) -> String {
    format!("Fn{arity}")
}

/// Built-in `TyCon` ids — assigned at workspace-init time, then immutable.
///
/// `#[non_exhaustive]` so that adding a new built-in (e.g. in 0.2.0) is
/// non-breaking for downstream match sites.
#[non_exhaustive]
#[derive(Debug)]
pub struct BuiltinTyCons {
    /// `Int` — 64-bit signed integer (D029).
    pub int: TyConId,
    /// `Float` — IEEE-754 double-precision float.
    pub float: TyConId,
    /// `Bool` — boolean.
    pub bool: TyConId,
    /// `Text` — UTF-8 string.
    pub text: TyConId,
    /// `Unit` — the unit type `()`.
    pub unit: TyConId,
    /// `Timestamp` — wall-clock time (D048).
    pub timestamp: TyConId,
    /// `List a` — an ordered sequence.
    pub list: TyConId,
    /// `Map k v` — an ordered key-value mapping (ordered/deterministic).
    pub map: TyConId,
    /// `Set a` — an ordered set.
    pub set: TyConId,
    /// `Option a` — an optional value.
    pub option: TyConId,
    /// `Result a e` — a fallible computation.
    pub result: TyConId,
    /// `Handle a` — a reference to a running actor instance (D061).
    pub handle: TyConId,
    /// `Error { code: Text, message: Text }` — stdlib error record (§3.11, OQ-S007).
    ///
    /// Used as the `e` parameter of `Result _ Error` returns in `std.io`,
    /// `std.fs`, `std.time`, `std.proc`.  Registered as a `TyConKind::Record`
    /// so that field access (`err.code`, `err.message`) is typeable.
    pub error: TyConId,
    /// `Duration { ms: Int }` — time difference record (§3.12).
    ///
    /// Returned by `std.time.diff`.
    pub duration: TyConId,
    /// `ProcOutput { stdout: Text, stderr: Text, exitCode: Int }` — process
    /// output record (§3.16 / OQ-S007 / D123).
    ///
    /// Returned as the `Ok` payload of `std.proc.run`.
    pub proc_output: TyConId,
    /// `Ordering = Less | Equal | Greater` — the result type of `compare`.
    ///
    /// Required by the `Ord` typeclass (0.2.13). Registered as a prelude
    /// union type so any module can match on `Less`, `Equal`, `Greater`
    /// without an explicit import.
    pub ordering: TyConId,
    /// `JsonValue` — the JSON value tree (§3.17).
    ///
    /// `JNull | JBool Bool | JInt Int | JFloat Float | JText Text
    ///  | JList (List JsonValue) | JObject (Map Text JsonValue)`.
    ///
    /// Registered as a prelude union so any module can build and match JSON
    /// values without importing `std.json`. The variants lower to the
    /// lowercase-snake BEAM atoms (`json_null`, `{json_int, N}`, …) that
    /// `ridge_rt:json_encode/1` walks — see `ridge-codegen-erl`.
    pub json_value: TyConId,
    /// `std.net.http` `Sql` — opaque `{ value: Text }` taint wrapper.
    pub sql: TyConId,
    /// `std.net.http` `Html` — opaque `{ value: Text }` taint wrapper.
    pub html: TyConId,
    /// `std.net.http` `SecureCookie` — opaque cookie record with safe defaults.
    pub secure_cookie: TyConId,
    /// `std.sql` `SqlValue` — the opaque, adapter-neutral SQL column value that
    /// the `SqlType` codec class maps Ridge values to and from. Modelled like
    /// `JsonValue` (a union of the base column shapes) but opaque to consumers:
    /// user code imports the type name and the `toSql`/`fromSql` methods, never
    /// the variants. The matching `pub type SqlValue` in `sql.ridge` is what the
    /// stdlib's own compilation sees.
    pub sql_value: TyConId,
    /// `Column e a` — a typed column reference produced by `deriving (Table)`.
    ///
    /// `e` (entity) and `a` (value type) are phantom parameters that keep
    /// columns of different tables and types from mixing; the carried data is
    /// `{ name: Text, table: Text }`. Registered as a `TyConKind::Record` so
    /// `col.name` is typeable. Compiler-internal: user code never names it
    /// directly, it only appears in a generated column mirror.
    pub column: TyConId,
    /// `Table e` — table metadata produced by `deriving (Table)`.
    ///
    /// `e` (entity) is phantom; the data is `{ name: Text, columns: List Text }`
    /// (the table name and ordered column names). Compiler-internal, like
    /// [`Self::column`].
    pub table: TyConId,
    /// `FieldSchema` — one entry in a [`Self::schema`] descriptor produced by
    /// `deriving (Schema)`.
    ///
    /// The data is `{ name: Text, column: Text, ty: Text, optional: Bool }`:
    /// the record field name, its SQL column name, a readable spelling of its
    /// type, and whether it is `Option`-wrapped. Registered as a
    /// `TyConKind::Record` so `field.ty` is typeable; non-opaque so user code
    /// can read the descriptor.
    pub field_schema: TyConId,
    /// `Schema` — the structural descriptor produced by `deriving (Schema)`.
    ///
    /// The data is `{ name: Text, table: Text, fields: List FieldSchema }`: the
    /// entity name, its SQL table name, and the per-field descriptors. Used as
    /// the introspection source for `OpenAPI` generation and migration diffing.
    /// Arity 0 (uniform across entities) so a `List Schema` collects every
    /// model. Compiler-internal, like [`Self::column`].
    pub schema: TyConId,
    /// `QExpr` — the reified expression tree a quoted predicate is captured as.
    ///
    /// A prelude union (like [`Self::json_value`]) so the quotation runtime can
    /// build and match it without an import. Leaves are columns and literals;
    /// inner nodes are the comparison and boolean operators. The compiler
    /// constructs these nodes directly when it reifies a quoted body.
    pub q_expr: TyConId,
    /// `Quote f` — a captured expression. `f` records the quoted shape (for a
    /// predicate, `Entity -> Bool`) so a query and its predicate stay in
    /// agreement; it is phantom — the value carries only the reified `tree`.
    pub quote: TyConId,
    /// Synthetic per-arity function-type constructors `Fn/0 … Fn/15`
    /// (index = arity). Dispatch keys only — never applied as `Type::Con`.
    /// See [`fn_tycon_id`] / [`FN_ARITY_COUNT`].
    pub fns: [TyConId; FN_ARITY_COUNT],
    /// `Ret/1` — the return-type extractor. `Ret p` reduces to the return of a
    /// concrete function `p`. Internal: it appears only in Rust-seeded schemes,
    /// never in surface syntax. See [`RET_TYCON_ID`].
    pub ret: TyConId,
    /// `Rows/1` — the row-shape extractor for the decode terminals. `Rows q`
    /// reduces to the row a query/join receiver decodes into. Internal; the
    /// reduction reads the reconciled receiver ids from the context. See
    /// [`ROWS_TYCON_ID`].
    pub rows: TyConId,
    /// `JoinCond/2` — the join-condition shape extractor. `JoinCond q f` reduces
    /// to the curried condition `joinOn` accepts over receiver `q` and new right
    /// entity `f`. Internal; the reduction reads the reconciled receiver ids from
    /// the context. See [`JOINCOND_TYCON_ID`].
    pub joincond: TyConId,
    /// `JoinResult/2` — the join-result extractor. `JoinResult q f` reduces to the
    /// type `joinOn` produces from receiver `q` and new right entity `f` (a binary
    /// `Join` from a query, the nested `Joined` from a composite). Internal; the
    /// reduction reads the reconciled receiver ids from the context. See
    /// [`JOINRESULT_TYCON_ID`].
    pub joinresult: TyConId,
    /// `LeftJoinResult/2` — the LEFT outer-join result extractor. `LeftJoinResult
    /// q f` reduces to the type `leftJoinOn` produces from receiver `q` and the
    /// new right entity `f` (a binary `LeftJoin` from a query, the nested
    /// `LeftJoined` from a composite). Internal; the reduction reads the
    /// reconciled receiver ids from the context. See [`LEFTJOINRESULT_TYCON_ID`].
    pub left_joinresult: TyConId,
    /// `RightJoinResult/2` — the RIGHT outer-join result extractor. `RightJoinResult
    /// q f` reduces to the type `rightJoinOn` produces (a binary `RightJoin` from a
    /// query, the nested `RightJoined` from a composite). Internal; reads the
    /// reconciled receiver ids from the context. See [`RIGHTJOINRESULT_TYCON_ID`].
    pub right_joinresult: TyConId,
    /// `FullJoinResult/2` — the FULL outer-join result extractor. `FullJoinResult q
    /// f` reduces to the type `fullJoinOn` produces (a binary `FullJoin` from a
    /// query, the nested `FullJoined` from a composite). Internal; reads the
    /// reconciled receiver ids from the context. See [`FULLJOINRESULT_TYCON_ID`].
    pub full_joinresult: TyConId,
    /// `InsertShape/1` — the insert-input shape extractor. `InsertShape e`
    /// reduces to the record a typed insert accepts for `e` — the entity minus
    /// its database-generated columns (a synthesized `<Entity>Insert` companion,
    /// or `e` itself when none). Internal; the reduction reads the per-entity
    /// shape table from the context and is invertible. See [`INSERTSHAPE_TYCON_ID`].
    pub insert_shape: TyConId,
    /// `Decimal` — an arbitrary-precision base-10 number. A primitive like
    /// `Int`/`Float`, but interned last (id 51) so the historical 0..50 index
    /// layout stays stable for the many call sites that hardcode those ids. The
    /// value/DDL wiring lives in `std.decimal` and `std.sql`.
    pub decimal: TyConId,
    /// `Uuid` — an RFC 4122 identifier. A primitive like `Int`/`Text`, interned
    /// after `Decimal` (id 52) so the historical 0..50 index layout stays stable.
    /// It has no literal syntax; a value comes from `std.uuid` (`gen`, `fromText`),
    /// and the codec that moves it across a SQL `uuid` column lives in `std.sql`.
    pub uuid: TyConId,
}

impl BuiltinTyCons {
    /// Returns an uninitialised `BuiltinTyCons` with sentinel values.
    ///
    /// **Panics** if any field is used before `allocate` (T3) has been called.
    /// This constructor exists only so that T2 types compile; real allocation
    /// is implemented in T3.
    #[must_use]
    pub const fn unallocated() -> Self {
        // Sentinel value — any use before T3 wires the real IDs will panic at
        // the call site (via the debug assertion in T3's allocator).
        const SENTINEL: TyConId = TyConId(u32::MAX);
        Self {
            int: SENTINEL,
            float: SENTINEL,
            bool: SENTINEL,
            text: SENTINEL,
            unit: SENTINEL,
            timestamp: SENTINEL,
            list: SENTINEL,
            map: SENTINEL,
            set: SENTINEL,
            option: SENTINEL,
            result: SENTINEL,
            handle: SENTINEL,
            error: SENTINEL,
            duration: SENTINEL,
            proc_output: SENTINEL,
            ordering: SENTINEL,
            json_value: SENTINEL,
            sql: SENTINEL,
            html: SENTINEL,
            secure_cookie: SENTINEL,
            sql_value: SENTINEL,
            column: SENTINEL,
            table: SENTINEL,
            field_schema: SENTINEL,
            schema: SENTINEL,
            q_expr: SENTINEL,
            quote: SENTINEL,
            fns: [SENTINEL; FN_ARITY_COUNT],
            ret: SENTINEL,
            rows: SENTINEL,
            joincond: SENTINEL,
            joinresult: SENTINEL,
            left_joinresult: SENTINEL,
            right_joinresult: SENTINEL,
            full_joinresult: SENTINEL,
            insert_shape: SENTINEL,
            decimal: SENTINEL,
            uuid: SENTINEL,
        }
    }

    /// Allocates the built-in `TyCons` into `arena` and returns a populated
    /// `BuiltinTyCons`.
    ///
    /// Indices are assigned in a fixed order (Int=0, Float=1, Bool=2, Text=3,
    /// Unit=4, Timestamp=5, List=6, Map=7, Set=8, Option=9, Result=10,
    /// Handle=11, Error=12, Duration=13, ProcOutput=14, Ordering=15,
    /// JsonValue=16, Sql=17, Html=18, SecureCookie=19, SqlValue=20) matching
    /// spec §4.1.
    /// Callers must pass a **fresh** arena (i.e. `arena.is_empty()` must be
    /// true) so that the resulting `TyConId`s are stable and predictable.
    ///
    /// # Panics
    ///
    /// Panics (debug only) if `arena` is not empty — indicates caller error:
    /// built-ins must be the first entries in the arena.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "flat sequential arena.intern() calls; splitting would harm readability without reducing complexity"
    )]
    #[allow(
        clippy::cognitive_complexity,
        reason = "same as too_many_lines above — clippy 1.88 also flags cognitive_complexity here"
    )]
    pub fn allocate(arena: &mut TyConArena) -> Self {
        debug_assert!(
            arena.is_empty(),
            "BuiltinTyCons::allocate requires an empty arena; got {} entries",
            arena.len()
        );

        // ── Primitive atom types (arity 0, TyConKind::Primitive) ──────────────
        let int = arena.intern(TyConDecl {
            id: TyConId(0), // overwritten by arena.intern
            name: "Int".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        let float = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Float".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        let bool_ = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Bool".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        let text = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Text".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        let unit = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Unit".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        let timestamp = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Timestamp".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // ── Generic built-in containers (TyConKind::Builtin) ──────────────────
        let list = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "List".to_string(),
            arity: 1,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        let map = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Map".to_string(),
            arity: 2,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        let set = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Set".to_string(),
            arity: 1,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // ── Prelude unions (TyConKind::Union) ─────────────────────────────────
        //
        // Option and Result carry canonical UnionSchemas so that T4 can attach
        // the right Scheme to `Some`, `None`, `Ok`, `Err` (§4.3).
        // The type-variable TyVids used here are *schema-level* placeholders,
        // not inference variables; they are stable dummy IDs that the prelude
        // wiring (T4) will replace with fresh ones on each instantiation.
        let option = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Option".to_string(),
            arity: 1,
            kind: TyConKind::Union(UnionSchema {
                params: vec![TyVid(0)],
                variants: vec![
                    UnionVariant {
                        name: "Some".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Var(TyVid(0))]),
                    },
                    UnionVariant {
                        name: "None".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        // Result a e — Ok a | Err e  (spec: Result a e)
        let result = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Result".to_string(),
            arity: 2,
            kind: TyConKind::Union(UnionSchema {
                params: vec![TyVid(0), TyVid(1)],
                variants: vec![
                    UnionVariant {
                        name: "Ok".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Var(TyVid(0))]),
                    },
                    UnionVariant {
                        name: "Err".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Var(TyVid(1))]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // ── Handle a — phantom actor-reference type (TyConKind::Builtin) ──────
        //
        // Handle is a 1-arity opaque type; its "schema" is the actor's TyConDecl
        // (looked up at use sites).  D061: `spawn ActorName args` produces a
        // `Handle(ActorTyCon)`.
        let handle = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Handle".to_string(),
            arity: 1,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // ── Stdlib record types (TyConKind::Record) ───────────────────────────
        //
        // These are declared as `TyConKind::Record` so that field access is
        // typeable (e.g. `err.code : Text`).  They parallel `Timestamp` in that
        // they are pre-allocated in `BuiltinTyCons` rather than arising from a
        // user `TypeDecl` — the stdlib build pipeline compiles each tier in
        // isolation, so cross-tier record references must be pre-registered here.
        //
        // The `Text` and `Int` field types reference the `text` and `int` ids
        // allocated above; at this point in `allocate` those ids are valid.
        //
        // §3.11 / OQ-S007: Error { code: Text, message: Text }
        let error = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Error".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "code".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "message".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        // §3.12: Duration { ms: Int }
        let duration = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Duration".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![RecordField {
                    name: "ms".to_string(),
                    ty: Type::Con(int, vec![]),
                }],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        // §3.16 / OQ-S007 / D123: ProcOutput { stdout: Text, stderr: Text, exitCode: Int }
        let proc_output = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "ProcOutput".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "stdout".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "stderr".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "exitCode".to_string(),
                        ty: Type::Con(int, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // Ordering = Less | Equal | Greater (0.2.13 prelude type, required by Ord)
        let ordering = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Ordering".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "Less".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Equal".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Greater".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None, // prelude — no user module
            opaque: false,
            is_anon: false,
        });

        // JsonValue — the JSON value tree (§3.17), a prelude union so any module
        // can build and match JSON without importing std.json. JList/JObject are
        // self-referential, so their payloads name JsonValue's own TyConId. The
        // arena assigns ids sequentially, so this is index 16 (asserted below).
        let json_value = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "JsonValue".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "JNull".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "JBool".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(bool_, vec![])]),
                    },
                    UnionVariant {
                        name: "JInt".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(int, vec![])]),
                    },
                    UnionVariant {
                        name: "JFloat".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(float, vec![])]),
                    },
                    UnionVariant {
                        name: "JText".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(text, vec![])]),
                    },
                    UnionVariant {
                        name: "JList".to_string(),
                        // List JsonValue
                        kind: VariantPayload::Positional(vec![Type::Con(
                            list,
                            vec![Type::Con(TyConId(16), vec![])],
                        )]),
                    },
                    UnionVariant {
                        name: "JObject".to_string(),
                        // Map Text JsonValue
                        kind: VariantPayload::Positional(vec![Type::Con(
                            map,
                            vec![Type::Con(text, vec![]), Type::Con(TyConId(16), vec![])],
                        )]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None, // prelude — no user module
            opaque: false,
            is_anon: false,
        });

        // Opaque taint wrappers. Declared as `opaque` records so that field access
        // outside their defining module is a type error (T036); `def_module_raw =
        // u32::MAX` is a sentinel that never equals a real user module, so any
        // consumer-module access is treated as cross-module. The matching local
        // declarations (`Sql` in sql.ridge; `Html`/`SecureCookie` in net/http.ridge)
        // are what the stdlib's own compilation sees; user code resolves these names
        // to these builtin ids.
        let sql = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Sql".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![RecordField {
                    name: "value".to_string(),
                    ty: Type::Con(text, vec![]),
                }],
            )),
            def_span: None,
            def_module_raw: Some(u32::MAX),
            opaque: true,
            is_anon: false,
        });
        let html = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Html".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![RecordField {
                    name: "value".to_string(),
                    ty: Type::Con(text, vec![]),
                }],
            )),
            def_span: None,
            def_module_raw: Some(u32::MAX),
            opaque: true,
            is_anon: false,
        });

        // SecureCookie — an opaque cookie value built with safe defaults by the
        // `secureCookie` factory and adjusted via the exported `with*` setters.
        let secure_cookie = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "SecureCookie".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "value".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "secure".to_string(),
                        ty: Type::Con(bool_, vec![]),
                    },
                    RecordField {
                        name: "httpOnly".to_string(),
                        ty: Type::Con(bool_, vec![]),
                    },
                    RecordField {
                        name: "sameSite".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "maxAge".to_string(),
                        ty: Type::Con(option, vec![Type::Con(int, vec![])]),
                    },
                    RecordField {
                        name: "path".to_string(),
                        ty: Type::Con(option, vec![Type::Con(text, vec![])]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: Some(u32::MAX),
            opaque: true,
            is_anon: false,
        });

        // SqlValue — the SQL column value the `SqlType` codec maps to/from. A
        // union of the base column shapes, mirroring the `pub type SqlValue` in
        // sql.ridge. Marked opaque (and given the cross-module sentinel module)
        // so consumers cannot construct or match its variants; they reach it only
        // through the imported `toSql`/`fromSql` methods.
        let sql_value = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "SqlValue".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "SqlInt".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(int, vec![])]),
                    },
                    UnionVariant {
                        name: "SqlText".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(text, vec![])]),
                    },
                    UnionVariant {
                        name: "SqlBool".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(bool_, vec![])]),
                    },
                    UnionVariant {
                        name: "SqlFloat".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(float, vec![])]),
                    },
                    UnionVariant {
                        name: "SqlNull".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "SqlInstant".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(int, vec![])]),
                    },
                    UnionVariant {
                        name: "SqlDecimal".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(text, vec![])]),
                    },
                    UnionVariant {
                        name: "SqlUuid".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(text, vec![])]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: Some(u32::MAX),
            opaque: true,
            is_anon: false,
        });

        // Column e a — typed column reference from `deriving (Table)`. Phantom
        // `e`/`a` (arity 2) are unused by the fields, so a use site's argument
        // substitution leaves `name`/`table` as `Text`. Non-opaque: `col.name`
        // is readable from user code.
        let column = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Column".to_string(),
            arity: 2,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1)],
                vec![
                    RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "table".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        // Table e — table metadata from `deriving (Table)`. Phantom `e`
        // (arity 1); fields are the table name and ordered column names.
        let table = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Table".to_string(),
            arity: 1,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0)],
                vec![
                    RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "columns".to_string(),
                        ty: Type::Con(list, vec![Type::Con(text, vec![])]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // FieldSchema — one column entry in a `deriving (Schema)` descriptor.
        // Interned before Schema so Schema's `fields` field can name its id.
        let field_schema = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "FieldSchema".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "column".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "ty".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "optional".to_string(),
                        ty: Type::Con(bool_, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });
        // Schema — the structural descriptor from `deriving (Schema)`. Arity 0;
        // `fields` is `List FieldSchema` (the id just allocated above).
        let schema = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Schema".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "table".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "fields".to_string(),
                        ty: Type::Con(list, vec![Type::Con(field_schema, vec![])]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // QExpr — the reified quotation expression tree. A prelude union (like
        // JsonValue) so the quotation runtime builds and matches it without an
        // import. Self-referential variants name QExpr's own id (index 25).
        let q_expr = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "QExpr".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "QCol".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(text, vec![])]),
                    },
                    UnionVariant {
                        name: "QLitInt".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(int, vec![])]),
                    },
                    UnionVariant {
                        name: "QLitText".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(text, vec![])]),
                    },
                    UnionVariant {
                        name: "QLitBool".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(bool_, vec![])]),
                    },
                    UnionVariant {
                        name: "QLitFloat".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(float, vec![])]),
                    },
                    UnionVariant {
                        name: "QAnd".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QOr".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QNot".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(TyConId(25), vec![])]),
                    },
                    UnionVariant {
                        name: "QEq".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QNe".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QLt".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QGt".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QLe".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QGe".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    // A projection: a select-list of `(alias, column)` pairs. The
                    // alias is the output column name (the record field), the
                    // QExpr is the projected column.
                    UnionVariant {
                        name: "QProj".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(
                            list,
                            vec![Type::Tuple(vec![
                                Type::Con(text, vec![]),
                                Type::Con(TyConId(25), vec![]),
                            ])],
                        )]),
                    },
                    // A right-side column reference in a join. `QCol` names a
                    // column of the left (or only) table; `QColR` names one of the
                    // right table, so a two-table quote keeps the two sides apart.
                    // Single-table quotes never produce it.
                    UnionVariant {
                        name: "QColR".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(text, vec![])]),
                    },
                    // The group key of a `groupBy` query — the column the rows are
                    // partitioned by. In a `summarize` projection or a `having`
                    // predicate it stands for `g.key`. Carries no column of its own:
                    // the key column travels alongside the tree at the seam.
                    UnionVariant {
                        name: "QGroupKey".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    // A per-group `COUNT(*)` — `g.count`. Nullary: it counts the
                    // rows of the group, naming no column.
                    UnionVariant {
                        name: "QAggCount".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    // The per-group scalar aggregates over a single column —
                    // `g.sum(col)`, `g.avg(col)`, `g.min(col)`, `g.max(col)`. Each
                    // wraps the `QCol` it folds.
                    UnionVariant {
                        name: "QAggSum".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(TyConId(25), vec![])]),
                    },
                    UnionVariant {
                        name: "QAggAvg".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(TyConId(25), vec![])]),
                    },
                    UnionVariant {
                        name: "QAggMin".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(TyConId(25), vec![])]),
                    },
                    UnionVariant {
                        name: "QAggMax".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(TyConId(25), vec![])]),
                    },
                    // A column reference tagged by source index in an N-ary join.
                    // `QCol` names a column of leaf 0 (the left or only table) and
                    // `QColR` one of leaf 1 (the binary right table); `QColAt`
                    // carries the leaf index explicitly so a join of three or more
                    // tables can name a column of any source. The leaf order is the
                    // left-to-right walk of the join tree, so `QColAt 2` is the
                    // third table joined. Two-table quotes never produce it — they
                    // stay `QCol`/`QColR` so the binary path is byte-identical.
                    UnionVariant {
                        name: "QColAt".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(int, vec![]),
                            Type::Con(text, vec![]),
                        ]),
                    },
                    // A `<expr> IS NOT TRUE` test: true when the inner predicate is
                    // false OR unknown (NULL). The three-valued dual of `QNot` — `QNot`
                    // of a NULL is still NULL, this is TRUE — so `every` can probe for a
                    // row that violates its predicate, an outer join's unmatched side
                    // (whose columns read NULL) counting as a violation. Appended last so
                    // the existing variant indices the lowering pass hardcodes stay put.
                    UnionVariant {
                        name: "QNotTrue".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(TyConId(25), vec![])]),
                    },
                    // A `value LIKE pattern` test. The first operand is the column,
                    // the second a `QLitText` carrying the SQL LIKE pattern (already
                    // escaped and wrapped at reify time for the `contains`/`startsWith`/
                    // `endsWith` forms, passed through verbatim for the raw `like`
                    // form). Appended after `QNotTrue` so existing variant indices the
                    // lowering pass hardcodes stay put.
                    UnionVariant {
                        name: "QLike".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    // A `value IN (e0, e1, …)` test. The first operand is the column,
                    // the second a list of literal `QExpr` elements (the IN set). An
                    // empty set renders as `FALSE` — nothing is a member of it.
                    UnionVariant {
                        name: "QIn".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(list, vec![Type::Con(TyConId(25), vec![])]),
                        ]),
                    },
                    // The arithmetic value nodes — `a + b`, `a - b`, `a * b`,
                    // `a / b`, `a % b` — each over two operand `QExpr`s. Unlike the
                    // comparison nodes these stand for a *value*, not a predicate:
                    // they appear as an operand of a comparison (`price * qty > 100`),
                    // recursively. Both operands share one numeric type (Int or
                    // Float); `%` is Int-only, matching Postgres. Appended after
                    // `QIn` so existing variant indices the lowering pass hardcodes
                    // stay put.
                    UnionVariant {
                        name: "QAdd".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QSub".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QMul".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QDiv".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "QMod".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    // A conditional value — `if cond then a else b` → the SQL
                    // `CASE WHEN cond THEN a ELSE b END`. The first child is the
                    // boolean condition, the second and third its two branches.
                    // The branches share one type — the value the whole CASE
                    // yields — or are both predicates, making the CASE itself a
                    // boolean usable in a WHERE. Appended after the arithmetic
                    // nodes so existing variant indices the lowering pass
                    // hardcodes stay put.
                    UnionVariant {
                        name: "QCase".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    // A correlated `EXISTS (SELECT 1 FROM <table> …)` subquery test,
                    // the in-quote `exists inner (fn p -> …)` over a captured table.
                    // The first operand is the inner table name (a runtime value read
                    // off the captured repo), the second the correlated predicate over
                    // the outer row (`QCol`) and the inner row (`QColR`) — the same two
                    // sides a join condition names, so the backend probes it through
                    // the existing two-row predicate path. `notExists` wraps it in a
                    // `QNot`. Appended after `QCase` so existing variant indices the
                    // lowering pass hardcodes stay put.
                    UnionVariant {
                        name: "QExists".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(text, vec![]),
                            Type::Con(TyConId(25), vec![]),
                        ]),
                    },
                    // A decimal literal captured in a quoted predicate. Carries a
                    // Decimal (tycon id 51, interned after this union). Appended last,
                    // like `QExists`, so the variant indices the lowering pass
                    // hardcodes stay put.
                    UnionVariant {
                        name: "QLitDecimal".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(TyConId(51), vec![])]),
                    },
                    // A uuid captured in a quoted predicate (a uuid has no literal
                    // syntax, so this only ever holds a captured runtime value).
                    // Carries a Uuid (tycon id 52). Appended last so the variant
                    // indices the lowering pass hardcodes stay put.
                    UnionVariant {
                        name: "QLitUuid".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(TyConId(52), vec![])]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None, // prelude — no user module
            opaque: false,
            is_anon: false,
        });

        // Quote f — a captured expression. Phantom `f` (arity 1); the carried
        // data is the reified `tree`. Prelude record (like Table) so the type
        // name resolves in any module without an import.
        let quote = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Quote".to_string(),
            arity: 1,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0)],
                vec![RecordField {
                    name: "tree".to_string(),
                    ty: Type::Con(q_expr, vec![]),
                }],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // Synthetic per-arity function-type constructors Fn/0 … Fn/15. Function
        // types are structural (`Type::Fn`) with no nominal TyCon; these ids let
        // an instance head over a function type participate in (ClassId, TyConId)
        // dispatch, keyed by arity only (caps are NOT part of the key). They are
        // dispatch keys — never applied as `Type::Con`, so `arity` is 0. They sit
        // immediately after the nominal builtins and before any user/stdlib TyCon
        // (FN_TYCON_BASE = 27), so user allocation cannot collide.
        let mut fns = [TyConId(u32::MAX); FN_ARITY_COUNT];
        for (n, slot) in fns.iter_mut().enumerate() {
            *slot = arena.intern(TyConDecl {
                id: TyConId(0),
                name: fn_tycon_name(n),
                arity: 0,
                kind: TyConKind::Builtin,
                def_span: None,
                def_module_raw: None,
                opaque: false,
                is_anon: false,
            });
        }

        // Ret/1 — the return-type extractor, interned right after the Fn/N block
        // (RET_TYCON_ID = 43). Unlike the Fn dispatch keys it IS applied as
        // `Type::Con(ret, [p])`, so its arity is 1; the reduction `Ret (fn .. -> r)
        // -> r` lives in the unifier and `deep_resolve`.
        let ret = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Ret".to_string(),
            arity: 1,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // Rows/1 — the row-shape extractor for the decode terminals, interned
        // right after Ret/1 (ROWS_TYCON_ID = 44). Applied as `Type::Con(rows, [q])`;
        // the reduction `Rows (Query e a) -> e` (and the join shapes) lives in the
        // unifier and `deep_resolve`, keyed on the receiver's reconciled tycon.
        let rows = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Rows".to_string(),
            arity: 1,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // JoinCond/2 — the join-condition shape extractor, interned right after
        // Rows/1 (JOINCOND_TYCON_ID = 45). Applied as `Type::Con(joincond, [q, f])`;
        // the reduction `JoinCond (Query e a) f -> e -> f -> Bool` (and the
        // composite shapes) lives in the unifier and `deep_resolve`, keyed on the
        // receiver's reconciled tycon.
        let joincond = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "JoinCond".to_string(),
            arity: 2,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // JoinResult/2 — the join-result extractor, interned right after
        // JoinCond/2 (JOINRESULT_TYCON_ID = 46). Applied as
        // `Type::Con(joinresult, [q, f])`; the reduction `JoinResult (Query e a) f
        // -> Join e f a` (binary) and `JoinResult <composite> f -> Joined …` lives
        // in the unifier and `deep_resolve`, keyed on the receiver's reconciled
        // tycon.
        let joinresult = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "JoinResult".to_string(),
            arity: 2,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // LeftJoinResult/2 — the LEFT outer-join result extractor, interned right
        // after JoinResult/2 (LEFTJOINRESULT_TYCON_ID = 47). Applied as
        // `Type::Con(left_joinresult, [q, f])`; the reduction `LeftJoinResult
        // (Query e a) f -> LeftJoin e f a` (binary) and `LeftJoinResult <composite>
        // f -> LeftJoined …` lives in the unifier and `deep_resolve`, keyed on the
        // receiver's reconciled tycon.
        let left_joinresult = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "LeftJoinResult".to_string(),
            arity: 2,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // RightJoinResult/2 — the RIGHT outer-join result extractor, interned right
        // after LeftJoinResult/2 (RIGHTJOINRESULT_TYCON_ID = 48). Applied as
        // `Type::Con(right_joinresult, [q, f])`; the reduction `RightJoinResult
        // (Query e a) f -> RightJoin e f a` (binary) and `RightJoinResult <composite>
        // f -> RightJoined …` lives in the unifier and `deep_resolve`.
        let right_joinresult = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "RightJoinResult".to_string(),
            arity: 2,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // FullJoinResult/2 — the FULL outer-join result extractor, interned right
        // after RightJoinResult/2 (FULLJOINRESULT_TYCON_ID = 49). Applied as
        // `Type::Con(full_joinresult, [q, f])`; the reduction `FullJoinResult
        // (Query e a) f -> FullJoin e f a` (binary) and `FullJoinResult <composite> f
        // -> FullJoined …` lives in the unifier and `deep_resolve`.
        let full_joinresult = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "FullJoinResult".to_string(),
            arity: 2,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // InsertShape/1 — the insert-input shape extractor, interned right after
        // FullJoinResult/2 (INSERTSHAPE_TYCON_ID = 50). Applied as
        // `Type::Con(insert_shape, [e])`; the reduction `InsertShape e -> <Entity>Insert`
        // (or `-> e` when the entity has no generated columns) lives in the unifier
        // and `deep_resolve`, keyed on the per-entity shape table, and is invertible.
        let insert_shape = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "InsertShape".to_string(),
            arity: 1,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // Decimal — an arbitrary-precision base-10 primitive (id 51). A scalar
        // like Int/Float/Timestamp, but interned last so the historical 0..50
        // index layout stays fixed; several call sites hardcode those ids. Its
        // runtime value is a scaled integer carried by `ridge_rt`; the codec and
        // column wiring live in `std.decimal` / `std.sql`.
        let decimal = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Decimal".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // Uuid — an RFC 4122 identifier primitive (id 52). A scalar like
        // Int/Text, interned after Decimal so the historical 0..50 layout stays
        // fixed. Its runtime value is the canonical text carried by `ridge_rt`;
        // the constructors live in `std.uuid` and the SQL codec in `std.sql`.
        let uuid = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Uuid".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // Verify assignment order matches spec §4.1 indices 0..16.
        debug_assert_eq!(int.0, 0);
        debug_assert_eq!(float.0, 1);
        debug_assert_eq!(bool_.0, 2);
        debug_assert_eq!(text.0, 3);
        debug_assert_eq!(unit.0, 4);
        debug_assert_eq!(timestamp.0, 5);
        debug_assert_eq!(list.0, 6);
        debug_assert_eq!(map.0, 7);
        debug_assert_eq!(set.0, 8);
        debug_assert_eq!(option.0, 9);
        debug_assert_eq!(result.0, 10);
        debug_assert_eq!(handle.0, 11);
        debug_assert_eq!(error.0, 12);
        debug_assert_eq!(duration.0, 13);
        debug_assert_eq!(proc_output.0, 14);
        debug_assert_eq!(ordering.0, 15);
        debug_assert_eq!(json_value.0, 16);
        debug_assert_eq!(sql.0, 17);
        debug_assert_eq!(html.0, 18);
        debug_assert_eq!(secure_cookie.0, 19);
        debug_assert_eq!(sql_value.0, 20);
        debug_assert_eq!(column.0, 21);
        debug_assert_eq!(table.0, 22);
        debug_assert_eq!(field_schema.0, 23);
        debug_assert_eq!(schema.0, 24);
        debug_assert_eq!(q_expr.0, 25);
        debug_assert_eq!(quote.0, 26);
        // Synthetic Fn/N block: Fn/0 = 27 … Fn/15 = 42 (FN_TYCON_BASE = 27).
        debug_assert_eq!(fns[0].0, FN_TYCON_BASE);
        debug_assert_eq!(fns[0].0, 27);
        debug_assert_eq!(fns[FN_ARITY_COUNT - 1].0, 42);
        // Ret/1 sits immediately after the Fn/N block (RET_TYCON_ID = 43).
        debug_assert_eq!(ret.0, RET_TYCON_ID);
        debug_assert_eq!(ret.0, 43);
        // Rows/1 sits immediately after Ret/1 (ROWS_TYCON_ID = 44).
        debug_assert_eq!(rows.0, ROWS_TYCON_ID);
        debug_assert_eq!(rows.0, 44);
        // JoinCond/2 sits immediately after Rows/1 (JOINCOND_TYCON_ID = 45).
        debug_assert_eq!(joincond.0, JOINCOND_TYCON_ID);
        debug_assert_eq!(joincond.0, 45);
        // JoinResult/2 sits immediately after JoinCond/2 (JOINRESULT_TYCON_ID = 46).
        debug_assert_eq!(joinresult.0, JOINRESULT_TYCON_ID);
        debug_assert_eq!(joinresult.0, 46);
        // LeftJoinResult/2 sits right after JoinResult/2 (LEFTJOINRESULT_TYCON_ID = 47).
        debug_assert_eq!(left_joinresult.0, LEFTJOINRESULT_TYCON_ID);
        debug_assert_eq!(left_joinresult.0, 47);
        // RightJoinResult/2 sits right after it (RIGHTJOINRESULT_TYCON_ID = 48).
        debug_assert_eq!(right_joinresult.0, RIGHTJOINRESULT_TYCON_ID);
        debug_assert_eq!(right_joinresult.0, 48);
        // FullJoinResult/2 sits right after it (FULLJOINRESULT_TYCON_ID = 49).
        debug_assert_eq!(full_joinresult.0, FULLJOINRESULT_TYCON_ID);
        debug_assert_eq!(full_joinresult.0, 49);
        // InsertShape/1 sits right after FullJoinResult/2 (INSERTSHAPE_TYCON_ID = 50).
        debug_assert_eq!(insert_shape.0, INSERTSHAPE_TYCON_ID);
        debug_assert_eq!(insert_shape.0, 50);
        // Decimal and Uuid are interned last so they do not disturb the 0..50 layout.
        debug_assert_eq!(decimal.0, 51);
        debug_assert_eq!(uuid.0, 52);

        // Suppress the "unused" lint — CapabilitySet is imported for future use
        // in T4 (actor schemas carry CapabilitySet).
        let _ = CapabilitySet::PURE;

        Self {
            int,
            float,
            bool: bool_,
            text,
            unit,
            timestamp,
            list,
            map,
            set,
            option,
            result,
            handle,
            error,
            duration,
            proc_output,
            ordering,
            json_value,
            sql,
            html,
            secure_cookie,
            sql_value,
            column,
            table,
            field_schema,
            schema,
            q_expr,
            quote,
            fns,
            ret,
            rows,
            joincond,
            joinresult,
            left_joinresult,
            right_joinresult,
            full_joinresult,
            insert_shape,
            decimal,
            uuid,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_arena_with_builtins() -> (TyConArena, BuiltinTyCons) {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        (arena, b)
    }

    // ── TyConId uniqueness ────────────────────────────────────────────────────

    #[test]
    fn fifteen_distinct_ids() {
        let (_, b) = make_arena_with_builtins();
        let ids = [
            b.int,
            b.float,
            b.bool,
            b.text,
            b.unit,
            b.timestamp,
            b.list,
            b.map,
            b.set,
            b.option,
            b.result,
            b.handle,
            b.error,
            b.duration,
            b.proc_output,
        ];
        // All 15 ids must be distinct.
        let mut seen = std::collections::HashSet::new();
        for id in &ids {
            assert!(seen.insert(id.0), "duplicate TyConId: {}", id.0);
        }
        assert_eq!(seen.len(), 15);
    }

    #[test]
    fn ids_match_spec_order() {
        let (_, b) = make_arena_with_builtins();
        assert_eq!(b.int.0, 0);
        assert_eq!(b.float.0, 1);
        assert_eq!(b.bool.0, 2);
        assert_eq!(b.text.0, 3);
        assert_eq!(b.unit.0, 4);
        assert_eq!(b.timestamp.0, 5);
        assert_eq!(b.list.0, 6);
        assert_eq!(b.map.0, 7);
        assert_eq!(b.set.0, 8);
        assert_eq!(b.option.0, 9);
        assert_eq!(b.result.0, 10);
        assert_eq!(b.handle.0, 11);
        assert_eq!(b.error.0, 12);
        assert_eq!(b.duration.0, 13);
        assert_eq!(b.proc_output.0, 14);
    }

    #[test]
    fn int_ne_float() {
        let (_, b) = make_arena_with_builtins();
        assert_ne!(b.int, b.float);
    }

    #[test]
    fn list_ne_option() {
        let (_, b) = make_arena_with_builtins();
        assert_ne!(b.list, b.option);
    }

    #[test]
    fn arena_len_is_53() {
        // 15 original builtins + Ordering + JsonValue + the std.net.http taint
        // wrappers Sql / Html / SecureCookie + std.sql's SqlValue + the
        // column-codegen builtins Column / Table + the schema-codegen builtins
        // FieldSchema / Schema + the quotation builtins QExpr / Quote (27 total)
        // + the 16 synthetic function-type constructors Fn/0 … Fn/15 + Ret/1 +
        // Rows/1 + JoinCond/2 + the four join-result extractors (Join/Left/Right/Full)
        // + InsertShape/1 + the Decimal and Uuid primitives (interned last).
        let (arena, _) = make_arena_with_builtins();
        assert_eq!(arena.len(), 27 + FN_ARITY_COUNT + 10);
        assert_eq!(arena.len(), 53);
    }

    #[test]
    fn fn_tycons_are_arity_keyed_and_contiguous() {
        let (arena, b) = make_arena_with_builtins();
        // Fn/0 = 27 … Fn/15 = 42, named "Fn0" … "Fn15", all Builtin-kind.
        for (n, &id) in b.fns.iter().enumerate() {
            let n_u32 = u32::try_from(n).unwrap();
            assert_eq!(id.0, FN_TYCON_BASE + n_u32);
            assert_eq!(fn_tycon_id(n), Some(id));
            assert_eq!(fn_tycon_arity(id), Some(n));
            let decl = arena.get(id);
            assert_eq!(decl.name, fn_tycon_name(n));
            assert_eq!(decl.name, format!("Fn{n}"));
            assert!(matches!(decl.kind, TyConKind::Builtin));
            assert!(decl.def_module_raw.is_none());
        }
        // Out of range → None, both directions.
        let count_u32 = u32::try_from(FN_ARITY_COUNT).unwrap();
        assert_eq!(fn_tycon_id(FN_ARITY_COUNT), None);
        assert_eq!(fn_tycon_arity(TyConId(FN_TYCON_BASE - 1)), None);
        assert_eq!(fn_tycon_arity(TyConId(FN_TYCON_BASE + count_u32)), None);
    }

    #[test]
    fn column_is_record_arity_2() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.column);
        assert_eq!(decl.name, "Column");
        assert_eq!(decl.arity, 2);
        assert!(!decl.opaque);
        let TyConKind::Record(schema) = &decl.kind else {
            panic!("Column should be a record");
        };
        let fields: Vec<&str> = schema
            .record_fields()
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(fields, vec!["name", "table"]);
    }

    #[test]
    fn table_is_record_arity_1() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.table);
        assert_eq!(decl.name, "Table");
        assert_eq!(decl.arity, 1);
        assert!(!decl.opaque);
        let TyConKind::Record(schema) = &decl.kind else {
            panic!("Table should be a record");
        };
        let fields: Vec<&str> = schema
            .record_fields()
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(fields, vec!["name", "columns"]);
    }

    #[test]
    fn field_schema_is_record_arity_0() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.field_schema);
        assert_eq!(decl.name, "FieldSchema");
        assert_eq!(decl.arity, 0);
        assert!(!decl.opaque);
        let TyConKind::Record(schema) = &decl.kind else {
            panic!("FieldSchema should be a record");
        };
        let fields: Vec<&str> = schema
            .record_fields()
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(fields, vec!["name", "column", "ty", "optional"]);
    }

    #[test]
    fn schema_is_record_arity_0() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.schema);
        assert_eq!(decl.name, "Schema");
        assert_eq!(decl.arity, 0);
        assert!(!decl.opaque);
        let TyConKind::Record(schema) = &decl.kind else {
            panic!("Schema should be a record");
        };
        let fields: Vec<&str> = schema
            .record_fields()
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(fields, vec!["name", "table", "fields"]);
        // `fields` is `List FieldSchema`.
        let fields_ty = &schema.record_fields()[2].ty;
        assert!(
            matches!(fields_ty, Type::Con(id, args)
                if *id == b.list && matches!(args.first(), Some(Type::Con(fs, _)) if *fs == b.field_schema)),
            "Schema.fields must be List FieldSchema, got {fields_ty:?}"
        );
    }

    // ── Arena get() round-trip ────────────────────────────────────────────────

    #[test]
    fn arena_get_int_name() {
        let (arena, b) = make_arena_with_builtins();
        assert_eq!(arena.get(b.int).name, "Int");
    }

    #[test]
    fn arena_get_option_is_union() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.option);
        assert!(matches!(decl.kind, TyConKind::Union(_)));
        assert_eq!(decl.arity, 1);
    }

    #[test]
    fn arena_get_result_is_union() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.result);
        assert!(matches!(decl.kind, TyConKind::Union(_)));
        assert_eq!(decl.arity, 2);
    }

    #[test]
    fn arena_get_list_is_builtin() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.list);
        assert!(matches!(decl.kind, TyConKind::Builtin));
        assert_eq!(decl.arity, 1);
    }

    #[test]
    fn arena_get_map_is_builtin_arity_2() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.map);
        assert!(matches!(decl.kind, TyConKind::Builtin));
        assert_eq!(decl.arity, 2);
    }

    #[test]
    fn arena_get_handle_is_builtin_arity_1() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.handle);
        assert!(matches!(decl.kind, TyConKind::Builtin));
        assert_eq!(decl.arity, 1);
    }

    #[test]
    fn option_schema_has_some_and_none() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.option);
        if let TyConKind::Union(schema) = &decl.kind {
            assert_eq!(schema.variants.len(), 2);
            assert_eq!(schema.variants[0].name, "Some");
            assert_eq!(schema.variants[1].name, "None");
        } else {
            panic!("Option must be a Union TyCon");
        }
    }

    #[test]
    fn result_schema_has_ok_and_err() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.result);
        if let TyConKind::Union(schema) = &decl.kind {
            assert_eq!(schema.variants.len(), 2);
            assert_eq!(schema.variants[0].name, "Ok");
            assert_eq!(schema.variants[1].name, "Err");
        } else {
            panic!("Result must be a Union TyCon");
        }
    }

    #[test]
    fn error_schema_has_code_and_message() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.error);
        assert_eq!(decl.name, "Error");
        assert_eq!(decl.arity, 0);
        if let TyConKind::Record(schema) = &decl.kind {
            let fields = schema.record_fields();
            assert_eq!(fields.len(), 2, "Error must have 2 fields");
            assert_eq!(fields[0].name, "code");
            assert_eq!(fields[1].name, "message");
            assert!(
                matches!(fields[0].ty, Type::Con(id, _) if id == b.text),
                "code must be Text"
            );
            assert!(
                matches!(fields[1].ty, Type::Con(id, _) if id == b.text),
                "message must be Text"
            );
        } else {
            panic!("Error must be a Record TyCon");
        }
    }

    #[test]
    fn duration_schema_has_ms() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.duration);
        assert_eq!(decl.name, "Duration");
        assert_eq!(decl.arity, 0);
        if let TyConKind::Record(schema) = &decl.kind {
            let fields = schema.record_fields();
            assert_eq!(fields.len(), 1, "Duration must have 1 field");
            assert_eq!(fields[0].name, "ms");
            assert!(
                matches!(fields[0].ty, Type::Con(id, _) if id == b.int),
                "ms must be Int"
            );
        } else {
            panic!("Duration must be a Record TyCon");
        }
    }

    #[test]
    fn proc_output_schema_has_stdout_stderr_exit_code() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.proc_output);
        assert_eq!(decl.name, "ProcOutput");
        assert_eq!(decl.arity, 0);
        if let TyConKind::Record(schema) = &decl.kind {
            let fields = schema.record_fields();
            assert_eq!(fields.len(), 3, "ProcOutput must have 3 fields");
            assert_eq!(fields[0].name, "stdout");
            assert_eq!(fields[1].name, "stderr");
            assert_eq!(fields[2].name, "exitCode");
            assert!(
                matches!(fields[0].ty, Type::Con(id, _) if id == b.text),
                "stdout must be Text"
            );
            assert!(
                matches!(fields[1].ty, Type::Con(id, _) if id == b.text),
                "stderr must be Text"
            );
            assert!(
                matches!(fields[2].ty, Type::Con(id, _) if id == b.int),
                "exitCode must be Int"
            );
        } else {
            panic!("ProcOutput must be a Record TyCon");
        }
    }

    #[test]
    fn json_value_is_union_with_seven_variants() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.json_value);
        assert_eq!(decl.name, "JsonValue");
        assert_eq!(decl.arity, 0);
        assert_eq!(b.json_value.0, 16);
        if let TyConKind::Union(schema) = &decl.kind {
            let names: Vec<&str> = schema.variants.iter().map(|v| v.name.as_str()).collect();
            assert_eq!(
                names,
                vec!["JNull", "JBool", "JInt", "JFloat", "JText", "JList", "JObject"]
            );
            // JNull is nullary; JList/JObject are self-referential.
            assert!(matches!(schema.variants[0].kind, VariantPayload::Nullary));
        } else {
            panic!("JsonValue must be a Union TyCon");
        }
    }

    #[test]
    fn primitives_have_arity_zero() {
        let (arena, b) = make_arena_with_builtins();
        for id in [
            b.int,
            b.float,
            b.bool,
            b.text,
            b.unit,
            b.timestamp,
            b.error,
            b.duration,
            b.proc_output,
        ] {
            let decl = arena.get(id);
            assert_eq!(decl.arity, 0, "{} must have arity 0", decl.name);
        }
    }

    #[test]
    fn all_def_spans_are_none() {
        let (arena, _) = make_arena_with_builtins();
        for decl in arena.all() {
            assert!(
                decl.def_span.is_none(),
                "{} must have no def_span (built-in)",
                decl.name
            );
        }
    }
}
