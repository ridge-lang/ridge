//! Symbol references and constructor kinds.
// OQ-IR003: SymbolRef is #[non_exhaustive] — see expr.rs for rationale.

use ridge_resolve::ModuleId;
use ridge_types::{ClassId, TyConId};

/// Opaque cross-module symbol reference.
///
/// `Local` is a same-module reference (top-level fn / actor). `Stdlib` is a
/// reference to a stdlib symbol resolved by Phase 3 (`std.list.map`,
/// `std.option.withDefault`, …). `External` is a reference to a `pub`-exported
/// symbol in a different project (gated by `D076 exported_externally`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SymbolRef {
    /// Same-module top-level fn / const / actor.
    Local {
        /// The symbol's source-level name.
        name: String,
        /// The module this symbol belongs to.
        module: ModuleId,
    },
    // OQ-L006: SymbolRef::Stdlib.module is String (not an interned id) for
    // debuggability; the stdlib path set is small and never hot-path hashed.
    /// A stdlib symbol resolved against `BUILTINS` (Phase 3).
    Stdlib {
        /// The stdlib module path (e.g. `"std.list"`).
        module: String,
        /// The symbol name within the stdlib module.
        name: String,
    },
    /// A `pub` symbol from another project.
    External {
        /// The external module's stable index.
        module: ModuleId,
        /// The exported symbol name.
        name: String,
    },
    /// An actor-handler reference: `(actor_module, actor_name, handler_name)`.
    Handler {
        /// The module containing the actor declaration.
        actor_module: ModuleId,
        /// The actor's source-level name.
        actor: String,
        /// The handler's source-level name (the `on m` tag).
        handler: String,
    },
    /// An actor type reference (used in spawn).
    ActorType {
        /// The module containing the actor declaration.
        module: ModuleId,
        /// The actor's source-level name.
        name: String,
    },
    /// A constructor (record-auto or union-variant). Kind is encoded by
    /// `ctor_kind` so backends can tell records from unions without a `TyCon` lookup.
    Constructor {
        /// Whether this is a record auto-constructor or a union variant constructor.
        ctor_kind: CtorKind,
        /// The type-constructor that owns this constructor.
        owner_type: TyConId,
        /// The constructor's source-level name.
        name: String,
        /// The variant index within the union (0 for records).
        variant: u32,
    },
    /// A binding from the implicit prelude (Some, None, Ok, Err, Option, Result).
    Prelude {
        /// The prelude symbol name.
        name: String,
    },

    /// An unresolved class-method reference.
    ///
    /// Emitted by the lowering pass for a method call inside a constrained
    /// function body, before the call site is resolved against the dictionary
    /// parameter. The codegen layer never sees this variant — it must be
    /// rewritten to a `Field` projection over the in-scope dict value before
    /// emission.
    ///
    /// If this variant reaches codegen it indicates a lowering invariant
    /// violation.
    Method {
        /// The class that declares this method.
        class: ClassId,
        /// The method name (e.g. `"toText"`, `"eq"`, `"compare"`).
        method: String,
    },
}

/// The kind of a constructor `SymbolRef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CtorKind {
    /// Auto-constructor for a record type (a single-variant union, variant 0).
    Record,
    /// A user-declared union variant.
    UnionVariant,
}
