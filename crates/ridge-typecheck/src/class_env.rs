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

use ridge_ast::{Span, Type as AstType};
use ridge_types::{
    ClassId, Constraint, TyConId, TyVid, DECODE_CLASS, ENCODE_CLASS, EQ_CLASS, ORD_CLASS,
    TOTEXT_CLASS,
};
use rustc_hash::FxHashMap;
use smallvec::{smallvec, SmallVec};

/// The instance-key head: the tuple of type constructors an instance applies
/// to. Length one for an ordinary class, several for a multi-parameter class.
pub type InstanceHead = SmallVec<[TyConId; 1]>;

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
    /// AST parameter types in declaration order, as written in the class body.
    ///
    /// Populated during the collect pass (pass 2) from the parsed `MethodSig`.
    /// Empty for prelude-registered methods that have no source AST.
    /// Consumers convert these to `ridge_types::Type` at inference time by
    /// substituting the class type variable for a fresh `TyVid`.
    pub ast_param_types: Vec<AstType>,
    /// AST return type as written in the class body.
    ///
    /// `None` for prelude-registered methods that have no source AST.
    pub ast_ret_type: Option<AstType>,
    /// The names of the class type variables (e.g. `["a"]` in `class Describe a`,
    /// `["a", "b"]` in `class Convert a b`).
    ///
    /// Used when converting `ast_param_types`/`ast_ret_type` to ridge types:
    /// every occurrence of one of these names is mapped to a freshly allocated
    /// `TyVid` representing that class type argument at the call site. Empty for
    /// prelude-registered methods that have no source AST.
    pub class_ty_vars: Vec<String>,
}

// ── ClassInfo ────────────────────────────────────────────────────────────────

