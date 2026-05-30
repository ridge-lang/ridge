//! Maranget's pattern exhaustiveness and redundancy algorithm (T12).
//!
//! Entry point: [`check_exhaustiveness`] — called from `infer_expr` after the
//! per-arm bodies are type-checked (§4.12).
//!
//! # Algorithm
//!
//! Implements Maranget (2007) "Warnings for pattern matching":
//! - `useful(P, q)` — is `q` useful relative to matrix `P`?
//! - Exhaustiveness: run `useful(arm-matrix, wildcard)` → `T016`.
//! - Redundancy:   for each arm `i`, run `useful(P[0..i], arm[i])` → `T017`.
//!
//! # Witness cap
//!
//! Missing witnesses are capped at `MAX_WITNESSES = 3`.  `T016` carries
//! `total_missing` (the true count) alongside the capped `witnesses` vec.
//! When `total_missing > witnesses.len()`, the renderer appends
//! `... and N more` where `N = total_missing - witnesses.len()`.

use ridge_ast::{ListPatElem, Literal, Pattern, Span};
use ridge_types::{
    BuiltinTyCons, MatchWitness, TyConArena, TyConId, TyConKind, Type, UnionVariant,
    VariantPayload, WitnessKind, WitnessPat,
};

use crate::ctx::InferCtx;
use crate::error::TypeError;

/// Maximum number of witnesses stored in a `T016` diagnostic.
const MAX_WITNESSES: usize = 3;

// ── Constructor ───────────────────────────────────────────────────────────────

/// A normalised constructor used in the pattern matrix.
///
/// This is the structural identity of a pattern head for specialisation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Constructor {
    /// A named union variant (e.g. `Some`, `Circle`).
    Variant {
        /// The union's `TyConId`.
        union_id: TyConId,
        /// Zero-based index into the `UnionSchema.variants` list.
        variant_idx: usize,
        /// Number of positional payload slots (arity).
        arity: usize,
        /// Name for witness rendering.
        name: String,
    },
    /// A record-typed constructor (single ctor of arity = field count).
    Record {
        /// The record's `TyConId`.
        record_id: TyConId,
        /// Number of fields.
        arity: usize,
        /// Type name for witness rendering.
        name: String,
    },
    /// A tuple constructor of a fixed arity.
    Tuple {
        /// Number of tuple elements.
        arity: usize,
    },
    /// A literal value treated as a 0-arity constructor (Bool, Int, …).
    Literal(LitKey),
    /// The empty list `[]` — a 0-arity constructor distinct from `::`.
    ListNil,
    /// The non-empty list `head :: tail` — a 2-arity constructor.
    ListCons,
}

impl Constructor {
    /// Arity of this constructor (number of sub-pattern slots after expansion).
    const fn arity(&self) -> usize {
        match self {
            Self::Variant { arity, .. } | Self::Record { arity, .. } | Self::Tuple { arity } => {
                *arity
            }
            Self::Literal(_) | Self::ListNil => 0,
            Self::ListCons => 2,
        }
    }
}

// ── LitKey ────────────────────────────────────────────────────────────────────

/// A stable key representing a literal value in the pattern matrix.
///
/// Used to distinguish `true`/`false` (Bool closed domain) and to identify
/// integer/text literal patterns (open domains where we use the default matrix).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum LitKey {
    BoolTrue,
    BoolFalse,
    /// Any other literal (Int, Float, Text) — used as a key but the domain
    /// is non-closed, so these never appear in the closed ctor set.
    Other(String),
}

// ── NormPat ───────────────────────────────────────────────────────────────────

/// A normalised pattern shape derived from a `ridge_ast::Pattern`.
///
/// `Wildcard` unifies `Pattern::Wildcard`, `Pattern::Var`, and `Pattern::As`
/// (binding forms that do not constrain the constructor).
#[derive(Debug, Clone)]
enum NormPat {
    /// Matches anything — wildcard, variable binding, or alias.
    Wildcard,
    /// A specific constructor with its normalised sub-patterns.
    Ctor(Constructor, Vec<Self>),
    /// A literal value treated as a 0-arity constructor.
    Literal(LitKey),
}

// ── PatternMatrix ─────────────────────────────────────────────────────────────

/// A matrix of normalised patterns, one row per arm.
///
/// Each row has the same column count.  For a 1-scrutinee `match`, every row
/// starts as a single-column vector; specialisation may expand it.
#[derive(Debug, Clone, Default)]
struct PatternMatrix {
    rows: Vec<Vec<NormPat>>,
}

impl PatternMatrix {
    const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn push(&mut self, row: Vec<NormPat>) {
        self.rows.push(row);
    }
}

// ── Lifting ridge_ast::Pattern → NormPat ─────────────────────────────────────

/// Lift a `ridge_ast::Pattern` into a `NormPat`.
///
/// Binding forms (`Var`, `As`) lift to `Wildcard`.
/// `Paren` is transparent.
/// `Constructor` lifts to `Ctor(...)` with its sub-patterns.
/// `Tuple` lifts to a `Ctor(Tuple { arity }, sub-pats)`.
/// `Literal` lifts to `Literal(LitKey)`.
/// `Cons` (list pattern) lifts to `Wildcard` — list exhaustiveness is out of
/// scope for 0.1.0 (no closed domain; treated as non-closed).
fn lift_pattern(pat: &Pattern) -> NormPat {
    match pat {
        // Wildcard, variable bindings, and inline record patterns lift to Wildcard.
        // TODO(0.2.12): Pattern::Record exhaustiveness is wired in T5.
        Pattern::Wildcard { .. } | Pattern::Var { .. } | Pattern::Record { .. } => {
            NormPat::Wildcard
        }

        // Empty-list pattern `[]` — 0-arity ListNil constructor.
        Pattern::ListNil { .. } => NormPat::Ctor(Constructor::ListNil, vec![]),

        // Cons pattern `head :: tail` — 2-arity ListCons constructor.
        Pattern::Cons { head, tail, .. } => NormPat::Ctor(
            Constructor::ListCons,
            vec![lift_pattern(head), lift_pattern(tail)],
        ),

        Pattern::As { inner, .. } | Pattern::Paren { inner, .. } => lift_pattern(inner),

        Pattern::Literal { lit, .. } => NormPat::Literal(lit_to_key(lit)),

        Pattern::Tuple { elems, .. } => {
            let sub: Vec<NormPat> = elems.iter().map(lift_pattern).collect();
            let arity = sub.len();
            NormPat::Ctor(Constructor::Tuple { arity }, sub)
        }

        Pattern::Constructor {
            name, args, fields, ..
        } => {
            if fields.is_some() {
                // Record-body constructor pattern — conservatively treat as
                // Wildcard.  Record types have a single ctor, so a record
                // pattern with any field bindings (including `..`) always
                // matches the whole constructor.  Lifting to Wildcard avoids
                // false T016 alerts for record types while still allowing
                // wildcard-based redundancy detection to work.  The `has_rest`
                // flag (D259) does not change this behaviour: an explicit
                // record pattern with `..` is still irrefutable over its type.
                NormPat::Wildcard
            } else {
                // Positional constructor — arity known from args.
                let sub: Vec<NormPat> = args.iter().map(lift_pattern).collect();
                let arity = sub.len();
                NormPat::Ctor(
                    Constructor::Variant {
                        union_id: TyConId(u32::MAX), // placeholder; resolved via ctor_set
                        variant_idx: usize::MAX,     // placeholder
                        arity,
                        name: name.text.clone(),
                    },
                    sub,
                )
            }
        }

        // Bracketed list pattern — desugar to Cons/ListNil/Wildcard and recurse.
        Pattern::List { elements, span } => {
            lift_pattern(&desugar_list_pattern_for_matrix(elements, *span))
        }
    }
}

