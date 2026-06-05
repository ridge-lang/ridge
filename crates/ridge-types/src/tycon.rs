//! Type-constructor declarations: [`TyConId`], [`TyConDecl`], [`TyConKind`],
//! [`RecordSchema`], [`UnionSchema`], [`ActorSchema`], and [`TyConArena`].

use ridge_ast::Span;

use crate::{
    capability_set::CapabilitySet,
    ty::{TyVid, Type},
};

// ── TyConId ───────────────────────────────────────────────────────────────────

/// Type-constructor identifier. Stable across modules in the same workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TyConId(pub u32);

// ── TyConArena ────────────────────────────────────────────────────────────────

/// Storage for all [`TyConDecl`]s allocated during a workspace type-check.
///
/// Built-in `TyCons` are registered first (indices 0..11) by
/// [`crate::BuiltinTyCons::allocate`] (T3). User-defined `TyCons` are appended
/// during the workspace pre-pass (T2 / T3 `tycon_collect`).
///
/// # Indexing invariant
///
/// For every valid index `i`, `arena.get(TyConId(i as u32)).id == TyConId(i as u32)`.
#[derive(Debug, Default)]
pub struct TyConArena {
    decls: Vec<TyConDecl>,
}

impl TyConArena {
    /// Creates an empty arena.
    #[must_use]
    pub const fn new() -> Self {
        Self { decls: Vec::new() }
    }

    /// Interns a [`TyConDecl`], assigning it the next available [`TyConId`]
    /// and storing it in the arena.
    ///
    /// The `decl.id` field is overwritten with the freshly-assigned id so
    /// callers do not need to pre-compute it. Returns the stable [`TyConId`].
    #[expect(
        clippy::cast_possible_truncation,
        reason = "TyCon arena is bounded by user TyCon count; exceeding 2^32 entries is not a realistic concern"
    )]
    pub fn intern(&mut self, mut decl: TyConDecl) -> TyConId {
        let id = TyConId(self.decls.len() as u32);
        decl.id = id;
        self.decls.push(decl);
        id
    }

    /// Retrieves a [`TyConDecl`] by its [`TyConId`].
    ///
    /// # Panics
    ///
    /// Panics if `id.0` is out of bounds (defensive; indicates a bug in the
    /// allocator or a stale id from a different arena).
    #[must_use]
    pub fn get(&self, id: TyConId) -> &TyConDecl {
        &self.decls[id.0 as usize]
    }

    /// Returns the number of registered `TyCons`.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.decls.len()
    }

    /// Returns `true` if no `TyCons` have been interned yet.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.decls.is_empty()
    }

    /// Returns a slice over all registered [`TyConDecl`]s, in allocation order.
    #[must_use]
    pub fn all(&self) -> &[TyConDecl] {
        &self.decls
    }

    /// Replaces the `kind` of an already-interned [`TyConDecl`].
    ///
    /// Used during two-pass user-`TyCon` collection: pass 1 interns
    /// placeholders so every type name has a stable [`TyConId`] before
    /// pass 2 starts resolving field types (which may forward-reference
    /// later declarations); pass 2 builds the real schema and writes it
    /// back via this call.
    ///
    /// # Panics
    ///
    /// Panics if `id.0` is out of bounds.
    pub fn replace_kind(&mut self, id: TyConId, new_kind: TyConKind) {
        self.decls[id.0 as usize].kind = new_kind;
    }
}

// ── TyConDecl ─────────────────────────────────────────────────────────────────

/// A type-constructor descriptor.
#[derive(Debug, Clone)]
pub struct TyConDecl {
    /// Stable numeric identifier.
    pub id: TyConId,
    /// Human-readable name (e.g. `"List"`, `"User"`).
    pub name: String,
    /// Number of type-parameter slots (`0` for `Int`, `1` for `List`, `2` for
    /// `Map`/`Result`).
    pub arity: u32,
    /// Structural kind of this type constructor.
    pub kind: TyConKind,
    /// Source span of the declaration; `None` for built-in `TyCons`.
    pub def_span: Option<Span>,
    /// Raw `u32` representation of the [`ModuleId`] that declared this type.
    ///
    /// Stored as a raw integer (not the strongly-typed `ridge_resolve::ModuleId`)
    /// to avoid a layering dependency: `ridge-types` is one of the foundational
    /// crates and should not depend on `ridge-resolve`. Downstream code that
    /// already has access to `ridge-resolve` reconstitutes the strongly-typed
    /// id with `ridge_resolve::ModuleId(def_module_raw.unwrap())`.
    ///
    /// `None` for built-in `TyCons` (no source module) and for stdlib
    /// declarations that bypass the user collect pass.
    pub def_module_raw: Option<u32>,
    /// `true` when the type was declared `opaque`. Field-level access (`.field`,
    /// `with`) of an opaque type is confined to its defining module; reaching a
    /// field from another module is a type error (T036). Always `false` for
    /// built-ins and anonymous inline records.
    pub opaque: bool,
    /// `true` for anonymous `TyCons` minted from inline record types.
    ///
    /// These are interned from `{ field: Type, … }` syntax and have no
    /// user-visible name.  Diagnostic renderers use this flag to switch from
    /// name-based rendering (`Coords`) to structural-shape rendering
    /// (`{ x: Int, y: Int }`).
    pub is_anon: bool,
}

