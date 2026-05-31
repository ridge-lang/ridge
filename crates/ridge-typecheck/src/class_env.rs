//! Class and instance registries: [`ClassTable`] and [`InstanceEnv`].
//!
//! The [`ClassTable`] maps class names to [`ClassInfo`] records. The
//! [`InstanceEnv`] holds exactly one [`InstanceInfo`] per `(ClassId, TyConId)`
//! pair — the structural encoding of Haskell-98 coherence. A second insert for
//! the same key is a coherence violation.
//!
//! Both live on the workspace typecheck pass, not on a per-module [`crate::ctx::InferCtx`].
//! Coherence is workspace-wide: an instance declared anywhere in the workspace
//! participates in the global uniqueness requirement.

use ridge_ast::Span;
use ridge_types::{ClassId, Constraint, TyConId, EQ_CLASS, ORD_CLASS, TOTEXT_CLASS};
use rustc_hash::FxHashMap;

use crate::error::TypeError;

// ── MethodSig (registry-level) ────────────────────────────────────────────────

/// A method signature stored in the class registry.
///
/// This mirrors the shape of [`ridge_ast::typeclass::MethodSig`] but is
/// owned and stored in the registry independently of the AST lifetime.
#[derive(Debug, Clone)]
pub struct MethodSig {
    /// Method name.
    pub name: String,
    /// Number of parameters (arity).
    pub arity: usize,
}

// ── ClassInfo ────────────────────────────────────────────────────────────────

/// Metadata for a registered typeclass.
#[derive(Debug, Clone)]
pub struct ClassInfo {
    /// Canonical class name (e.g. `"ToText"`, `"Eq"`, `"Ord"`).
    pub name: String,
    /// Method signatures declared in the class body.
    pub method_sigs: Vec<MethodSig>,
    /// Immediate superclass ids (e.g. `Ord` has `[EQ_CLASS]`).
    pub superclasses: Vec<ClassId>,
    /// The module that declared this class, or `None` for prelude classes.
    pub def_module: Option<u32>,
}

// ── ClassTable ────────────────────────────────────────────────────────────────

/// Workspace-level class registry: name → [`ClassId`] + [`ClassInfo`].
///
/// [`ClassId`]s are allocated sequentially. The three prelude classes
/// (`ToText`, `Eq`, `Ord`) have fixed ids reserved by the constants in
/// [`ridge_types`]: `TOTEXT_CLASS=0`, `EQ_CLASS=1`, `ORD_CLASS=2`.
#[derive(Debug, Default)]
pub struct ClassTable {
    /// Id → class information.
    classes: FxHashMap<ClassId, ClassInfo>,
    /// Name → id index for O(1) name lookup.
    by_name: FxHashMap<String, ClassId>,
    /// Next id to allocate (starts at 3, below that are prelude constants).
    next_id: u32,
}

impl ClassTable {
    /// Returns a new, empty [`ClassTable`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            classes: FxHashMap::default(),
            by_name: FxHashMap::default(),
            next_id: 3, // 0, 1, 2 reserved for prelude constants
        }
    }

    /// Interns a class name, returning its [`ClassId`].
    ///
    /// If the name already exists the existing id is returned unchanged
    /// (idempotent). New names are allocated sequentially starting from 3;
    /// ids 0–2 are reserved for the prelude constants and must be registered
    /// explicitly via [`ClassTable::insert_with_id`].
    #[must_use]
    pub fn intern(&mut self, name: &str) -> ClassId {
        if let Some(&id) = self.by_name.get(name) {
            return id;
        }
        let id = ClassId(self.next_id);
        self.next_id += 1;
        self.by_name.insert(name.to_string(), id);
        id
    }

    /// Inserts or replaces a class entry with a specific [`ClassId`].
    ///
    /// Used when registering prelude classes at their reserved ids (0–2).
    /// Also registers the name → id mapping.
    pub fn insert_with_id(&mut self, id: ClassId, info: ClassInfo) {
        self.by_name.insert(info.name.clone(), id);
        self.classes.insert(id, info);
    }

    /// Looks up a [`ClassId`] by name.
    #[must_use]
    pub fn id_by_name(&self, name: &str) -> Option<ClassId> {
        self.by_name.get(name).copied()
    }

    /// Looks up [`ClassInfo`] by [`ClassId`].
    #[must_use]
    pub fn get(&self, id: ClassId) -> Option<&ClassInfo> {
        self.classes.get(&id)
    }

    /// Returns `true` if the table contains the given [`ClassId`].
    #[must_use]
    pub fn contains(&self, id: ClassId) -> bool {
        self.classes.contains_key(&id)
    }

    /// Returns an iterator over all `(ClassId, &ClassInfo)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (ClassId, &ClassInfo)> {
        self.classes.iter().map(|(&id, info)| (id, info))
    }
}