/// Desugar a bracketed list pattern to the cons/nil form used by the
/// exhaustiveness matrix.
///
/// Suffix and middle-rest elements are required to be irrefutable (enforced at
/// lowering by `L009`), so a pattern like `[a, .., b]` constrains only its first
/// element and a minimum length — it is equivalent to `a :: _ :: _`. We replace
/// each suffix element with a wildcard in prefix position and terminate with a
/// wildcard tail (for `..`) or `[]` (for a fixed-length list). This keeps every
/// length precisely covered by the cons/nil recursion, so the slice surface adds
/// no special cases to the core algorithm and stays sound.
fn desugar_list_pattern_for_matrix(elements: &[ListPatElem], span: Span) -> Pattern {
    let rest_pos = elements
        .iter()
        .position(|e| matches!(e, ListPatElem::Rest { .. }));

    let mut heads: Vec<Pattern> = Vec::new();
    for (i, elem) in elements.iter().enumerate() {
        if let ListPatElem::Elem(p) = elem {
            // Prefix elements keep their pattern; suffix elements (after the
            // rest) are irrefutable, so only their count matters → wildcard.
            let is_suffix = rest_pos.is_some_and(|r| i > r);
            if is_suffix {
                heads.push(Pattern::Wildcard { span: p.span() });
            } else {
                heads.push(p.clone());
            }
        }
    }

    let tail = if rest_pos.is_some() {
        Pattern::Wildcard { span }
    } else {
        Pattern::ListNil { span }
    };

    heads
        .into_iter()
        .rev()
        .fold(tail, |acc, head| Pattern::Cons {
            head: Box::new(head),
            tail: Box::new(acc),
            span,
        })
}

fn lit_to_key(lit: &Literal) -> LitKey {
    match lit {
        Literal::Bool { value: true, .. } => LitKey::BoolTrue,
        Literal::Bool { value: false, .. } => LitKey::BoolFalse,
        Literal::IntDec { raw, .. }
        | Literal::IntBin { raw, .. }
        | Literal::IntOct { raw, .. }
        | Literal::IntHex { raw, .. }
        | Literal::Float { raw, .. }
        | Literal::Text { raw, .. }
        | Literal::RawText { raw, .. } => LitKey::Other(raw.clone()),
    }
}

// ── Constructor set ───────────────────────────────────────────────────────────

/// Returns the complete set of constructors for a type, if the domain is
/// closed (finite).
///
/// Returns `None` for open domains (Int, Float, Text, Timestamp, List, Map,
/// Set, Handle, and any unresolved type variable).
///
/// The returned vec is in declaration order so witness rendering is deterministic.
fn ctor_set_for(ty: &Type, b: &BuiltinTyCons, arena: &TyConArena) -> Option<Vec<Constructor>> {
    match ty {
        Type::Con(id, _) => {
            // Bool is a primitive but has a closed 2-element domain.
            if *id == b.bool {
                return Some(vec![
                    Constructor::Literal(LitKey::BoolTrue),
                    Constructor::Literal(LitKey::BoolFalse),
                ]);
            }

            // List is a structurally closed type: every value is either
            // `[]` (ListNil) or `head :: tail` (ListCons).
            if *id == b.list {
                return Some(vec![Constructor::ListNil, Constructor::ListCons]);
            }

            // Int, Float, Text, Unit, Timestamp: not pattern-matchable as closed
            // (Unit has a single value but no unit literal pattern in Ridge 0.1.0).
            // Map, Set, Handle: open/infinite.
            // Option and Result: union types — handled below via TyConKind::Union.

            // Guard against sentinel ids and out-of-range ids.
            if id.0 as usize >= arena.len() {
                return None;
            }

            let decl = arena.get(*id);
            match &decl.kind {
                TyConKind::Union(schema) => {
                    let ctors: Vec<Constructor> = schema
                        .variants
                        .iter()
                        .enumerate()
                        .map(|(idx, v)| Constructor::Variant {
                            union_id: *id,
                            variant_idx: idx,
                            arity: variant_arity(v),
                            name: v.name.clone(),
                        })
                        .collect();
                    Some(ctors)
                }
                TyConKind::Record(schema) => {
                    // A record has exactly one constructor.
                    let arity = schema.record_fields().len();
                    Some(vec![Constructor::Record {
                        record_id: *id,
                        arity,
                        name: decl.name.clone(),
                    }])
                }
                // Primitives, Builtin (List/Map/Set/Handle), Actor, Alias: open.
                _ => None,
            }
        }

        // Tuple types have a single tuple-constructor.
        Type::Tuple(elems) => {
            let arity = elems.len();
            Some(vec![Constructor::Tuple { arity }])
        }

        // Unresolved type variable, function type, alias, error: non-closed.
        _ => None,
    }
}

/// Arity (number of positional sub-patterns) of a union variant.
fn variant_arity(v: &UnionVariant) -> usize {
    match &v.kind {
        VariantPayload::Nullary => 0,
        VariantPayload::Positional(tys) => tys.len(),
        VariantPayload::Record(schema) => schema.record_fields().len(),
    }
}

// ── Collect head constructors present in a column ────────────────────────────

/// Collects the set of explicit constructors that appear at the head of any row
/// in the first column of `p`, normalised to match the canonical constructor set.
///
/// Wildcards are NOT counted here — they are handled via the default matrix.
fn collect_head_ctors(p: &PatternMatrix, canonical: &[Constructor]) -> Vec<Constructor> {
    let mut seen: Vec<Constructor> = Vec::new();
    for row in &p.rows {
        let Some(head) = row.first() else { continue };
        match head {
            NormPat::Ctor(c, _) => {
                // Match by name against canonical set to get the real ctor.
                let matched = canonical.iter().find(|canon| ctor_name_eq(c, canon));
                if let Some(canon_c) = matched {
                    if !seen.iter().any(|s| ctor_name_eq(s, canon_c)) {
                        seen.push(canon_c.clone());
                    }
                }
            }
            NormPat::Literal(k) => {
                let c = Constructor::Literal(k.clone());
                if !seen.contains(&c) {
                    seen.push(c);
                }
            }
            NormPat::Wildcard => {
                // Wildcards are handled via the default matrix, not here.
            }
        }
    }
    seen
}

/// True when two constructors represent the same syntactic head.
///
/// For `Variant` constructors, uses name equality (the lifted `NormPat` carries
/// placeholder ids; the canonical set carries real ids).
/// For `Literal` and `Tuple`, uses full equality.
fn ctor_name_eq(a: &Constructor, b: &Constructor) -> bool {
    match (a, b) {
        (Constructor::Variant { name: na, .. }, Constructor::Variant { name: nb, .. })
        | (Constructor::Record { name: na, .. }, Constructor::Record { name: nb, .. }) => na == nb,
        (Constructor::Tuple { arity: aa }, Constructor::Tuple { arity: ab }) => aa == ab,
        (Constructor::Literal(ka), Constructor::Literal(kb)) => ka == kb,
        (Constructor::ListNil, Constructor::ListNil)
        | (Constructor::ListCons, Constructor::ListCons) => true,
        _ => false,
    }
}

// ── Specialisation ────────────────────────────────────────────────────────────

/// Specialise the matrix `p` on constructor `c`:
/// - Rows whose head is a wildcard (or any binding) expand to `arity` wildcards
///   prepended to the rest.
/// - Rows whose head is `c` (by name) expand their sub-patterns prepended to
///   the rest.
/// - Rows whose head is a different constructor are dropped.
fn specialise(p: &PatternMatrix, c: &Constructor) -> PatternMatrix {
    let mut result = PatternMatrix::default();
    let arity = c.arity();
    for row in &p.rows {
        let Some(head) = row.first() else {
            // Empty row — carry through.
            result.push(row.clone());
            continue;
        };
        let tail = &row[1..];
        match head {
            NormPat::Wildcard => {
                // Expand wildcard to `arity` wildcards + tail.
                let mut new_row: Vec<NormPat> = vec![NormPat::Wildcard; arity];
                new_row.extend_from_slice(tail);
                result.push(new_row);
            }
            NormPat::Ctor(head_c, sub_pats) if ctor_name_eq(head_c, c) => {
                // Expand with sub-patterns + tail.
                let mut new_row: Vec<NormPat> = sub_pats.clone();
                new_row.extend_from_slice(tail);
                result.push(new_row);
            }
            NormPat::Literal(k) => {
                if ctor_name_eq(&Constructor::Literal(k.clone()), c) {
                    // Literal matches: arity 0, just the tail.
                    result.push(tail.to_vec());
                }
                // else: different literal, drop.
            }
            NormPat::Ctor(_, _) => {
                // Different constructor, drop.
            }
        }
    }
    result
}