/// Metadata for a registered typeclass.
#[derive(Debug, Clone)]
pub struct ClassInfo {
    /// Canonical class name (e.g. `"ToText"`, `"Eq"`, `"Ord"`).
    pub name: String,
    /// Number of class type parameters (`1` for an ordinary class, more for a
    /// multi-parameter class such as `Convert a b`). An instance head must
    /// supply exactly this many type atoms.
    pub arity: usize,
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
/// [`ClassId`]s are allocated sequentially. The five prelude classes
/// (`ToText`, `Eq`, `Ord`, `Encode`, `Decode`) have fixed ids reserved by the
/// constants in [`ridge_types`]: `TOTEXT_CLASS=0`, `EQ_CLASS=1`, `ORD_CLASS=2`,
/// `ENCODE_CLASS=3`, `DECODE_CLASS=4`.
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
            next_id: 5, // 0..=4 reserved for prelude constants (ToText/Eq/Ord/Encode/Decode)
        }
    }

    /// Interns a class name, returning its [`ClassId`].
    ///
    /// If the name already exists the existing id is returned unchanged
    /// (idempotent). New names are allocated sequentially starting from 5;
    /// ids 0–4 are reserved for the prelude constants and must be registered
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

    /// Returns the class name that declares `method`, when exactly one class
    /// owns it. Returns `None` when the method is unknown or is shared by more
    /// than one class (callers must consult `ClassMethodIndex.collisions` for
    /// the ambiguous case).
    #[must_use]
    pub fn class_name_for_method(&self, method: &str) -> Option<&str> {
        let mut found: Option<&str> = None;
        for info in self.classes.values() {
            if info.method_sigs.iter().any(|m| m.name == method) {
                if found.is_some() {
                    // Two classes declare this method — ambiguous.
                    return None;
                }
                found = Some(&info.name);
            }
        }
        found
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
    /// Constraints required by the instance's method bodies.
    ///
    /// For a parametric instance `Encode (List a) where Encode a`, this holds
    /// one entry for the `Encode a` context requirement. For non-parametric
    /// instances this is always empty.
    ///
    /// The [`TyVid`]s stored here are **sentinel zeros** — they are never
    /// valid inference variables. The solver uses [`InstanceInfo::head_var_positions`]
    /// to substitute the correct concrete type from the head's type arguments
    /// before enqueuing a `ctx_constraint`.
    pub ctx_constraints: Vec<Constraint>,
    /// Per-`ctx_constraint` head argument position.
    ///
    /// `head_var_positions[i]` is the zero-based index into the head type's
    /// argument list that carries the type variable for `ctx_constraints[i]`.
    ///
    /// For `instance Encode (List a) where Encode a`, the head is `List a`
    /// (one arg at position 0), so `head_var_positions = [0]`.
    /// For `instance Foo (Result a e) where Bar a, Baz e`, this would be
    /// `[0, 1]`.
    ///
    /// Always the same length as `ctx_constraints`. Empty for non-parametric
    /// instances.
    pub head_var_positions: Vec<usize>,
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
/// The single-value-per-key `(ClassId, InstanceHead) → InstanceInfo` map IS the
/// Haskell-98 coherence constraint: at most one instance per `(class, head)`
/// tuple, where the head is the tuple of type constructors the instance applies
/// to (one for an ordinary class, several for a multi-parameter class). A second
/// insert for the same key returns a [`CoherenceError`]; distinct head tuples
/// are distinct keys, so `Convert Celsius Fahrenheit` and `Convert Celsius Kelvin`
/// coexist.
#[derive(Debug, Default)]
pub struct InstanceEnv {
    /// The canonical instance map.
    pub instances: FxHashMap<(ClassId, InstanceHead), InstanceInfo>,
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
        self.insert_multi(key.0, smallvec![key.1], info, class_name, type_name)
    }

    /// Inserts an instance keyed by a multi-constructor head, or returns a
    /// [`CoherenceError`] on conflict. The single-parameter [`Self::insert`]
    /// delegates here with a length-one head.
    pub fn insert_multi(
        &mut self,
        class: ClassId,
        head: InstanceHead,
        info: InstanceInfo,
        class_name: &str,
        type_name: &str,
    ) -> Result<(), CoherenceError> {
        let key = (class, head);
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

    /// Looks up an instance by `(ClassId, TyConId)` — the single-parameter case.
    #[must_use]
    pub fn get(&self, key: (ClassId, TyConId)) -> Option<&InstanceInfo> {
        self.get_multi(key.0, &[key.1])
    }

    /// Looks up an instance by a class id and a head tuple of type constructors.
    #[must_use]
    pub fn get_multi(&self, class: ClassId, head: &[TyConId]) -> Option<&InstanceInfo> {
        let key = (class, InstanceHead::from_slice(head));
        self.instances.get(&key)
    }
}

// ── Prelude class registration ────────────────────────────────────────────────

/// Registers the built-in prelude classes (`ToText`, `Eq`, `Ord`, `Encode`,
/// `Decode`) into `class_table` at their reserved [`ClassId`]s (0–4).
///
/// Must be called once before the workspace collect pass so that user-declared
/// class and instance items can reference these classes by name.
///
/// `Encode`/`Decode` mirror the Ridge-syntax declarations in
/// `crates/ridge-stdlib/stdlib/codec.ridge`, which is the canonical source for
/// humans; a consistency test keeps the two in sync.
pub fn register_prelude_classes(ct: &mut ClassTable) {
    // ToText (id=0) — no superclasses; one method: toText
    ct.insert_with_id(
        TOTEXT_CLASS,
        ClassInfo {
            name: "ToText".to_string(),
            arity: 1,
            method_sigs: vec![MethodSig {
                name: "toText".to_string(),
                arity: 1,
                ast_param_types: vec![],
                ast_ret_type: None,
                class_ty_vars: Vec::new(),
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
            arity: 1,
            method_sigs: vec![MethodSig {
                name: "eq".to_string(),
                arity: 2,
                ast_param_types: vec![],
                ast_ret_type: None,
                class_ty_vars: Vec::new(),
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
            arity: 1,
            method_sigs: vec![MethodSig {
                name: "compare".to_string(),
                arity: 2,
                ast_param_types: vec![],
                ast_ret_type: None,
                class_ty_vars: Vec::new(),
            }],
            superclasses: vec![EQ_CLASS],
            def_module: None,
        },
    );

    // Encode (id=3) — no superclasses; one method: encode (a -> JsonValue).
    ct.insert_with_id(
        ENCODE_CLASS,
        ClassInfo {
            name: "Encode".to_string(),
            arity: 1,
            method_sigs: vec![MethodSig {
                name: "encode".to_string(),
                arity: 1,
                ast_param_types: vec![],
                ast_ret_type: None,
                class_ty_vars: Vec::new(),
            }],
            superclasses: vec![],
            def_module: None,
        },
    );

    // Decode (id=4) — no superclasses; one method: decode (JsonValue -> Result a Error).
    ct.insert_with_id(
        DECODE_CLASS,
        ClassInfo {
            name: "Decode".to_string(),
            arity: 1,
            method_sigs: vec![MethodSig {
                name: "decode".to_string(),
                arity: 1,
                ast_param_types: vec![],
                ast_ret_type: None,
                class_ty_vars: Vec::new(),
            }],
            superclasses: vec![],
            def_module: None,
        },
    );
}

// ── Prelude instance registration ────────────────────────────────────────────

/// Registers the built-in prelude instances for `ToText`, `Eq`, `Ord`,
/// `Encode`, and `Decode` into `instance_env`.
///
/// These instances cover the primitive prelude types (`Int`, `Bool`, `Text`,
/// `Timestamp`, `Ordering`, plus `Float` for `Encode`/`Decode`) and are
/// equivalent to the instances the user would declare explicitly, but live in
/// the prelude module (`def_module = None`).
///
/// Notable omissions (intentional):
/// - **`Eq Float`** — floating-point equality is a footgun; the instance is
///   intentionally absent so that `deriving Eq` on a `Float`-bearing type
///   fails with a T029 that includes the footgun warning.
/// - **`Ord Float`**, **`Ord Bool`** — not defined in the 0.2.13 prelude.
///
/// `TyConId` values are the fixed builtin indices assigned by
/// [`ridge_types::BuiltinTyCons::allocate`]:
/// `Int=0, Float=1, Bool=2, Text=3, Unit=4, Timestamp=5, …, Ordering=15`.
#[expect(
    clippy::too_many_lines,
    reason = "flat sequential env.insert() calls, one per prelude instance; splitting per class would hurt readability without reducing complexity"
)]
pub fn register_prelude_instances(env: &mut InstanceEnv) {
    let ds = Span::point(0);

    // Helper to build a minimal prelude instance entry.
    let prelude_inst = |method: &str| InstanceInfo {
        def_module: None,
        methods: vec![(method.to_string(), String::new())],
        ctx_constraints: vec![],
        head_var_positions: vec![],
        origin: InstanceOrigin::Explicit,
        span: ds,
    };

    // Helper to build a PARAMETRIC prelude instance entry such as
    // `instance Encode (List a) where Encode a`. The context constraints carry
    // the element class with a sentinel `TyVid(0)`; the solver substitutes the
    // concrete element type using `head_var_positions` (see `discharge_concrete`).
    let parametric_inst = |method: &str, ctx_class: ClassId, positions: Vec<usize>| {
        let ctx_constraints = positions
            .iter()
            .map(|_| Constraint::single(ctx_class, TyVid(0)))
            .collect::<Vec<_>>();
        InstanceInfo {
            def_module: None,
            methods: vec![(method.to_string(), String::new())],
            ctx_constraints,
            head_var_positions: positions,
            origin: InstanceOrigin::Explicit,
            span: ds,
        }
    };

    // ── ToText instances ──────────────────────────────────────────────────────
    // Int (TyConId 0)
    let _ = env.insert(
        (TOTEXT_CLASS, TyConId(0)),
        prelude_inst("toText"),
        "ToText",
        "Int",
    );
    // Float (TyConId 1)
    let _ = env.insert(
        (TOTEXT_CLASS, TyConId(1)),
        prelude_inst("toText"),
        "ToText",
        "Float",
    );
    // Bool (TyConId 2)
    let _ = env.insert(
        (TOTEXT_CLASS, TyConId(2)),
        prelude_inst("toText"),
        "ToText",
        "Bool",
    );
    // Text (TyConId 3)
    let _ = env.insert(
        (TOTEXT_CLASS, TyConId(3)),
        prelude_inst("toText"),
        "ToText",
        "Text",
    );
    // Timestamp (TyConId 5)
    let _ = env.insert(
        (TOTEXT_CLASS, TyConId(5)),
        prelude_inst("toText"),
        "ToText",
        "Timestamp",
    );
    // Ordering (TyConId 15)
    let _ = env.insert(
        (TOTEXT_CLASS, TyConId(15)),
        prelude_inst("toText"),
        "ToText",
        "Ordering",
    );

    // ── Eq instances ─────────────────────────────────────────────────────────
    // Eq Float is intentionally absent — floating-point equality is a footgun.
    // Int (TyConId 0)
    let _ = env.insert((EQ_CLASS, TyConId(0)), prelude_inst("eq"), "Eq", "Int");
    // Bool (TyConId 2)
    let _ = env.insert((EQ_CLASS, TyConId(2)), prelude_inst("eq"), "Eq", "Bool");
    // Text (TyConId 3)
    let _ = env.insert((EQ_CLASS, TyConId(3)), prelude_inst("eq"), "Eq", "Text");
    // Timestamp (TyConId 5)
    let _ = env.insert(
        (EQ_CLASS, TyConId(5)),
        prelude_inst("eq"),
        "Eq",
        "Timestamp",
    );
    // Eq Ordering — required by the Ord Ordering superclass check
    let _ = env.insert(
        (EQ_CLASS, TyConId(15)),
        prelude_inst("eq"),
        "Eq",
        "Ordering",
    );

    // ── Ord instances ─────────────────────────────────────────────────────────
    // Int (TyConId 0)
    let _ = env.insert(
        (ORD_CLASS, TyConId(0)),
        prelude_inst("compare"),
        "Ord",
        "Int",
    );
    // Text (TyConId 3) — lexicographic ordering
    let _ = env.insert(
        (ORD_CLASS, TyConId(3)),
        prelude_inst("compare"),
        "Ord",
        "Text",
    );
    // Ord Ordering — natural ordering: Less < Equal < Greater
    let _ = env.insert(
        (ORD_CLASS, TyConId(15)),
        prelude_inst("compare"),
        "Ord",
        "Ordering",
    );

    // ── Encode instances ──────────────────────────────────────────────────────
    // Each primitive maps to the matching JsonValue variant (JInt/JFloat/JBool/
    // JText). Unlike Eq, Encode Float is fine — JSON numbers carry floats.
    // The method bodies are filled in by the deriving pass; here we only record
    // that the instance exists so derived Encode can discharge field constraints.
    let _ = env.insert(
        (ENCODE_CLASS, TyConId(0)),
        prelude_inst("encode"),
        "Encode",
        "Int",
    );
    let _ = env.insert(
        (ENCODE_CLASS, TyConId(1)),
        prelude_inst("encode"),
        "Encode",
        "Float",
    );
    let _ = env.insert(
        (ENCODE_CLASS, TyConId(2)),
        prelude_inst("encode"),
        "Encode",
        "Bool",
    );
    let _ = env.insert(
        (ENCODE_CLASS, TyConId(3)),
        prelude_inst("encode"),
        "Encode",
        "Text",
    );

    // ── Decode instances ──────────────────────────────────────────────────────
    let _ = env.insert(
        (DECODE_CLASS, TyConId(0)),
        prelude_inst("decode"),
        "Decode",
        "Int",
    );
    let _ = env.insert(
        (DECODE_CLASS, TyConId(1)),
        prelude_inst("decode"),
        "Decode",
        "Float",
    );
    let _ = env.insert(
        (DECODE_CLASS, TyConId(2)),
        prelude_inst("decode"),
        "Decode",
        "Bool",
    );
    let _ = env.insert(
        (DECODE_CLASS, TyConId(3)),
        prelude_inst("decode"),
        "Decode",
        "Text",
    );

    // ── Parametric container instances (Encode/Decode) ────────────────────────
    // `instance Encode (List a) where Encode a`, and the Option/Map/Result duals,
    // for both Encode and Decode. The head TyConIds are the fixed builtin slots:
    // List=6, Map=7, Option=9, Result=10. The constrained element variable sits
    // at head position 0 for List/Option, position 1 for `Map Text a` (the Text
    // key is at 0), and positions 0 and 1 for `Result a e`.
    //
    // These instances have NO source body. Their dictionaries are synthesised in
    // the lowering pass (see `ridge_lower::prelude_dict`); registering them here
    // is what lets the constraint solver discharge `Encode (List Int)` etc. and
    // build the dict-of-dicts plan. `def_module = None` bypasses the orphan rule.
    let _ = env.insert(
        (ENCODE_CLASS, TyConId(6)),
        parametric_inst("encode", ENCODE_CLASS, vec![0]),
        "Encode",
        "List",
    );
    let _ = env.insert(
        (ENCODE_CLASS, TyConId(9)),
        parametric_inst("encode", ENCODE_CLASS, vec![0]),
        "Encode",
        "Option",
    );
    let _ = env.insert(
        (ENCODE_CLASS, TyConId(7)),
        parametric_inst("encode", ENCODE_CLASS, vec![1]),
        "Encode",
        "Map",
    );
    let _ = env.insert(
        (ENCODE_CLASS, TyConId(10)),
        parametric_inst("encode", ENCODE_CLASS, vec![0, 1]),
        "Encode",
        "Result",
    );

    let _ = env.insert(
        (DECODE_CLASS, TyConId(6)),
        parametric_inst("decode", DECODE_CLASS, vec![0]),
        "Decode",
        "List",
    );
    let _ = env.insert(
        (DECODE_CLASS, TyConId(9)),
        parametric_inst("decode", DECODE_CLASS, vec![0]),
        "Decode",
        "Option",
    );
    let _ = env.insert(
        (DECODE_CLASS, TyConId(7)),
        parametric_inst("decode", DECODE_CLASS, vec![1]),
        "Decode",
        "Map",
    );
    let _ = env.insert(
        (DECODE_CLASS, TyConId(10)),
        parametric_inst("decode", DECODE_CLASS, vec![0, 1]),
        "Decode",
        "Result",
    );
}

// ── Stdlib class registration ────────────────────────────────────────────────

/// Registers stdlib-defined typeclasses that user code consumes by import.
///
/// Currently `SqlType` from std.sql, registered with the same shape as the
/// `pub class SqlType` in sql.ridge so the constraint solver and instance
/// coherence see it. Its method schemes are seeded directly (see
/// `seed_sql_codec_schemes` in lib.rs), so the method sigs carry no AST types
/// and the AST-driven `seed_class_method_schemes` skips them.
///
/// Must be called once, after `register_prelude_classes`, before the workspace
/// collect pass, so the dynamically-assigned `ClassId` precedes user classes.
#[expect(
    clippy::too_many_lines,
    reason = "one literal MethodSig per stdlib class method; the list reads best kept together"
)]
pub fn register_stdlib_classes(ct: &mut ClassTable) {
    let id = ct.intern("SqlType");
    ct.insert_with_id(
        id,
        ClassInfo {
            name: "SqlType".to_string(),
            arity: 1,
            method_sigs: vec![
                MethodSig {
                    name: "toSql".to_string(),
                    arity: 1,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "fromSql".to_string(),
                    arity: 1,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
            ],
            superclasses: vec![],
            def_module: None,
        },
    );

    // `Row` from std.sql — the database-row decoder class. Its single method
    // `fromRow` is seeded directly (see `seed_sql_codec_schemes` in lib.rs), so
    // the method sig carries no AST types, exactly like `SqlType` above. The
    // instances come from `deriving (Row)` on user records, not from the prelude.
    let row_id = ct.intern("Row");
    ct.insert_with_id(
        row_id,
        ClassInfo {
            name: "Row".to_string(),
            arity: 1,
            method_sigs: vec![MethodSig {
                name: "fromRow".to_string(),
                arity: 1,
                ast_param_types: vec![],
                ast_ret_type: None,
                class_ty_vars: Vec::new(),
            }],
            superclasses: vec![],
            def_module: None,
        },
    );

    // `Adapter` from std.data — the storage seam. Both methods (`insert`/`all`)
    // are seeded directly (see `seed_sql_codec_schemes` in lib.rs), so the sigs
    // carry no AST types, like `SqlType`/`Row` above. Its instances are the
    // in-memory adapter (registered in `register_stdlib_instances`) and, later,
    // backend adapters such as Postgres.
    let adapter_id = ct.intern("Adapter");
    ct.insert_with_id(
        adapter_id,
        ClassInfo {
            name: "Adapter".to_string(),
            arity: 1,
            method_sigs: vec![
                MethodSig {
                    name: "insert".to_string(),
                    arity: 3,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "all".to_string(),
                    arity: 2,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "select".to_string(),
                    arity: 3,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "get".to_string(),
                    arity: 4,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "delete".to_string(),
                    arity: 3,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "fetch".to_string(),
                    arity: 6,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "countWhere".to_string(),
                    arity: 3,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "project".to_string(),
                    arity: 7,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "join".to_string(),
                    arity: 8,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
                MethodSig {
                    name: "joinSelect".to_string(),
                    arity: 9,
                    ast_param_types: vec![],
                    ast_ret_type: None,
                    class_ty_vars: Vec::new(),
                },
            ],
            superclasses: vec![],
            def_module: None,
        },
    );
}

/// Registers the base-type instances of stdlib-defined classes into `env`.
///
/// Currently covers `SqlType` for Int/Text/Bool/Float, keyed by the class id
/// that `register_stdlib_classes` assigned. Like prelude instances they live
/// outside any user module (`def_module = None`) so the orphan rule does not
/// apply.
///
/// Must run after `collect_instance_decls` so that if `sql.ridge` is being
/// compiled as part of the stdlib build (tier 5), its source-level instance
/// declarations are already in the env and this function is a no-op for those
/// keys. For user workspaces (which never declare `instance SqlType T`), all
/// four entries are inserted here and the constraint solver can discharge them.
/// `reconciled_tycon_names` carries the reserved-block stdlib type ids so the
/// in-memory `Adapter MemAdapter` instance can be keyed by `MemAdapter`'s id.
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashMap is the workspace-wide hasher; this mirrors collect_workspace's signature"
)]
pub fn register_stdlib_instances(
    env: &mut InstanceEnv,
    ct: &ClassTable,
    reconciled_tycon_names: &rustc_hash::FxHashMap<String, TyConId>,
) {
    let ds = Span::point(0);
    if let Some(sqltype) = ct.id_by_name("SqlType") {
        let inst = || InstanceInfo {
            def_module: None,
            methods: vec![
                ("toSql".to_string(), String::new()),
                ("fromSql".to_string(), String::new()),
            ],
            ctx_constraints: vec![],
            head_var_positions: vec![],
            origin: InstanceOrigin::Explicit,
            span: ds,
        };
        // Builtin TyConIds: Int=0, Float=1, Bool=2, Text=3.
        // Use entry/or_insert rather than the coherence-checking insert so that
        // source-level declarations (from sql.ridge in the stdlib build) always
        // win — they got here first, and we never want to overwrite them or
        // surface a spurious T032.
        env.instances
            .entry((sqltype, smallvec![TyConId(0)]))
            .or_insert_with(inst);
        env.instances
            .entry((sqltype, smallvec![TyConId(3)]))
            .or_insert_with(inst);
        env.instances
            .entry((sqltype, smallvec![TyConId(2)]))
            .or_insert_with(inst);
        env.instances
            .entry((sqltype, smallvec![TyConId(1)]))
            .or_insert_with(inst);
    }

    // `Adapter MemAdapter` — the in-memory adapter instance from std.data, keyed
    // by the reconciled `MemAdapter` id. During the stdlib's own build the
    // reconciled block is skipped (the map has no `MemAdapter`), so data.ridge's
    // source instance is collected instead and this is a no-op; for user
    // workspaces the entry is inserted here so the solver discharges the
    // `Adapter MemAdapter` constraint.
    let adapter_methods = || {
        vec![
            ("insert".to_string(), String::new()),
            ("all".to_string(), String::new()),
            ("select".to_string(), String::new()),
            ("get".to_string(), String::new()),
            ("delete".to_string(), String::new()),
            ("fetch".to_string(), String::new()),
            ("countWhere".to_string(), String::new()),
            ("project".to_string(), String::new()),
            ("join".to_string(), String::new()),
            ("joinSelect".to_string(), String::new()),
        ]
    };
    if let (Some(adapter), Some(&mem_adapter)) = (
        ct.id_by_name("Adapter"),
        reconciled_tycon_names.get("MemAdapter"),
    ) {
        env.instances
            .entry((adapter, smallvec![mem_adapter]))
            .or_insert_with(|| InstanceInfo {
                def_module: None,
                methods: adapter_methods(),
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: ds,
            });
    }

    // `Adapter Postgres` — the Postgres adapter instance from std.data, keyed by
    // the reconciled `Postgres` id, on the same terms as `Adapter MemAdapter`
    // above: inserted for user workspaces, a no-op during the stdlib's own build
    // where data.ridge's source instance is collected directly.
    if let (Some(adapter), Some(&postgres)) = (
        ct.id_by_name("Adapter"),
        reconciled_tycon_names.get("Postgres"),
    ) {
        env.instances
            .entry((adapter, smallvec![postgres]))
            .or_insert_with(|| InstanceInfo {
                def_module: None,
                methods: adapter_methods(),
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: ds,
            });
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_types::{DECODE_CLASS, ENCODE_CLASS, EQ_CLASS, ORD_CLASS, TOTEXT_CLASS};

    fn dummy_span() -> Span {
        Span::point(0)
    }

    fn make_instance(origin: InstanceOrigin) -> InstanceInfo {
        InstanceInfo {
            def_module: None,
            methods: vec![],
            ctx_constraints: vec![],
            head_var_positions: vec![],
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
        assert_eq!(ct.id_by_name("Encode"), Some(ENCODE_CLASS));
        assert_eq!(ct.id_by_name("Decode"), Some(DECODE_CLASS));

        let ord_info = ct.get(ORD_CLASS).expect("Ord must be in ClassTable");
        assert_eq!(ord_info.superclasses, vec![EQ_CLASS]);

        // Encode/Decode each have a single arity-1 method and no superclass.
        let encode_info = ct.get(ENCODE_CLASS).expect("Encode must be in ClassTable");
        assert_eq!(encode_info.method_sigs.len(), 1);
        assert_eq!(encode_info.method_sigs[0].name, "encode");
        assert_eq!(encode_info.method_sigs[0].arity, 1);
        assert!(encode_info.superclasses.is_empty());

        let decode_info = ct.get(DECODE_CLASS).expect("Decode must be in ClassTable");
        assert_eq!(decode_info.method_sigs.len(), 1);
        assert_eq!(decode_info.method_sigs[0].name, "decode");
        assert_eq!(decode_info.method_sigs[0].arity, 1);
        assert!(decode_info.superclasses.is_empty());
    }

    #[test]
    fn prelude_encode_decode_instances_registered() {
        let mut env = InstanceEnv::new();
        register_prelude_instances(&mut env);
        // Encode/Decode cover the four JSON primitives Int/Float/Bool/Text.
        for tycon in [TyConId(0), TyConId(1), TyConId(2), TyConId(3)] {
            assert!(
                env.get((ENCODE_CLASS, tycon)).is_some(),
                "Encode instance missing for {tycon:?}"
            );
            assert!(
                env.get((DECODE_CLASS, tycon)).is_some(),
                "Decode instance missing for {tycon:?}"
            );
        }
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
