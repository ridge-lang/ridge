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
        /// Whether naming this symbol references a function or evaluates a
        /// value. See [`StdlibKind`] — arity cannot answer it.
        kind: StdlibKind,
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
        /// The type-constructor that owns this constructor, when there is one.
        ///
        /// `None` for constructors with no nominal owner: inline record
        /// literals, which the type checker infers structurally, and the
        /// synthesised dictionary and instance values, which are plain maps in
        /// the IR. Those cases used to write `TyConId(0)` and mean "unowned",
        /// which reads back as whichever type the arena happens to intern
        /// first — so an anonymous record was labelled `Int`, and any consumer
        /// indexing the arena with it got a confident wrong answer.
        owner_type: Option<TyConId>,
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

/// What naming a stdlib [`SymbolRef`] means.
///
/// A stdlib declaration is either a function, so naming it is a reference, or a
/// value, so naming it is that value. Arity cannot separate the two:
/// `std.time.now` is `() -> Timestamp` and `std.map.empty` is `Map k v`, and
/// both take nothing. Codegen read the arity and evaluated every one of them,
/// so a nullary stdlib function handed to a higher-order function arrived as
/// its own result and the runtime reported `badfun`.
///
/// This rides on the symbol for the same reason [`CtorKind`] does: it is a fact
/// about the program that every backend needs, and one no backend can work out
/// on its own — the answer is in the type checker's scheme, which the IR is the
/// contract for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StdlibKind {
    /// The symbol's scheme is a function type. Naming it is a reference to the
    /// function; `Function` with no parameters is `fn f () -> T`.
    Function,
    /// The symbol's scheme is the result type, not a function type. Naming it
    /// is that value, so the reference is evaluated where it appears.
    ///
    /// `std.list.empty`, `std.map.empty` and `std.set.empty` are the declared
    /// ones; the generated instance-dictionary constants of a stdlib typeclass
    /// (`$inst_SqlType_Int`) are the synthesised ones.
    Constant,
}

/// The kind of a constructor `SymbolRef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CtorKind {
    /// Auto-constructor for a record type (a single-variant union, variant 0).
    Record,
    /// A user-declared union variant.
    UnionVariant,
}