/// Default matrix `D(P)`: keeps only rows whose first pattern is a wildcard,
/// dropping the first column.
fn default_matrix(p: &PatternMatrix) -> PatternMatrix {
    let mut result = PatternMatrix::default();
    for row in &p.rows {
        let Some(head) = row.first() else { continue };
        if matches!(head, NormPat::Wildcard) {
            result.push(row[1..].to_vec());
        }
    }
    result
}

// ── Usefulness ────────────────────────────────────────────────────────────────

/// Result of a usefulness check.
#[derive(Debug)]
enum Usefulness {
    /// The candidate row `q` is covered by matrix `P`.
    NotUseful,
    /// The candidate row `q` finds at least one uncovered value.
    ///
    /// The vec contains one `MatchWitness` per column of `q`.  For a
    /// single-column `match`, this is always length 1.
    UsefulWithWitness(Vec<MatchWitness>),
}

/// Is the row `q` useful relative to the pattern matrix `p`?
///
/// `column_types[i]` is the type of column `i` — used by `ctor_set_for` to
/// decide whether the domain is closed.
///
/// Follows Maranget (2007) §3.  Produces one witness per column on success.
///
/// # Base cases
///
/// - `P` is empty: ALWAYS `UsefulWithWitness(witness_from_q(q))`.
///   An empty matrix covers nothing; any `q` represents an uncovered value.
/// - `P` is non-empty AND `column_types` is empty: `NotUseful`.
///   Zero remaining columns means the row is fully covered by P.
fn useful(
    p: &PatternMatrix,
    q: &[NormPat],
    column_types: &[Type],
    b: &BuiltinTyCons,
    arena: &TyConArena,
) -> Usefulness {
    // ── Base case 1: empty matrix → q is unmatched ────────────────────────────
    if p.is_empty() {
        return Usefulness::UsefulWithWitness(witness_from_q(q));
    }

    // ── Base case 2: no columns → p has rows → q is covered ──────────────────
    if column_types.is_empty() {
        return Usefulness::NotUseful;
    }

    let col_ty = &column_types[0];
    let Some(head) = q.first() else {
        // q is empty but column_types is non-empty — should not happen in a
        // well-formed call.  Treat as covered.
        return Usefulness::NotUseful;
    };

    match head {
        NormPat::Wildcard => {
            // Case: q head is a wildcard — check whether the type is closed.
            let ctors_opt = ctor_set_for(col_ty, b, arena);
            if let Some(ctors) = ctors_opt {
                // Closed type: enumerate the constructor set.
                let head_ctors_in_p = collect_head_ctors(p, &ctors);
                if head_ctors_in_p.len() == ctors.len() {
                    // All constructors appear explicitly in P's first column.
                    // (Any wildcard rows are handled via specialise — they expand
                    // to arity-many wildcards and are included in p_spec.)
                    // Recurse into each ctor: if ANY is useful, the whole is useful.
                    for c in &ctors {
                        let p_spec = specialise(p, c);
                        // Wildcard q expands to `arity` wildcards for this ctor,
                        // then the rest of q[1..].
                        let mut q_spec: Vec<NormPat> = vec![NormPat::Wildcard; c.arity()];
                        q_spec.extend_from_slice(&q[1..]);
                        let new_col_types =
                            updated_col_types(col_ty, c, b, arena, &column_types[1..]);
                        if let Usefulness::UsefulWithWitness(witnesses) =
                            useful(&p_spec, &q_spec, &new_col_types, b, arena)
                        {
                            let (sub_count, lifted) = lift_witness_for_ctor(c, &witnesses);
                            let mut out = vec![lifted];
                            out.extend_from_slice(&witnesses[sub_count..]);
                            return Usefulness::UsefulWithWitness(out);
                        }
                    }
                    Usefulness::NotUseful
                } else {
                    // Some constructors are missing from P's explicit first column.
                    // Find the first missing ctor — it provides the witness head.
                    // The outer if-else guarantees at least one ctor is missing,
                    // so `find` cannot return None here; fall back to NotUseful to
                    // keep the panic-free invariant without weakening the deny.
                    let Some(missing_ctor) = ctors
                        .iter()
                        .find(|c| !head_ctors_in_p.iter().any(|h| ctor_name_eq(h, c)))
                    else {
                        return Usefulness::NotUseful;
                    };
                    let missing_ctor = missing_ctor.clone();
                    // Recurse via the default matrix (only wildcard rows survive).
                    // If useful → there are uncovered values (wildcard rows leave
                    // some sub-values open too). If NotUseful → wildcard rows in
                    // P cover the missing ctor — no gap.
                    let p_default = default_matrix(p);
                    let q_tail = &q[1..];
                    match useful(&p_default, q_tail, &column_types[1..], b, arena) {
                        Usefulness::UsefulWithWitness(mut witnesses) => {
                            witnesses.insert(0, witness_for_ctor(&missing_ctor));
                            Usefulness::UsefulWithWitness(witnesses)
                        }
                        Usefulness::NotUseful => {
                            // Wildcard rows cover the missing ctor.
                            Usefulness::NotUseful
                        }
                    }
                }
            } else {
                // Non-closed type (Int, Float, Text, …): use default matrix.
                // The wildcard q only represents "anything" in a non-closed domain;
                // coverage is determined by whether any wildcard row in P covers
                // the remaining columns.
                let p_default = default_matrix(p);
                let q_tail = &q[1..];
                match useful(&p_default, q_tail, &column_types[1..], b, arena) {
                    Usefulness::UsefulWithWitness(mut witnesses) => {
                        witnesses.insert(
                            0,
                            MatchWitness {
                                example: WitnessPat::Wild,
                                kind: WitnessKind::Missing,
                            },
                        );
                        Usefulness::UsefulWithWitness(witnesses)
                    }
                    Usefulness::NotUseful => Usefulness::NotUseful,
                }
            }
        }

        NormPat::Ctor(c, sub_pats) => {
            // Case: q head is a specific constructor.
            let p_spec = specialise(p, c);
            let mut q_spec: Vec<NormPat> = sub_pats.clone();
            q_spec.extend_from_slice(&q[1..]);
            let new_col_types = updated_col_types(col_ty, c, b, arena, &column_types[1..]);
            match useful(&p_spec, &q_spec, &new_col_types, b, arena) {
                Usefulness::UsefulWithWitness(witnesses) => {
                    let (sub_count, lifted) = lift_witness_for_ctor(c, &witnesses);
                    let mut out = vec![lifted];
                    out.extend_from_slice(&witnesses[sub_count..]);
                    Usefulness::UsefulWithWitness(out)
                }
                Usefulness::NotUseful => Usefulness::NotUseful,
            }
        }

        NormPat::Literal(lit_key) => {
            // Case: q head is a literal — treat as 0-arity constructor.
            let c = Constructor::Literal(lit_key.clone());
            let p_spec = specialise(p, &c);
            let q_tail = &q[1..];
            match useful(&p_spec, q_tail, &column_types[1..], b, arena) {
                Usefulness::UsefulWithWitness(mut witnesses) => {
                    witnesses.insert(0, witness_for_lit(lit_key));
                    Usefulness::UsefulWithWitness(witnesses)
                }
                Usefulness::NotUseful => Usefulness::NotUseful,
            }
        }
    }
}

// ── Updated column types after specialisation ─────────────────────────────────