// ── InstanceOrigin ────────────────────────────────────────────────────────────

/// How an instance entered the [`InstanceEnv`].
///
/// Used by the coherence duplicate-key check to differentiate between:
/// - Two explicit `instance C T` declarations (→ T032 `OverlappingInstance`).
/// - An auto-promoted `pub fn toText` conflicting with an explicit
///   `instance ToText T` (→ T034 `ToTextConflict`).
///
/// This flag routes duplicate inserts to the correct error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceOrigin {
    /// Written by the user as `instance C T = …`.
    Explicit,
    /// Synthesized from a `pub fn toText (x: T) -> Text` declaration.
    AutoPromoted,
}

// ── InstanceInfo ─────────────────────────────────────────────────────────────

/// Metadata for a registered typeclass instance.
#[derive(Debug, Clone)]
pub struct InstanceInfo {
    /// The module that declared this instance, or `None` for prelude instances.
    pub def_module: Option<u32>,
    /// Method name → symbol (placeholder; dictionary lowering fills in real
    /// `SymbolRef`s).
    pub methods: Vec<(String, String)>,
    /// Constraints required by the instance's method bodies (for parametric
    /// instances — always empty in 0.2.13 single-param non-generic instances).
    pub ctx_constraints: Vec<Constraint>,
    /// How this instance was created.
    pub origin: InstanceOrigin,
    /// Source span of the `instance` declaration (for error messages).
    pub span: Span,
}

// ── CoherenceError ────────────────────────────────────────────────────────────

/// A coherence violation detected during [`InstanceEnv::insert`].
///
/// Callers convert these into the appropriate [`TypeError`] variant.
#[derive(Debug)]
pub enum CoherenceError {
    /// Two explicit instances for the same `(ClassId, TyConId)` pair (T032).
    OverlappingInstance {
        /// Display name of the class.
        class_name: String,
        /// Display name of the type.
        type_name: String,
        /// Span of the first (prior) instance declaration.
        first_span: Span,
        /// Span of the second (conflicting) instance declaration.
        second_span: Span,
    },
    /// An auto-promoted `pub fn toText` conflicts with an explicit
    /// `instance ToText T` (T034).
    ToTextConflict {
        /// Display name of the type.
        type_name: String,
        /// Span of the explicit `instance ToText T` declaration.
        totext_span: Span,
        /// Span of the `pub fn toText` declaration that was auto-promoted.
        auto_promote_span: Span,
    },
}

impl CoherenceError {
    /// Converts this coherence error into the corresponding [`TypeError`].
    #[must_use]
    pub fn into_type_error(self) -> TypeError {
        match self {
            Self::OverlappingInstance {
                class_name,
                type_name,
                first_span,
                second_span,
            } => TypeError::OverlappingInstance {
                class: class_name,
                ty: type_name,
                first_span,
                second_span,
            },
            Self::ToTextConflict {
                type_name,
                totext_span,
                auto_promote_span,
            } => TypeError::ToTextConflict {
                ty: type_name,
                totext_span,
                auto_promote_span,
            },
        }
    }
}

// ── InstanceEnv ───────────────────────────────────────────────────────────────

/// Workspace-level instance registry.
///
/// The single-value-per-key `(ClassId, TyConId) → InstanceInfo` map IS the
/// Haskell-98 coherence constraint: at most one instance per `(class, type)` pair.
/// A second insert for the same key returns a [`CoherenceError`].
#[derive(Debug, Default)]
pub struct InstanceEnv {
    /// The canonical instance map.
    pub instances: FxHashMap<(ClassId, TyConId), InstanceInfo>,
}