// ── TyConKind ─────────────────────────────────────────────────────────────────

/// The structural kind of a type constructor.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum TyConKind {
    /// Built-in primitive: `Int`, `Float`, `Bool`, `Text`, `Unit`, `Timestamp`.
    Primitive,
    /// User-defined record type.
    Record(RecordSchema),
    /// User-defined union type (including built-in `Option` / `Result`).
    Union(UnionSchema),
    /// A type alias, eagerly resolved to its RHS.
    ///
    /// `params` are the alias's own type-parameter slots (empty for
    /// non-parametric aliases like `type Bag = List Int`).  `body` is the
    /// expanded RHS and may contain `Type::Var(p)` for each `p` in `params`.
    /// At use sites callers substitute the `params` with the supplied
    /// argument types before wrapping in `Type::Alias { name, body }` for
    /// diagnostic naming.
    Alias {
        /// Fresh `TyVid`s standing in for the alias's declared parameters.
        params: Vec<TyVid>,
        /// The expanded RHS, with `Type::Var(p)` placeholders for `params`.
        body: Type,
    },
    /// Actor type — `Handle X` instantiates this.
    Actor(ActorSchema),
    /// Generic built-in container: `List`, `Map`, `Set`, `Handle`.
    ///
    /// These have no user-visible schema; their shapes are opaque.
    Builtin,
}

// ── RecordSchema ──────────────────────────────────────────────────────────────

/// Schema for a record type constructor.
///
/// **`fields` is `pub(crate)`, not `pub`**. External readers
/// use the `record_fields()` accessor on the `ridge-types` crate root.
///
/// # Rationale
///
/// 0.1.0 records are closed (no row polymorphism); 0.2.0 will introduce a row
/// variable on `Type::Con(record_tycon, ..)`. Routing reads through the accessor
/// lets that future change land without touching call sites.
#[derive(Debug, Clone)]
pub struct RecordSchema {
    /// Fresh unification-variable slots for each type parameter.
    pub params: Vec<TyVid>,
    /// Declared field order — read via [`RecordSchema::record_fields`].
    pub(crate) fields: Vec<RecordField>,
}

impl RecordSchema {
    /// Constructs a new `RecordSchema` from type params and declared fields.
    ///
    /// This is the only stable API for constructing a `RecordSchema` from
    /// outside the `ridge-types` crate (forward-compat accessor).
    /// Direct field access to `fields` is `pub(crate)` only.
    #[must_use]
    pub const fn new(params: Vec<TyVid>, fields: Vec<RecordField>) -> Self {
        Self { params, fields }
    }

    /// Returns a slice over the declared record fields.
    ///
    /// This is the only stable API for reading field definitions from outside
    /// the `ridge-types` crate (forward-compat accessor).
    #[must_use]
    pub fn record_fields(&self) -> &[RecordField] {
        &self.fields
    }
}

/// A single field in a [`RecordSchema`].
#[derive(Debug, Clone)]
pub struct RecordField {
    /// Field name.
    pub name: String,
    /// Declared field type.
    pub ty: Type,
}

// ── UnionSchema ───────────────────────────────────────────────────────────────

/// Schema for a union type constructor.
#[derive(Debug, Clone)]
pub struct UnionSchema {
    /// Type-parameter slots.
    pub params: Vec<TyVid>,
    /// Declared variants.
    pub variants: Vec<UnionVariant>,
}

/// A single variant in a [`UnionSchema`].
#[derive(Debug, Clone)]
pub struct UnionVariant {
    /// Variant name (e.g. `"Some"`, `"Err"`, `"Circle"`).
    pub name: String,
    /// Payload shape.
    pub kind: VariantPayload,
}