/// Returns the updated column types after specialising on constructor `c`.
///
/// When expanding a `Constructor::Variant` or `Constructor::Tuple`, the new
/// leading columns are the sub-types; followed by `tail_types`.
fn updated_col_types(
    col_ty: &Type,
    c: &Constructor,
    _b: &BuiltinTyCons,
    arena: &TyConArena,
    tail_types: &[Type],
) -> Vec<Type> {
    let sub_types: Vec<Type> = match c {
        Constructor::Variant {
            union_id,
            variant_idx,
            arity,
            ..
        } => {
            if *arity == 0 {
                vec![]
            } else if union_id.0 as usize >= arena.len() {
                vec![Type::Error; *arity]
            } else {
                let decl = arena.get(*union_id);
                if let TyConKind::Union(schema) = &decl.kind {
                    schema.variants.get(*variant_idx).map_or_else(
                        || vec![Type::Error; *arity],
                        |variant| variant_payload_types(variant, col_ty, schema),
                    )
                } else {
                    vec![Type::Error; *arity]
                }
            }
        }
        Constructor::Record {
            record_id, arity, ..
        } => {
            if *arity == 0 {
                vec![]
            } else if record_id.0 as usize >= arena.len() {
                vec![Type::Error; *arity]
            } else {
                let decl = arena.get(*record_id);
                if let TyConKind::Record(schema) = &decl.kind {
                    schema
                        .record_fields()
                        .iter()
                        .map(|f| f.ty.clone())
                        .collect()
                } else {
                    vec![Type::Error; *arity]
                }
            }
        }
        Constructor::Tuple { arity } => {
            if let Type::Tuple(elem_types) = col_ty {
                elem_types.clone()
            } else {
                vec![Type::Error; *arity]
            }
        }
        Constructor::Literal(_) | Constructor::ListNil => vec![],
        // ListCons has 2 sub-slots: elem and tail (both typed as the list element/list).
        Constructor::ListCons => {
            // col_ty should be List ?elem; extract the elem type.
            if let Type::Con(_, args) = col_ty {
                args.first().map_or_else(
                    || vec![Type::Error, col_ty.clone()],
                    |elem_ty| vec![elem_ty.clone(), col_ty.clone()],
                )
            } else {
                vec![Type::Error, Type::Error]
            }
        }
    };
    let mut result = sub_types;
    result.extend_from_slice(tail_types);
    result
}

/// Extracts the payload types of a union variant, substituting the type
/// parameters from the concrete `col_ty`.
fn variant_payload_types(
    variant: &UnionVariant,
    col_ty: &Type,
    schema: &ridge_types::UnionSchema,
) -> Vec<Type> {
    // Determine the concrete type arguments applied to this union.
    let type_args: Vec<Type> = if let Type::Con(_, args) = col_ty {
        args.clone()
    } else {
        vec![]
    };

    match &variant.kind {
        VariantPayload::Nullary => vec![],
        VariantPayload::Positional(tys) => tys
            .iter()
            .map(|t| subst_ty(t, &schema.params, &type_args))
            .collect(),
        VariantPayload::Record(record_schema) => record_schema
            .record_fields()
            .iter()
            .map(|f| subst_ty(&f.ty, &schema.params, &type_args))
            .collect(),
    }
}

/// Apply a parallel substitution `params[i] → args[i]` to `ty`.
fn subst_ty(ty: &Type, params: &[ridge_types::TyVid], args: &[Type]) -> Type {
    match ty {
        Type::Var(v) => {
            if let Some(pos) = params.iter().position(|p| p == v) {
                if let Some(arg) = args.get(pos) {
                    return arg.clone();
                }
            }
            ty.clone()
        }
        Type::Con(id, sub_args) => {
            let new_sub: Vec<Type> = sub_args.iter().map(|a| subst_ty(a, params, args)).collect();
            Type::Con(*id, new_sub)
        }
        Type::Tuple(ts) => {
            let new_ts: Vec<Type> = ts.iter().map(|t| subst_ty(t, params, args)).collect();
            Type::Tuple(new_ts)
        }
        _ => ty.clone(),
    }
}

// ── Witness builders ──────────────────────────────────────────────────────────

/// Builds a `Vec<MatchWitness>` from a candidate row `q`.
///
/// Each column in `q` contributes one `MatchWitness` (kind = `Missing`).
fn witness_from_q(q: &[NormPat]) -> Vec<MatchWitness> {
    q.iter()
        .map(|p| MatchWitness {
            example: norm_pat_to_witness_pat(p),
            kind: WitnessKind::Missing,
        })
        .collect()
}

fn norm_pat_to_witness_pat(p: &NormPat) -> WitnessPat {
    match p {
        NormPat::Wildcard => WitnessPat::Wild,
        NormPat::Literal(k) => WitnessPat::Lit(lit_key_to_str(k)),
        NormPat::Ctor(c, sub) => {
            let sub_wp: Vec<WitnessPat> = sub.iter().map(norm_pat_to_witness_pat).collect();
            ctor_to_witness_pat(c, sub_wp)
        }
    }
}

fn ctor_to_witness_pat(c: &Constructor, sub: Vec<WitnessPat>) -> WitnessPat {
    match c {
        Constructor::Variant { name, .. } | Constructor::Record { name, .. } => WitnessPat::Ctor {
            name: name.clone(),
            args: sub,
        },
        Constructor::Tuple { .. } => WitnessPat::Tuple(sub),
        Constructor::Literal(k) => WitnessPat::Lit(lit_key_to_str(k)),
        Constructor::ListNil => WitnessPat::Lit("[]".to_string()),
        Constructor::ListCons => WitnessPat::Ctor {
            name: "::".to_string(),
            args: sub,
        },
    }
}

fn lit_key_to_str(k: &LitKey) -> String {
    match k {
        LitKey::BoolTrue => "true".to_string(),
        LitKey::BoolFalse => "false".to_string(),
        LitKey::Other(s) => s.clone(),
    }
}

/// Builds a `MatchWitness` for a missing constructor (all sub-patterns are
/// wildcards).
fn witness_for_ctor(c: &Constructor) -> MatchWitness {
    let sub: Vec<WitnessPat> = vec![WitnessPat::Wild; c.arity()];
    MatchWitness {
        example: ctor_to_witness_pat(c, sub),
        kind: WitnessKind::Missing,
    }
}

/// Builds a `MatchWitness` for a missing literal.
fn witness_for_lit(k: &LitKey) -> MatchWitness {
    MatchWitness {
        example: WitnessPat::Lit(lit_key_to_str(k)),
        kind: WitnessKind::Missing,
    }
}

/// Reconstructs a single `MatchWitness` for the parent constructor `c` by
/// consuming the first `c.arity()` entries from `witnesses` as sub-patterns.
///
/// Returns `(sub_count, reconstructed_witness)`.
fn lift_witness_for_ctor(c: &Constructor, witnesses: &[MatchWitness]) -> (usize, MatchWitness) {
    let arity = c.arity();
    // Safety: callers guarantee witnesses has at least `arity` entries.
    let safe_arity = arity.min(witnesses.len());
    let sub_wps: Vec<WitnessPat> = witnesses[..safe_arity]
        .iter()
        .map(|w| w.example.clone())
        .collect();
    // Pad with wildcards if not enough (defensive).
    let mut padded = sub_wps;
    while padded.len() < arity {
        padded.push(WitnessPat::Wild);
    }
    let wp = ctor_to_witness_pat(c, padded);
    let lifted = MatchWitness {
        example: wp,
        kind: WitnessKind::Missing,
    };
    (safe_arity, lifted)
}

// ── Witness rendering helper ──────────────────────────────────────────────────