impl InstanceEnv {
    /// Returns a new, empty [`InstanceEnv`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            instances: FxHashMap::default(),
        }
    }

    /// Inserts a new instance, or returns a [`CoherenceError`] on conflict.
    ///
    /// Conflict routing (reconciliation item #1):
    /// - Explicit vs Explicit → T032 [`CoherenceError::OverlappingInstance`].
    /// - `AutoPromoted` vs `Explicit` (or vice versa) → T034
    ///   [`CoherenceError::ToTextConflict`].
    ///
    /// `class_name` and `type_name` are display strings for error messages.
    pub fn insert(
        &mut self,
        key: (ClassId, TyConId),
        info: InstanceInfo,
        class_name: &str,
        type_name: &str,
    ) -> Result<(), CoherenceError> {
        if let Some(existing) = self.instances.get(&key) {
            let (first_span, second_span) = (existing.span, info.span);
            let one_auto = existing.origin == InstanceOrigin::AutoPromoted
                || info.origin == InstanceOrigin::AutoPromoted;
            let conflict = if one_auto {
                // Determine which span belongs to the pub fn toText side.
                let (totext_span, auto_promote_span) =
                    if existing.origin == InstanceOrigin::AutoPromoted {
                        (second_span, first_span)
                    } else {
                        (first_span, second_span)
                    };
                CoherenceError::ToTextConflict {
                    type_name: type_name.to_string(),
                    totext_span,
                    auto_promote_span,
                }
            } else {
                CoherenceError::OverlappingInstance {
                    class_name: class_name.to_string(),
                    type_name: type_name.to_string(),
                    first_span,
                    second_span,
                }
            };
            return Err(conflict);
        }
        self.instances.insert(key, info);
        Ok(())
    }

    /// Looks up an instance by `(ClassId, TyConId)`.
    #[must_use]
    pub fn get(&self, key: (ClassId, TyConId)) -> Option<&InstanceInfo> {
        self.instances.get(&key)
    }
}

// ── Prelude class registration ────────────────────────────────────────────────