/// Payload shape for a union variant.
#[derive(Debug, Clone)]
pub enum VariantPayload {
    /// No payload: `None`, `Empty`.
    Nullary,
    /// Positional payload: `Some a`, `Cons a (List a)`.
    Positional(Vec<Type>),
    /// Inline record payload: `Login { userId: Text, at: Timestamp }`.
    Record(RecordSchema),
}

// ── ActorSchema ───────────────────────────────────────────────────────────────

/// Schema for an actor type constructor.
#[derive(Debug, Clone)]
pub struct ActorSchema {
    /// Fields in the actor's state record.
    pub state_fields: Vec<RecordField>,
    /// Init parameters; `None` if the actor has no `init` block (D061).
    pub init_params: Option<Vec<Type>>,
    /// Capabilities required by the `init` block.
    pub init_caps: CapabilitySet,
    /// Declared `on` handlers.
    pub handlers: Vec<HandlerSchema>,
}

/// Schema for a single `on` handler in an actor.
#[derive(Debug, Clone)]
pub struct HandlerSchema {
    /// Handler message name.
    pub name: String,
    /// Parameter types.
    pub params: Vec<Type>,
    /// Return type.
    pub ret: Type,
    /// Capabilities declared on this handler.
    pub caps: CapabilitySet,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::{TyVid, Type};
    use ridge_ast::Span;

    fn dummy_span() -> Span {
        Span::point(0)
    }

    fn mk_record(fields: Vec<(&str, Type)>) -> RecordSchema {
        RecordSchema {
            params: vec![],
            fields: fields
                .into_iter()
                .map(|(n, t)| RecordField {
                    name: n.to_string(),
                    ty: t,
                })
                .collect(),
        }
    }

    // ── TyConDecl round-trip ──────────────────────────────────────────────────

    #[test]
    fn tycon_decl_round_trip() {
        let d = TyConDecl {
            id: TyConId(1),
            name: "User".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: Some(dummy_span()),
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        };
        assert_eq!(d.id.0, 1);
        assert_eq!(d.name, "User");
        assert_eq!(d.arity, 0);
        assert!(d.def_span.is_some());
    }

    // ── TyConKind variants ────────────────────────────────────────────────────

    #[test]
    fn tycon_kind_record() {
        let schema = mk_record(vec![("name", Type::Con(TyConId(0), vec![]))]);
        let kind = TyConKind::Record(schema);
        assert!(matches!(kind, TyConKind::Record(_)));
    }

    #[test]
    fn tycon_kind_union() {
        let schema = UnionSchema {
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
        };
        let kind = TyConKind::Union(schema);
        assert!(matches!(kind, TyConKind::Union(_)));
    }

    #[test]
    fn tycon_kind_alias() {
        let alias_body = Type::Con(TyConId(3), vec![]);
        let kind = TyConKind::Alias {
            params: vec![],
            body: alias_body,
        };
        assert!(matches!(kind, TyConKind::Alias { .. }));
    }

    #[test]
    fn tycon_kind_builtin() {
        let kind = TyConKind::Builtin;
        assert!(matches!(kind, TyConKind::Builtin));
    }

    #[test]
    fn tycon_kind_actor() {
        let schema = ActorSchema {
            state_fields: vec![],
            init_params: None,
            init_caps: CapabilitySet::PURE,
            handlers: vec![],
        };
        let kind = TyConKind::Actor(schema);
        assert!(matches!(kind, TyConKind::Actor(_)));
    }

    // ── RecordSchema field accessor ───────────────────────────────────────────

    #[test]
    fn record_fields_accessor_returns_same_vec() {
        let schema = mk_record(vec![
            ("id", Type::Con(TyConId(0), vec![])),
            ("email", Type::Con(TyConId(1), vec![])),
        ]);
        let fields = schema.record_fields();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "id");
        assert_eq!(fields[1].name, "email");
    }

    #[test]
    fn record_fields_empty() {
        let schema = RecordSchema {
            params: vec![],
            fields: vec![],
        };
        assert!(schema.record_fields().is_empty());
    }

    // ── No-init actor ─────────────────────────────────────────────────────────

    #[test]
    fn actor_schema_no_init() {
        let schema = ActorSchema {
            state_fields: vec![RecordField {
                name: "count".to_string(),
                ty: Type::Con(TyConId(0), vec![]),
            }],
            init_params: None,
            init_caps: CapabilitySet::PURE,
            handlers: vec![HandlerSchema {
                name: "increment".to_string(),
                params: vec![],
                ret: Type::Con(TyConId(4), vec![]), // Unit
                caps: CapabilitySet::PURE,
            }],
        };
        assert!(schema.init_params.is_none());
        assert_eq!(schema.handlers.len(), 1);
    }
}