/// Renders a `WitnessPat` as a human-readable string.
///
/// Used to populate the `witnesses: Vec<String>` field of `T016`.
pub fn render_witness(w: &WitnessPat) -> String {
    match w {
        WitnessPat::Wild => "_".to_string(),
        WitnessPat::Lit(s) => s.clone(),
        WitnessPat::Ctor { name, args } => {
            if args.is_empty() {
                name.clone()
            } else {
                let rendered_args: Vec<String> = args.iter().map(render_witness).collect();
                format!("{} {}", name, rendered_args.join(" "))
            }
        }
        WitnessPat::Tuple(elems) => {
            let rendered: Vec<String> = elems.iter().map(render_witness).collect();
            format!("({})", rendered.join(", "))
        }
        WitnessPat::Record { ctor, fields } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(name, pat)| format!("{} = {}", name, render_witness(pat)))
                .collect();
            format!("{} {{ {} }}", ctor, field_strs.join(", "))
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// True if `ty` (or any nested element type) is `Type::Error`.
///
/// Used by [`check_exhaustiveness`] to silently bail when the scrutinee
/// inherits an absorbed error (R5: cascade-silent invariant).
fn scrutinee_contains_error(ty: &Type) -> bool {
    match ty {
        Type::Error => true,
        Type::Tuple(elems) => elems.iter().any(scrutinee_contains_error),
        Type::Con(_, args) => args.iter().any(scrutinee_contains_error),
        Type::Alias { body, .. } => scrutinee_contains_error(body),
        _ => false,
    }
}

/// Check exhaustiveness and redundancy for a `match scrutinee { arms... }`.
///
/// # Arguments
///
/// - `ctx`          — mutable inference context (errors are pushed here).
/// - `arena`        — type-constructor arena (for looking up union/record
///   schemas for user-defined types).
/// - `b`            — built-in `TyCon` handles.
/// - `scrutinee_ty` — deep-resolved type of the scrutinee.
/// - `arms`         — the match arms (pattern + body; exhaustiveness only reads
///   the patterns).
/// - `span`         — span of the entire `match` expression (for T016).
///
/// # Side effects
///
/// Pushes `T016 NonExhaustiveMatch` and/or `T017 RedundantPattern` into
/// `ctx.errors`.
///
/// # Ordering
///
/// Must be called AFTER the per-arm body type-check (§4.12 matiz).
pub fn check_exhaustiveness(
    ctx: &mut InferCtx,
    arena: &TyConArena,
    b: &BuiltinTyCons,
    scrutinee_ty: &Type,
    arms: &[ridge_ast::MatchArm],
    span: Span,
) {
    // R5: cascade silently when the scrutinee type carries `Type::Error` from
    // upstream (e.g. stubbed record types like `Request`/`Response` in
    // `examples/url_shortener.ridge`).  Firing T016/T017 on absorbed errors
    // drowns the user with cascading diagnostics.  The original error has
    // already been emitted (or intentionally suppressed for stubs).
    if scrutinee_contains_error(scrutinee_ty) {
        return;
    }

    // ── 1. Build the pattern matrix (one row per arm, one column) ─────────────
    //
    // Arms with a `when` guard are excluded from the matrix: a guarded arm
    // only matches when the guard evaluates to true, and the checker cannot
    // prove that statically. Treating a guarded arm as covering its pattern
    // unconditionally would fire spurious T017s for every arm below it and
    // mask genuine T016 gaps when every arm is guarded.
    let mut matrix = PatternMatrix::default();
    for arm in arms {
        if arm.guard.is_none() {
            matrix.push(vec![lift_pattern(&arm.pattern)]);
        }
    }

    let column_types = vec![scrutinee_ty.clone()];

    // ── 2. Exhaustiveness check: useful(matrix, [Wildcard]) ──────────────────
    let wildcard_q = vec![NormPat::Wildcard];
    if let Usefulness::UsefulWithWitness(first_witness) =
        useful(&matrix, &wildcard_q, &column_types, b, arena)
    {
        // Collect missing witnesses (capped at MAX_WITNESSES).
        // total_missing = exact count of top-level missing constructors for
        // union types; 1 for single-ctor or non-closed types.
        let (stored_witnesses, total_missing) =
            collect_all_missing(scrutinee_ty, &matrix, b, arena, first_witness);

        if total_missing == 0 {
            // Degenerate case — guard against spurious T016.
            // (Should not happen with a correct useful() result, but defensive.)
        } else {
            let witness_strings: Vec<String> = stored_witnesses
                .iter()
                .map(|w| render_witness(&w.example))
                .collect();

            ctx.errors.push(TypeError::NonExhaustiveMatch {
                scrutinee_ty: render_type(scrutinee_ty, arena),
                witnesses: witness_strings,
                total_missing,
                span,
            });
        }
    }

    // ── 3. Redundancy check: for each arm i, useful(matrix[0..i], arm[i]) ────
    //
    // Same guard caveat as in §1: a guarded arm only fires when the runtime
    // guard is true, so it cannot count toward the prefix used to judge
    // whether later arms are redundant. The arm itself is still checked
    // against the prefix — a guarded arm whose pattern is already covered
    // by an earlier unguarded arm is unreachable regardless of the guard,
    // so T017 still fires in that genuine case.
    let mut prefix_matrix = PatternMatrix::default();
    for (i, arm) in arms.iter().enumerate() {
        let arm_row = vec![lift_pattern(&arm.pattern)];
        match useful(&prefix_matrix, &arm_row, &column_types, b, arena) {
            Usefulness::NotUseful => {
                // This arm is covered by earlier arms.
                ctx.errors.push(TypeError::RedundantPattern {
                    arm_index: i,
                    span: arm.span,
                });
            }
            Usefulness::UsefulWithWitness(_) => {
                // Arm is useful — add it to the prefix if it has no guard.
            }
        }
        if arm.guard.is_none() {
            prefix_matrix.push(arm_row);
        }
    }
}

/// Collects missing witnesses, capping at `MAX_WITNESSES`.
///
/// Returns `(capped_witnesses, total_missing)` where `total_missing` is the
/// true count of distinct missing values/constructors.
///
/// For union types: counts missing top-level constructors exactly.
/// For tuple/record/non-closed types: uses the single witness from `useful`.
fn collect_all_missing(
    scrutinee_ty: &Type,
    matrix: &PatternMatrix,
    b: &BuiltinTyCons,
    arena: &TyConArena,
    first_witness: Vec<MatchWitness>,
) -> (Vec<MatchWitness>, usize) {
    // For union types with multiple variants, we can count missing top-level ctors.
    if let Some(ctors) = ctor_set_for(scrutinee_ty, b, arena) {
        if ctors.len() > 1 {
            // Multiple ctors: count which are missing from the matrix's first column.
            let head_ctors_in_p = collect_head_ctors(matrix, &ctors);
            let missing_ctors: Vec<&Constructor> = ctors
                .iter()
                .filter(|c| !head_ctors_in_p.iter().any(|h| ctor_name_eq(h, c)))
                .collect();

            let total_missing = missing_ctors.len();
            if total_missing > 0 {
                let stored: Vec<MatchWitness> = missing_ctors
                    .into_iter()
                    .take(MAX_WITNESSES)
                    .map(witness_for_ctor)
                    .collect();
                return (stored, total_missing);
            }
            // All ctors present but still non-exhaustive (sub-pattern gap).
            // Fall through to use the first_witness from useful().
        }
    }
    // Single-ctor types (tuples, records) or non-closed types (Int, etc.):
    // use the witness from useful() directly. total_missing = 1.
    let stored: Vec<MatchWitness> = first_witness.into_iter().take(MAX_WITNESSES).collect();
    let total_missing = usize::from(!stored.is_empty());
    (stored, total_missing)
}

// ── Type rendering helper ─────────────────────────────────────────────────────

/// Renders a `Type` as a human-readable string for diagnostic messages.
fn render_type(ty: &Type, arena: &TyConArena) -> String {
    match ty {
        Type::Con(id, args) => {
            if id.0 as usize >= arena.len() {
                return format!("?{}", id.0);
            }
            let name = &arena.get(*id).name;
            if args.is_empty() {
                name.clone()
            } else {
                let arg_strs: Vec<String> = args.iter().map(|a| render_type(a, arena)).collect();
                format!("{} {}", name, arg_strs.join(" "))
            }
        }
        Type::Tuple(ts) => {
            let parts: Vec<String> = ts.iter().map(|t| render_type(t, arena)).collect();
            format!("({})", parts.join(", "))
        }
        Type::Var(v) => format!("?a{}", v.0),
        Type::Error => "Error".to_string(),
        Type::Fn { .. } => "Fn".to_string(),
        Type::Alias { name, body: _ } => {
            if name.0 as usize >= arena.len() {
                return format!("?{}", name.0);
            }
            arena.get(*name).name.clone()
        }
        _ => "_".to_string(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Ident, MatchArm, Pattern, Span};
    use ridge_types::{
        BuiltinTyCons, RecordField, RecordSchema, TyConArena, TyConDecl, TyConId, TyConKind, Type,
        UnionSchema, UnionVariant, VariantPayload,
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn dummy_span() -> Span {
        Span::point(0)
    }

    fn make_ident(text: &str) -> Ident {
        Ident {
            text: text.to_string(),
            span: dummy_span(),
        }
    }

    /// Creates a fresh arena + builtins, returns both.
    fn make_builtins() -> (TyConArena, BuiltinTyCons) {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        (arena, b)
    }

    fn wildcard_arm() -> MatchArm {
        MatchArm {
            pattern: Pattern::Wildcard { span: dummy_span() },
            guard: None,
            body: ridge_ast::Expr::Literal(ridge_ast::Literal::IntDec {
                raw: "0".to_string(),
                span: dummy_span(),
            }),
            span: dummy_span(),
        }
    }

    fn bool_arm(value: bool) -> MatchArm {
        MatchArm {
            pattern: Pattern::Literal {
                lit: ridge_ast::Literal::Bool {
                    value,
                    span: dummy_span(),
                },
                span: dummy_span(),
            },
            guard: None,
            body: ridge_ast::Expr::Literal(ridge_ast::Literal::IntDec {
                raw: "1".to_string(),
                span: dummy_span(),
            }),
            span: dummy_span(),
        }
    }

    fn int_arm(raw: &str) -> MatchArm {
        MatchArm {
            pattern: Pattern::Literal {
                lit: ridge_ast::Literal::IntDec {
                    raw: raw.to_string(),
                    span: dummy_span(),
                },
                span: dummy_span(),
            },
            guard: None,
            body: ridge_ast::Expr::Literal(ridge_ast::Literal::IntDec {
                raw: "1".to_string(),
                span: dummy_span(),
            }),
            span: dummy_span(),
        }
    }

    fn ctor_arm(name: &str, args: Vec<Pattern>) -> MatchArm {
        MatchArm {
            pattern: Pattern::Constructor {
                name: make_ident(name),
                fields: None,
                has_rest: false,
                args,
                span: dummy_span(),
            },
            guard: None,
            body: ridge_ast::Expr::Literal(ridge_ast::Literal::IntDec {
                raw: "1".to_string(),
                span: dummy_span(),
            }),
            span: dummy_span(),
        }
    }

    fn tuple_arm(pats: Vec<Pattern>) -> MatchArm {
        MatchArm {
            pattern: Pattern::Tuple {
                elems: pats,
                span: dummy_span(),
            },
            guard: None,
            body: ridge_ast::Expr::Literal(ridge_ast::Literal::IntDec {
                raw: "1".to_string(),
                span: dummy_span(),
            }),
            span: dummy_span(),
        }
    }

    fn record_ctor_arm(name: &str) -> MatchArm {
        MatchArm {
            pattern: Pattern::Constructor {
                name: make_ident(name),
                fields: Some(vec![]),
                has_rest: false,
                args: vec![],
                span: dummy_span(),
            },
            guard: None,
            body: ridge_ast::Expr::Literal(ridge_ast::Literal::IntDec {
                raw: "1".to_string(),
                span: dummy_span(),
            }),
            span: dummy_span(),
        }
    }

    /// Registers a union type in the arena and returns `(TyConId, Type)`.
    fn add_union(
        arena: &mut TyConArena,
        name: &str,
        variants: Vec<UnionVariant>,
    ) -> (TyConId, Type) {
        let id = arena.intern(TyConDecl {
            id: TyConId(0),
            name: name.to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants,
            }),
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        (id, Type::Con(id, vec![]))
    }

    /// Registers a record type in the arena and returns `(TyConId, Type)`.
    fn add_record(
        arena: &mut TyConArena,
        name: &str,
        fields: Vec<(&str, Type)>,
    ) -> (TyConId, Type) {
        let record_fields: Vec<RecordField> = fields
            .into_iter()
            .map(|(n, t)| RecordField {
                name: n.to_string(),
                ty: t,
            })
            .collect();
        let id = arena.intern(TyConDecl {
            id: TyConId(0),
            name: name.to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(vec![], record_fields)),
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        (id, Type::Con(id, vec![]))
    }

    fn no_errors(errs: &[TypeError]) {
        assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
    }

    fn has_t016(errs: &[TypeError]) -> &TypeError {
        errs.iter()
            .find(|e| e.code() == "T016")
            .unwrap_or_else(|| panic!("expected T016, errors: {errs:?}"))
    }

    fn has_t017(errs: &[TypeError]) -> &TypeError {
        errs.iter()
            .find(|e| e.code() == "T017")
            .unwrap_or_else(|| panic!("expected T017, errors: {errs:?}"))
    }

    // ── Test 1: match_bool_exhaustive ─────────────────────────────────────────
    /// `match true { true -> 1; false -> 0 }` — no T016.
    #[test]
    fn match_bool_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![bool_arm(true), bool_arm(false)];
        let scrutinee_ty = Type::Con(b.bool, vec![]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    // ── Test 2: match_bool_missing_false ──────────────────────────────────────
    /// `match b { true -> 1 }` — T016 with witness `false`.
    #[test]
    fn match_bool_missing_false() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![bool_arm(true)];
        let scrutinee_ty = Type::Con(b.bool, vec![]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        let t016 = has_t016(&ctx.errors);
        if let TypeError::NonExhaustiveMatch {
            witnesses,
            total_missing,
            ..
        } = t016
        {
            assert_eq!(*total_missing, 1, "one missing: false");
            assert_eq!(witnesses.len(), 1);
            assert_eq!(witnesses[0], "false");
        } else {
            panic!("expected T016");
        }
    }

    // ── Test 3: match_option_exhaustive ──────────────────────────────────────
    /// `match opt { Some x -> x; None -> 0 }` — no T016.
    #[test]
    fn match_option_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![
            ctor_arm(
                "Some",
                vec![Pattern::Var {
                    name: make_ident("x"),
                    span: dummy_span(),
                }],
            ),
            ctor_arm("None", vec![]),
        ];
        let scrutinee_ty = Type::Con(b.option, vec![Type::Con(b.int, vec![])]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    // ── Test 4: match_option_missing_none ────────────────────────────────────
    /// `match opt { Some x -> x }` — T016 with witness `None`.
    #[test]
    fn match_option_missing_none() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![ctor_arm(
            "Some",
            vec![Pattern::Var {
                name: make_ident("x"),
                span: dummy_span(),
            }],
        )];
        let scrutinee_ty = Type::Con(b.option, vec![Type::Con(b.int, vec![])]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        let t016 = has_t016(&ctx.errors);
        if let TypeError::NonExhaustiveMatch {
            witnesses,
            total_missing,
            ..
        } = t016
        {
            assert_eq!(*total_missing, 1, "one missing: None");
            assert_eq!(witnesses.len(), 1);
            assert_eq!(witnesses[0], "None");
        } else {
            panic!("expected T016");
        }
    }

    // ── Test 5: match_user_union_3_variants_partial ──────────────────────────
    /// `match shape { Circle r -> 1 }` — T016 with witnesses `Rectangle _ _`,
    /// `Triangle _ _ _`.  (`DoD` test)
    ///
    /// `Shape = Circle Float | Rectangle Float Float | Triangle Float Float Float`
    #[test]
    fn match_user_union_3_variants_partial() {
        let (mut arena, b) = make_builtins();
        let float_ty = Type::Con(b.float, vec![]);
        let (_shape_id, shape_ty) = add_union(
            &mut arena,
            "Shape",
            vec![
                UnionVariant {
                    name: "Circle".to_string(),
                    kind: VariantPayload::Positional(vec![float_ty.clone()]),
                },
                UnionVariant {
                    name: "Rectangle".to_string(),
                    kind: VariantPayload::Positional(vec![float_ty.clone(), float_ty.clone()]),
                },
                UnionVariant {
                    name: "Triangle".to_string(),
                    kind: VariantPayload::Positional(vec![
                        float_ty.clone(),
                        float_ty.clone(),
                        float_ty,
                    ]),
                },
            ],
        );
        let mut ctx = InferCtx::new();
        let arms = vec![ctor_arm(
            "Circle",
            vec![Pattern::Var {
                name: make_ident("r"),
                span: dummy_span(),
            }],
        )];
        check_exhaustiveness(&mut ctx, &arena, &b, &shape_ty, &arms, dummy_span());
        let t016 = has_t016(&ctx.errors);
        if let TypeError::NonExhaustiveMatch {
            witnesses,
            total_missing,
            ..
        } = t016
        {
            assert_eq!(*total_missing, 2, "two missing: Rectangle, Triangle");
            assert_eq!(witnesses.len(), 2);
            assert!(
                witnesses.iter().any(|w| w == "Rectangle _ _"),
                "expected Rectangle _ _ in witnesses, got {witnesses:?}"
            );
            assert!(
                witnesses.iter().any(|w| w == "Triangle _ _ _"),
                "expected Triangle _ _ _ in witnesses, got {witnesses:?}"
            );
        } else {
            panic!("expected T016");
        }
    }

    // ── Test 6: match_50_variants_witness_capped ─────────────────────────────
    /// Synthetic 50-variant union, only first variant matched.
    /// T016: witnesses capped at `MAX_WITNESSES=3`, `total_missing` = 49.
    #[test]
    fn match_50_variants_witness_capped() {
        let (mut arena, b) = make_builtins();
        let variants: Vec<UnionVariant> = (0..50)
            .map(|i| UnionVariant {
                name: format!("V{i}"),
                kind: VariantPayload::Nullary,
            })
            .collect();
        let (_id, ty) = add_union(&mut arena, "Big50", variants);

        let mut ctx = InferCtx::new();
        let arms = vec![ctor_arm("V0", vec![])];
        check_exhaustiveness(&mut ctx, &arena, &b, &ty, &arms, dummy_span());

        let t016 = has_t016(&ctx.errors);
        if let TypeError::NonExhaustiveMatch {
            witnesses,
            total_missing,
            ..
        } = t016
        {
            assert_eq!(*total_missing, 49, "49 missing variants");
            assert_eq!(witnesses.len(), MAX_WITNESSES, "capped at MAX_WITNESSES=3");
        } else {
            panic!("expected T016");
        }
    }

    // ── Test 7: match_wildcard_exhaustive ─────────────────────────────────────
    /// `match x { _ -> 1 }` — no T016.
    #[test]
    fn match_wildcard_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![wildcard_arm()];
        let scrutinee_ty = Type::Con(b.int, vec![]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    // ── Test 8: match_redundant_arm_T017 ─────────────────────────────────────
    /// `match x { _ -> 1; 0 -> 2 }` — T017 on arm 1.  (`DoD` test)
    #[test]
    fn match_redundant_arm_t017() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![wildcard_arm(), int_arm("0")];
        let scrutinee_ty = Type::Con(b.int, vec![]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        let t017 = has_t017(&ctx.errors);
        if let TypeError::RedundantPattern { arm_index, .. } = t017 {
            assert_eq!(*arm_index, 1, "arm 1 is redundant");
        } else {
            panic!("expected T017");
        }
    }

    // ── Test 9: match_redundant_after_specific ───────────────────────────────
    /// `match opt { Some 1 -> 1; Some 1 -> 2; None -> 0 }` — T017 on arm 1.
    #[test]
    fn match_redundant_after_specific() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let some_1_arm = ctor_arm(
            "Some",
            vec![Pattern::Literal {
                lit: ridge_ast::Literal::IntDec {
                    raw: "1".to_string(),
                    span: dummy_span(),
                },
                span: dummy_span(),
            }],
        );
        let arms = vec![
            some_1_arm.clone(),
            some_1_arm, // duplicate → redundant
            ctor_arm("None", vec![]),
        ];
        let scrutinee_ty = Type::Con(b.option, vec![Type::Con(b.int, vec![])]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        let t017 = has_t017(&ctx.errors);
        if let TypeError::RedundantPattern { arm_index, .. } = t017 {
            assert_eq!(*arm_index, 1, "arm 1 (second Some 1) is redundant");
        } else {
            panic!("expected T017");
        }
    }

    // ── Test 10: match_tuple_exhaustive ──────────────────────────────────────
    /// `match (a, b) { (true, _) -> 1; (false, _) -> 2 }` — no T016.  (`DoD` test)
    #[test]
    fn match_tuple_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let bool_ty = Type::Con(b.bool, vec![]);
        let scrutinee_ty = Type::Tuple(vec![bool_ty.clone(), bool_ty]);
        let arms = vec![
            tuple_arm(vec![
                Pattern::Literal {
                    lit: ridge_ast::Literal::Bool {
                        value: true,
                        span: dummy_span(),
                    },
                    span: dummy_span(),
                },
                Pattern::Wildcard { span: dummy_span() },
            ]),
            tuple_arm(vec![
                Pattern::Literal {
                    lit: ridge_ast::Literal::Bool {
                        value: false,
                        span: dummy_span(),
                    },
                    span: dummy_span(),
                },
                Pattern::Wildcard { span: dummy_span() },
            ]),
        ];
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    // ── Test 11: match_tuple_missing ─────────────────────────────────────────
    /// `match (a, b) { (true, true) -> 1 }` — T016.
    #[test]
    fn match_tuple_missing() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let bool_ty = Type::Con(b.bool, vec![]);
        let scrutinee_ty = Type::Tuple(vec![bool_ty.clone(), bool_ty]);
        let arms = vec![tuple_arm(vec![
            Pattern::Literal {
                lit: ridge_ast::Literal::Bool {
                    value: true,
                    span: dummy_span(),
                },
                span: dummy_span(),
            },
            Pattern::Literal {
                lit: ridge_ast::Literal::Bool {
                    value: true,
                    span: dummy_span(),
                },
                span: dummy_span(),
            },
        ])];
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        has_t016(&ctx.errors);
    }

    // ── Test 12: match_int_literal_default_required ──────────────────────────
    /// `match n { 0 -> 1 }` — T016 fires (Int is non-closed; no default arm).
    #[test]
    fn match_int_literal_default_required() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![int_arm("0")];
        let scrutinee_ty = Type::Con(b.int, vec![]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        has_t016(&ctx.errors);
    }

    // ── Test 13: match_int_literal_with_default ──────────────────────────────
    /// `match n { 0 -> 1; _ -> 2 }` — no T016.
    #[test]
    fn match_int_literal_with_default() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![int_arm("0"), wildcard_arm()];
        let scrutinee_ty = Type::Con(b.int, vec![]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    // ── Test 14: match_record_pattern ────────────────────────────────────────
    /// `match user { User { name, .. } -> name }` — single record-ctor →
    /// exhaustive.
    #[test]
    fn match_record_pattern() {
        let (mut arena, b) = make_builtins();
        let text_ty = Type::Con(b.text, vec![]);
        let (_user_id, user_ty) = add_record(
            &mut arena,
            "User",
            vec![("name", text_ty), ("age", Type::Con(b.int, vec![]))],
        );
        let mut ctx = InferCtx::new();
        // Record-body constructor pattern — fields are Some(...).
        // This lifts to Wildcard in lift_pattern, so it covers everything.
        let arms = vec![record_ctor_arm("User")];
        check_exhaustiveness(&mut ctx, &arena, &b, &user_ty, &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    // ── Guard handling ───────────────────────────────────────────────────────

    /// Attach a `when guard` to an arm. The guard expression is irrelevant to
    /// the exhaustiveness algorithm — only its presence matters.
    fn with_guard(mut arm: MatchArm) -> MatchArm {
        arm.guard = Some(ridge_ast::Expr::Literal(ridge_ast::Literal::Bool {
            value: true,
            span: dummy_span(),
        }));
        arm
    }

    /// `match n { m when ... -> 1; m when ... -> 2; m when ... -> 3; _ -> 4 }`
    /// — no T017. Mirrors the canonical guarded-fizzbuzz shape. Pre-fix the
    /// checker treated each guarded `m` as a variable pattern covering all
    /// values and flagged arms 1, 2, 3 as redundant.
    #[test]
    fn match_guarded_variable_arms_not_redundant() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![
            with_guard(wildcard_arm()),
            with_guard(wildcard_arm()),
            with_guard(wildcard_arm()),
            wildcard_arm(),
        ];
        let scrutinee_ty = Type::Con(b.int, vec![]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    /// `match n { _ when ... -> 1 }` — T016 fires. A single guarded arm
    /// cannot cover the scrutinee statically; the match must have an
    /// unguarded fall-through.
    #[test]
    fn match_single_guarded_arm_not_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![with_guard(wildcard_arm())];
        let scrutinee_ty = Type::Con(b.int, vec![]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        assert!(
            ctx.errors
                .iter()
                .any(|e| matches!(e, TypeError::NonExhaustiveMatch { .. })),
            "expected T016 NonExhaustiveMatch when every arm carries a guard, got: {:?}",
            ctx.errors,
        );
    }

    /// `match b { true -> 1; true when ... -> 2 }` — T017 still fires.
    /// A guarded arm whose pattern is already covered by an earlier
    /// unguarded arm is unreachable regardless of the guard.
    #[test]
    fn match_guarded_arm_redundant_after_unguarded() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![bool_arm(true), with_guard(bool_arm(true)), bool_arm(false)];
        let scrutinee_ty = Type::Con(b.bool, vec![]);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        let t017 = has_t017(&ctx.errors);
        if let TypeError::RedundantPattern { arm_index, .. } = t017 {
            assert_eq!(
                *arm_index, 1,
                "arm 1 (true when ...) is redundant after arm 0"
            );
        } else {
            panic!("expected T017 on the guarded duplicate");
        }
    }

    // ── List pattern exhaustiveness (D258) ────────────────────────────────────

    fn list_arm(pat: Pattern) -> MatchArm {
        MatchArm {
            pattern: pat,
            guard: None,
            body: ridge_ast::Expr::Literal(ridge_ast::Literal::IntDec {
                raw: "1".to_string(),
                span: dummy_span(),
            }),
            span: dummy_span(),
        }
    }

    fn list_nil_arm() -> MatchArm {
        list_arm(Pattern::ListNil { span: dummy_span() })
    }

    fn cons_wildcard_arm() -> MatchArm {
        // `_ :: _` — matches any non-empty list
        list_arm(Pattern::Cons {
            head: Box::new(Pattern::Wildcard { span: dummy_span() }),
            tail: Box::new(Pattern::Wildcard { span: dummy_span() }),
            span: dummy_span(),
        })
    }

    fn make_list_ty(b: &BuiltinTyCons) -> Type {
        Type::Con(b.list, vec![Type::Con(b.int, vec![])])
    }

    /// `match xs { [] -> ..; _ :: _ -> .. }` — exhaustive (no T016).
    #[test]
    fn match_list_nil_and_cons_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![list_nil_arm(), cons_wildcard_arm()];
        let scrutinee_ty = make_list_ty(&b);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    /// `match xs { [] -> .. }` — non-exhaustive (T016, missing `_ :: _`).
    #[test]
    fn match_list_nil_only_non_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![list_nil_arm()];
        let scrutinee_ty = make_list_ty(&b);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        has_t016(&ctx.errors);
    }

    /// Desugared `[_, ..]` (= `_ :: _`) + `[]` — exhaustive via `desugar_list`.
    ///
    /// Builds `Pattern::List { elements: [Elem(_), Rest { bind: None }] }` and
    /// checks that it desugars correctly and the pair is exhaustive.
    #[test]
    fn match_list_bracket_prefix_rest_exhaustive() {
        use ridge_ast::pattern::ListPatElem;

        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();

        // `[_, ..]` as a List node
        let list_pat = Pattern::List {
            elements: vec![
                ListPatElem::Elem(Pattern::Wildcard { span: dummy_span() }),
                ListPatElem::Rest {
                    bind: None,
                    span: dummy_span(),
                },
            ],
            span: dummy_span(),
        };

        let arms = vec![list_arm(list_pat), list_nil_arm()];
        let scrutinee_ty = make_list_ty(&b);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    /// Single-arm `[_, ..]` without `[]` — non-exhaustive (missing empty list).
    #[test]
    fn match_list_bracket_prefix_rest_missing_nil() {
        use ridge_ast::pattern::ListPatElem;

        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();

        let list_pat = Pattern::List {
            elements: vec![
                ListPatElem::Elem(Pattern::Wildcard { span: dummy_span() }),
                ListPatElem::Rest {
                    bind: None,
                    span: dummy_span(),
                },
            ],
            span: dummy_span(),
        };

        let arms = vec![list_arm(list_pat)];
        let scrutinee_ty = make_list_ty(&b);
        check_exhaustiveness(&mut ctx, &arena, &b, &scrutinee_ty, &arms, dummy_span());
        has_t016(&ctx.errors);
    }

    // ── Suffix / middle rest (slice surface) ─────────────────────────────────
    //
    // Suffix and middle elements are irrefutable, so a `[.., last]` /
    // `[a, .., b]` pattern is equivalent to a prefix pattern of the same minimum
    // length (`_ :: _` / `a :: _ :: _`) and is desugared as such for the matrix.

    fn list_elem_var(name: &str) -> ListPatElem {
        ListPatElem::Elem(Pattern::Var {
            name: make_ident(name),
            span: dummy_span(),
        })
    }
    fn list_rest() -> ListPatElem {
        ListPatElem::Rest {
            bind: None,
            span: dummy_span(),
        }
    }
    fn list_node(elements: Vec<ListPatElem>) -> Pattern {
        Pattern::List {
            elements,
            span: dummy_span(),
        }
    }

    /// `[] + [.., last]` — exhaustive (`[.., last]` ≡ any non-empty list).
    #[test]
    fn match_list_suffix_rest_plus_nil_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![
            list_nil_arm(),
            list_arm(list_node(vec![list_rest(), list_elem_var("last")])),
        ];
        check_exhaustiveness(&mut ctx, &arena, &b, &make_list_ty(&b), &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    /// `[.., last]` alone — non-exhaustive (missing `[]`).
    #[test]
    fn match_list_suffix_rest_only_non_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![list_arm(list_node(vec![
            list_rest(),
            list_elem_var("last"),
        ]))];
        check_exhaustiveness(&mut ctx, &arena, &b, &make_list_ty(&b), &arms, dummy_span());
        has_t016(&ctx.errors);
    }

    /// `[] + [first, .., last]` — NON-exhaustive: the middle-rest needs length
    /// two or more, so single-element lists `[_]` are uncovered. (The unsound
    /// length-blind model wrongly accepted this.)
    #[test]
    fn match_list_middle_rest_plus_nil_missing_singleton() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![
            list_nil_arm(),
            list_arm(list_node(vec![
                list_elem_var("first"),
                list_rest(),
                list_elem_var("last"),
            ])),
        ];
        check_exhaustiveness(&mut ctx, &arena, &b, &make_list_ty(&b), &arms, dummy_span());
        has_t016(&ctx.errors);
    }

    /// `[] + [_] + [first, .., last]` — exhaustive (lengths 0, 1, and >= 2).
    #[test]
    fn match_list_middle_rest_plus_nil_plus_singleton_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![
            list_nil_arm(),
            list_arm(list_node(vec![ListPatElem::Elem(Pattern::Wildcard {
                span: dummy_span(),
            })])),
            list_arm(list_node(vec![
                list_elem_var("first"),
                list_rest(),
                list_elem_var("last"),
            ])),
        ];
        check_exhaustiveness(&mut ctx, &arena, &b, &make_list_ty(&b), &arms, dummy_span());
        no_errors(&ctx.errors);
    }

    /// `[a, b, ..] + []` — NON-exhaustive (missing single-element lists).
    /// Guards the unsoundness that prompted the slice-exhaustiveness rework.
    #[test]
    fn match_list_mixed_min_lengths_missing_singleton() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![
            list_arm(list_node(vec![
                list_elem_var("a"),
                list_elem_var("b"),
                list_rest(),
            ])),
            list_nil_arm(),
        ];
        check_exhaustiveness(&mut ctx, &arena, &b, &make_list_ty(&b), &arms, dummy_span());
        has_t016(&ctx.errors);
    }

    /// `[a, b, ..] + [x] + []` — exhaustive (lengths >= 2, 1, and 0).
    #[test]
    fn match_list_mixed_min_lengths_exhaustive() {
        let (arena, b) = make_builtins();
        let mut ctx = InferCtx::new();
        let arms = vec![
            list_arm(list_node(vec![
                list_elem_var("a"),
                list_elem_var("b"),
                list_rest(),
            ])),
            list_arm(list_node(vec![list_elem_var("x")])),
            list_nil_arm(),
        ];
        check_exhaustiveness(&mut ctx, &arena, &b, &make_list_ty(&b), &arms, dummy_span());
        no_errors(&ctx.errors);
    }
}