/// Registers the three built-in prelude classes (`ToText`, `Eq`, `Ord`) into
/// `class_table` at their reserved [`ClassId`]s (0–2).
///
/// Must be called once before the workspace collect pass so that user-declared
/// class and instance items can reference these classes by name.
pub fn register_prelude_classes(ct: &mut ClassTable) {
    // ToText (id=0) — no superclasses; one method: toText
    ct.insert_with_id(
        TOTEXT_CLASS,
        ClassInfo {
            name: "ToText".to_string(),
            method_sigs: vec![MethodSig {
                name: "toText".to_string(),
                arity: 1,
            }],
            superclasses: vec![],
            def_module: None, // prelude — no module id
        },
    );

    // Eq (id=1) — no superclasses; one method: eq
    ct.insert_with_id(
        EQ_CLASS,
        ClassInfo {
            name: "Eq".to_string(),
            method_sigs: vec![MethodSig {
                name: "eq".to_string(),
                arity: 2,
            }],
            superclasses: vec![],
            def_module: None,
        },
    );

    // Ord (id=2) — superclass: Eq; one method: compare
    ct.insert_with_id(
        ORD_CLASS,
        ClassInfo {
            name: "Ord".to_string(),
            method_sigs: vec![MethodSig {
                name: "compare".to_string(),
                arity: 2,
            }],
            superclasses: vec![EQ_CLASS],
            def_module: None,
        },
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_types::{EQ_CLASS, ORD_CLASS, TOTEXT_CLASS};

    fn dummy_span() -> Span {
        Span::point(0)
    }

    fn make_instance(origin: InstanceOrigin) -> InstanceInfo {
        InstanceInfo {
            def_module: None,
            methods: vec![],
            ctx_constraints: vec![],
            origin,
            span: dummy_span(),
        }
    }

    // ── ClassTable::intern is idempotent ──────────────────────────────────────

    #[test]
    fn intern_idempotent() {
        let mut ct = ClassTable::new();
        let id1 = ct.intern("MyClass");
        let id2 = ct.intern("MyClass");
        assert_eq!(id1, id2, "intern must return the same id for the same name");
    }

    #[test]
    fn intern_two_names_produce_distinct_ids() {
        let mut ct = ClassTable::new();
        let id1 = ct.intern("Foo");
        let id2 = ct.intern("Bar");
        assert_ne!(id1, id2);
    }

    // ── register_prelude_classes populates the table correctly ────────────────

    #[test]
    fn prelude_classes_registered() {
        let mut ct = ClassTable::new();
        register_prelude_classes(&mut ct);

        assert_eq!(ct.id_by_name("ToText"), Some(TOTEXT_CLASS));
        assert_eq!(ct.id_by_name("Eq"), Some(EQ_CLASS));
        assert_eq!(ct.id_by_name("Ord"), Some(ORD_CLASS));

        let ord_info = ct.get(ORD_CLASS).expect("Ord must be in ClassTable");
        assert_eq!(ord_info.superclasses, vec![EQ_CLASS]);
    }

    // ── InstanceEnv::insert duplicate → OverlappingInstance (T032) ───────────

    #[test]
    fn insert_duplicate_explicit_explicit_returns_err_t032() {
        let mut env = InstanceEnv::new();
        let key = (TOTEXT_CLASS, TyConId(0));

        let r1 = env.insert(
            key,
            make_instance(InstanceOrigin::Explicit),
            "ToText",
            "Color",
        );
        assert!(r1.is_ok(), "first insert must succeed");

        let r2 = env.insert(
            key,
            make_instance(InstanceOrigin::Explicit),
            "ToText",
            "Color",
        );
        assert!(
            matches!(r2, Err(CoherenceError::OverlappingInstance { .. })),
            "two explicit inserts must produce OverlappingInstance, got {r2:?}"
        );
    }

    // ── InstanceEnv::insert auto-promoted vs explicit → T034 ─────────────────

    #[test]
    fn insert_auto_promoted_and_explicit_returns_err_t034() {
        let mut env = InstanceEnv::new();
        let key = (TOTEXT_CLASS, TyConId(1));

        // First: auto-promoted (from pub fn toText)
        let r1 = env.insert(
            key,
            make_instance(InstanceOrigin::AutoPromoted),
            "ToText",
            "User",
        );
        assert!(r1.is_ok());

        // Second: explicit instance ToText User
        let r2 = env.insert(
            key,
            make_instance(InstanceOrigin::Explicit),
            "ToText",
            "User",
        );
        assert!(
            matches!(r2, Err(CoherenceError::ToTextConflict { .. })),
            "auto-promoted + explicit must produce ToTextConflict, got {r2:?}"
        );
    }

    #[test]
    fn insert_explicit_then_auto_promoted_returns_err_t034() {
        let mut env = InstanceEnv::new();
        let key = (TOTEXT_CLASS, TyConId(2));

        // First: explicit
        let r1 = env.insert(
            key,
            make_instance(InstanceOrigin::Explicit),
            "ToText",
            "Order",
        );
        assert!(r1.is_ok());

        // Second: auto-promoted
        let r2 = env.insert(
            key,
            make_instance(InstanceOrigin::AutoPromoted),
            "ToText",
            "Order",
        );
        assert!(
            matches!(r2, Err(CoherenceError::ToTextConflict { .. })),
            "explicit + auto-promoted must produce ToTextConflict, got {r2:?}"
        );
    }

    // ── Single insert succeeds and is retrievable ─────────────────────────────

    #[test]
    fn insert_single_then_get() {
        let mut env = InstanceEnv::new();
        let key = (EQ_CLASS, TyConId(5));
        env.insert(key, make_instance(InstanceOrigin::Explicit), "Eq", "Foo")
            .expect("single insert must succeed");
        assert!(env.get(key).is_some());
    }

    // ── Only auto-promoted — no conflict ─────────────────────────────────────

    #[test]
    fn auto_promote_no_conflict() {
        let mut env = InstanceEnv::new();
        let key = (TOTEXT_CLASS, TyConId(3));
        let result = env.insert(
            key,
            make_instance(InstanceOrigin::AutoPromoted),
            "ToText",
            "Widget",
        );
        assert!(result.is_ok(), "single auto-promoted insert must not fail");
        assert!(env.get(key).is_some());
    }
}
