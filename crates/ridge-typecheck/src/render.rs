//! `Display` + `std::error::Error` for [`TypeError`], plus the [`emit_internal`]
//! helper for T999.
//!
//! # Rendering format
//!
//! All messages follow the spec §5.3 / §5.4 / §6.4 multi-line text shape:
//!
//! ```text
//! {code}: {title}
//!   {detail line}
//!   suggestion: ...
//! ```
//!
//! Ariadne source-span rendering (the `| 12 | fn io …` lines) is added later
//! by `ridge-diagnostics`'s ariadne pass. The `Display` output here is the
//! *prose* portion only — suitable for tests, tracing logs, and simple
//! terminal output without source context.
//!
//! # T999
//!
//! [`emit_internal`] is the canonical emit site for `T999 InternalTypeError`.
//! In debug builds it fires a `debug_assert!` panic to surface invariant
//! violations immediately during development. In release builds the error is
//! pushed to `ctx.errors` and inference continues.

use std::fmt;

use ridge_ast::Span;
use ridge_diagnostics::HasErrorCode;
use ridge_resolve::Severity;

use crate::ctx::InferCtx;
use crate::error::{CapDeclKind, TypeDesc, TypeError};

// ── Display helpers ───────────────────────────────────────────────────────────

/// How a `T003` opens, given a callee that may not have a name.
///
/// An annotation mismatch and a lambda applied in place have nothing to print
/// there, and an empty pair of backticks reads as a name the compiler lost
/// rather than one that never existed. The rest of the sentence carries fine
/// without a subject.
fn arity_subject(callee: &str) -> String {
    if callee.is_empty() {
        "expects".to_owned()
    } else {
        format!("`{callee}` expects")
    }
}

/// The `T057` sentence, agreeing in number on both counts.
///
/// Deliberately says nothing about arguments: `takesPair (1, 2, 3)` passes
/// exactly one, and calling this an arity mismatch sent the reader looking for
/// a fourth argument nobody wrote.
/// Render module names as `` `a` ``, `` `a` or `b` ``, `` `a`, `b` or `c` ``.
///
/// A message that lists the places an instance may go is read as a set of
/// options, so it is written as one. Callers pass a non-empty list; an empty
/// one means there are no options at all, which reads as a different sentence.
fn join_or(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|m| format!("`{m}`")).collect();
    match quoted.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    }
}

fn tuple_width_sentence(expected: usize, found: usize) -> String {
    format!(
        "this tuple has {found} component{s1}, but {expected} {s2} expected",
        s1 = if found == 1 { "" } else { "s" },
        s2 = if expected == 1 { "is" } else { "are" },
    )
}

// ── Display impl ──────────────────────────────────────────────────────────────

impl TypeDesc {
    /// Render under a namer shared with the rest of this diagnostic.
    ///
    /// The sharing is the point. Two types printed by two `VarNamer`s each
    /// start their letters at `a`, so `expected a, found a` can name two
    /// different variables and `expected a, found b` can name one. The pair
    /// helpers this replaced existed for exactly that reason, and only the
    /// sites that remembered to call them were safe.
    pub(crate) fn render_in(
        &self,
        tycons: &[ridge_types::TyConDecl],
        namer: &mut VarNamer,
    ) -> String {
        match self {
            Self::Ty(t) => render_at_depth(t, tycons, 0, namer),
            Self::Text(s) => s.clone(),
            Self::Phrase(p) => (*p).to_owned(),
        }
    }
}

/// Render several descriptions of one diagnostic under a single namer.
///
/// The namer is what makes the letters mean something: within one message,
/// the same type variable prints the same letter and two different ones do
/// not collide. Every entry point that renders a `TypeDesc` goes through here
/// so that property does not depend on the caller remembering it.
pub(crate) fn render_descs(descs: &[&TypeDesc], tycons: &[ridge_types::TyConDecl]) -> Vec<String> {
    render_reserving(|namer| descs.iter().map(|d| d.render_in(tycons, namer)).collect())
}

impl TypeError {
    /// The reader's message.
    ///
    /// Takes the type-constructor table because that is what turns a
    /// [`TypeDesc::Ty`] into a name. `Display` could not take it, which is why
    /// these fields used to arrive pre-rendered as `String` — and why nothing
    /// stopped a construction site from rendering them with `Debug`.
    #[must_use]
    pub fn render(&self, tycons: &[ridge_types::TyConDecl]) -> String {
        render_reserving(|namer| {
            let mut out = String::new();
            // Writing to a `String` is infallible.
            let _ = self.write_message(&mut out, tycons, namer);
            out
        })
    }

    #[expect(clippy::too_many_lines, reason = "one match arm per T### error code")]
    fn write_message(
        &self,
        f: &mut String,
        tycons: &[ridge_types::TyConDecl],
        namer: &mut VarNamer,
    ) -> fmt::Result {
        use fmt::Write as _;

        // The code is written here and nowhere else. Every arm below used to
        // repeat it inside its own format string, in a different file from the
        // `code()` that decides it, with nothing comparing the two. A prefix
        // that drifted would survive the strip in `Diagnostic::message_parts`
        // and reach the reader as two disagreeing codes on one line.
        write!(f, "{}: ", self.code())?;
        match self {
            // ── T001 ──────────────────────────────────────────────────────────
            Self::TypeMismatch {
                expected,
                found,
                hint,
                ..
            } => {
                let expected = expected.render_in(tycons, namer);
                let found = found.render_in(tycons, namer);
                write!(f, "type mismatch\n  expected {expected}, got {found}")?;
                if let Some(h) = hint {
                    write!(f, "\n  hint: {h}")?;
                }
                Ok(())
            }

            // ── T002 ──────────────────────────────────────────────────────────
            Self::TypeMismatchInCall {
                callee,
                arg_index,
                expected,
                found,
                ..
            } => {
                let expected = expected.render_in(tycons, namer);
                let found = found.render_in(tycons, namer);
                write!(
                    f,
                    "type mismatch in call to `{callee}`\n  argument {n}: expected {expected}, got {found}",
                    n = arg_index + 1,
                )
            }

            // ── T003 ──────────────────────────────────────────────────────────
            Self::ArityMismatch {
                callee,
                expected,
                found,
                hint,
                ..
            } => {
                write!(
                    f,
                    "arity mismatch\n  {subject} {expected} argument{s1}, got {found}",
                    subject = arity_subject(callee),
                    s1 = if *expected == 1 { "" } else { "s" },
                )?;
                if let Some(h) = hint {
                    write!(f, "\n  hint: {h}")?;
                }
                Ok(())
            }

            // ── T004 ──────────────────────────────────────────────────────────
            Self::MissingField { record, field, .. } => {
                write!(
                    f,
                    "missing field in record construction\n  record `{record}` requires field `{field}`"
                )
            }

            // ── T005 ──────────────────────────────────────────────────────────
            Self::UnknownField {
                record,
                field,
                suggestions,
                ..
            } => {
                write!(f, "unknown field `{field}` on record `{record}`")?;
                if let Some(s) = suggestions.first() {
                    write!(f, "\n  did you mean: {s}?")?;
                }
                Ok(())
            }

            // ── T006 ──────────────────────────────────────────────────────────
            Self::WithOnNonRecord { ty, .. } => {
                let ty = ty.render_in(tycons, namer);
                write!(f, "`with` on non-record\n  found type `{ty}`")
            }

            // ── T007 ──────────────────────────────────────────────────────────
            Self::PatternTypeMismatch {
                expected, pattern, ..
            } => {
                let expected = expected.render_in(tycons, namer);
                let pattern = pattern.render_in(tycons, namer);
                write!(
                    f,
                    "pattern type mismatch\n  expected `{expected}`, but pattern implies `{pattern}`"
                )
            }

            // ── T009 ──────────────────────────────────────────────────────────
            Self::WrongConstructorArity {
                ctor,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "wrong constructor arity\n  `{ctor}` expects {expected} argument{s1}, got {found}",
                    s1 = if *expected == 1 { "" } else { "s" },
                )
            }

            // ── T010 ──────────────────────────────────────────────────────────
            Self::OccursCheck { var, ty, .. } => {
                let var = var.render_in(tycons, namer);
                let ty = ty.render_in(tycons, namer);
                write!(
                    f,
                    "infinite type\n  {var} would have to contain itself: `{ty}`"
                )
            }

            // ── T011 ──────────────────────────────────────────────────────────
            Self::RecursiveTypeAlias { cycle, .. } => {
                write!(f, "recursive type alias\n  cycle: {}", cycle.join(" -> "))
            }

            // ── T013 ──────────────────────────────────────────────────────────
            Self::PolymorphicRecursion { decl, fix_hint, .. } => {
                write!(
                    f,
                    "`{decl}` is used at a second type inside its own definition\n  that is checked only against a signature that annotates every parameter and the return type\n  fix: {fix_hint}"
                )
            }

            // ── T014 (spec §5.3 exact text shape) ────────────────────────────
            //
            // Spec example:
            //   Error: function 'f' declared as `fn io` uses capability `fs`
            //     at src/Main.ridge:12
            //     ...
            //     The call to `Fs.readFile` requires `fs`.
            //     Options:
            //       - Add `fs` to the signature: `fn io fs procesarConfig`
            //       - Remove the call to `Fs.readFile`
            //
            // Display (prose portion, no source lines):
            Self::CapabilityNotDeclared {
                decl,
                kind,
                declared,
                missing,
                inferred,
                ..
            } => {
                // The `Options:` lines are meant to be pasted back into the
                // source, so the capability set is written the way a
                // declaration writes it. `Display` gives `{io}`, which reads
                // correctly in the prose above but starts a record type in the
                // language — `fn {io} main` does not parse.
                let inferred_src = inferred.as_source_caps();
                match kind {
                    CapDeclKind::Fn => write!(
                        f,
                        "capability not declared\n  function `{decl}` declared as `fn {declared}` uses capability `{missing}`\n  Options:\n    - Add `{missing}` to the signature: `fn {inferred_src} {decl}`\n    - Remove the call requiring `{missing}`"
                    ),
                    CapDeclKind::Handler => write!(
                        f,
                        "capability not declared\n  handler `{decl}` declared as `on {declared}` uses capability `{missing}`\n  Options:\n    - Add `{missing}` to the signature: `on {inferred_src} {decl}`\n    - Remove the call requiring `{missing}`"
                    ),
                    CapDeclKind::Init => write!(
                        f,
                        "capability not declared\n  the init block declared as `init {declared}` uses capability `{missing}`\n  Options:\n    - Add `{missing}` to the signature: `init {inferred_src}`\n    - Remove the call requiring `{missing}`"
                    ),
                    CapDeclKind::Terminate => write!(
                        f,
                        "capability not declared\n  the terminate callback declared as `terminate {declared}` uses capability `{missing}`\n  Options:\n    - Add `{missing}` to the signature: `terminate {inferred_src}`\n    - Remove the call requiring `{missing}`"
                    ),
                    CapDeclKind::OnDown => write!(
                        f,
                        "capability not declared\n  the onDown handler declared as `onDown {declared}` uses capability `{missing}`\n  Options:\n    - Add `{missing}` to the signature: `onDown {inferred_src}`\n    - Remove the call requiring `{missing}`"
                    ),
                    // Rule 4 compares the inner fn's own annotation against the
                    // *enclosing* effective set, so the resolution lives on the
                    // enclosing declaration, not on the inner fn.
                    CapDeclKind::InnerFn => write!(
                        f,
                        "capability not declared\n  inner function `{decl}` declares `{inferred}` but the enclosing scope provides only `{declared}`\n  Options:\n    - Add `{missing}` to the enclosing signature\n    - Remove `{missing}` from `{decl}`"
                    ),
                }
            }

            // ── T015 ──────────────────────────────────────────────────────────
            Self::UnknownActorHandler {
                actor,
                handler,
                suggestions,
                ..
            } => {
                write!(f, "unknown handler `{handler}` on actor `{actor}`")?;
                if let Some(s) = suggestions.first() {
                    write!(f, "\n  did you mean: {s}?")?;
                }
                Ok(())
            }

            // ── T016 (spec §5.4 exact text shape) ────────────────────────────
            //
            // Spec example:
            //   Error: non-exhaustive match
            //     at src/Shape.ridge:12
            //     Missing cases:
            //       Triangle _ _ _
            //
            // When total_missing > witnesses.len(), append
            //   `... and N more`
            Self::NonExhaustiveMatch {
                scrutinee_ty,
                witnesses,
                total_missing,
                ..
            } => {
                let scrutinee_ty = scrutinee_ty.render_in(tycons, namer);
                write!(
                    f,
                    "non-exhaustive match on `{scrutinee_ty}`\n  Missing cases:"
                )?;
                for w in witnesses {
                    write!(f, "\n    {w}")?;
                }
                let extra = total_missing.saturating_sub(witnesses.len());
                if extra > 0 {
                    write!(f, "\n    ... and {extra} more")?;
                }
                Ok(())
            }

            // ── T017 ──────────────────────────────────────────────────────────
            Self::RedundantPattern { arm_index, .. } => {
                write!(
                    f,
                    "redundant pattern\n  arm {} is unreachable — an earlier arm already covers this case",
                    arm_index + 1,
                )
            }

            // ── T018 ──────────────────────────────────────────────────────────
            Self::CallerCapabilityInsufficient {
                caller,
                callee,
                missing,
                ..
            } => {
                write!(
                    f,
                    "caller capability insufficient\n  `{caller}` calls `{callee}` which requires `{missing}`\n  Options:\n    - Add `{missing}` to the signature of `{caller}`\n    - Use a pure alternative to `{callee}`"
                )
            }

            // ── T019 ──────────────────────────────────────────────────────────
            Self::ActorCapabilityLeak {
                actor,
                handler,
                leaking_caps,
                ..
            } => {
                write!(
                    f,
                    "actor capability leak\n  `{handler}` on actor `{actor}` declares `{leaking_caps}`, which no member of the running actor declares"
                )
            }

            // ── T020 ──────────────────────────────────────────────────────────
            Self::SendOnNonActor { found_ty, .. } => {
                let found_ty = found_ty.render_in(tycons, namer);
                write!(
                    f,
                    "send (`!`) on non-actor\n  found type `{found_ty}`, expected an actor Handle"
                )
            }

            // ── T021 ──────────────────────────────────────────────────────────
            Self::AskOnNonActor { found_ty, .. } => {
                let found_ty = found_ty.render_in(tycons, namer);
                write!(
                    f,
                    "ask (`?>`) on non-actor\n  found type `{found_ty}`, expected an actor Handle"
                )
            }

            // ── T022 ──────────────────────────────────────────────────────────
            Self::DiscardedResult { ty, .. } => {
                let ty = ty.render_in(tycons, namer);
                write!(
                    f,
                    "discarded result\n  expression of type `{ty}` is not bound — use `let _ =` to explicitly discard"
                )
            }

            // ── T023 ──────────────────────────────────────────────────────────
            Self::UnsolvedTypeVariable { var, .. } => {
                write!(
                    f,
                    "unsolved type variable `{var}`\n  add a type annotation to resolve the ambiguity"
                )
            }

            // ── T024 ──────────────────────────────────────────────────────────
            Self::RowVariableLeak { decl, .. } => {
                write!(
                    f,
                    "capability row variable leaked in `{decl}`\n  add an explicit capability annotation to pin the row"
                )
            }

            // ── T025 ──────────────────────────────────────────────────────────
            Self::SpawnArityMismatch {
                actor,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "spawn arity mismatch\n  `{actor}` init expects {expected} argument{s1}, got {found}",
                    s1 = if *expected == 1 { "" } else { "s" },
                )
            }

            // ── T026 ──────────────────────────────────────────────────────────
            Self::AskTimeoutNotInt { found, .. } => {
                let found = found.render_in(tycons, namer);
                write!(
                    f,
                    "ask timeout must be Int\n  expected `Int`, found `{found}`\n  hint: use `?> handler() timeout 1000` (milliseconds) or `timeout never`"
                )
            }

            // ── T027 ──────────────────────────────────────────────────────────
            Self::MailboxPolicyDropOldestNotShipped { actor, .. } => {
                write!(
                    f,
                    "`drop oldest` mailbox policy is not yet implemented\n  actor `{actor}` declares `mailbox bounded N drop oldest`\n  hint: use `drop newest` (silently drop the incoming message) or `error` (signal failure to the sender) until `drop oldest` ships"
                )
            }

            // ── T028 ──────────────────────────────────────────────────────────
            Self::IncompleteRecordPattern {
                record,
                missing_fields,
                ..
            } => {
                write!(
                    f,
                    "record pattern is missing fields\n  type `{record}` has fields not covered by this pattern"
                )?;
                for field in missing_fields {
                    write!(f, "\n  missing field: `{field}`")?;
                }
                write!(
                    f,
                    "\n  hint: add the missing field bindings, or add `..` to ignore the rest"
                )
            }

            // ── T029 ──────────────────────────────────────────────────────────
            Self::NoInstance {
                class,
                ty,
                fix_hint,
                ..
            } => {
                let ty = ty.render_in(tycons, namer);
                write!(f, "no instance `{class} {ty}`\n  {fix_hint}")
            }

            // ── T056 ──────────────────────────────────────────────────────────
            Self::UnknownTypeName {
                name, suggestions, ..
            } => {
                let hint = if suggestions.is_empty() {
                    "declare it, or import the module that does".to_string()
                } else {
                    let quoted = suggestions
                        .iter()
                        .map(|s| format!("`{s}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("did you mean {quoted}?")
                };
                write!(f, "unknown type `{name}`\n  {hint}")
            }

            // ── T057 ──────────────────────────────────────────────────────────
            Self::TupleWidthMismatch {
                expected, found, ..
            } => {
                write!(
                    f,
                    "tuple width mismatch\n  {}",
                    tuple_width_sentence(*expected, *found)
                )
            }

            // ── T030 ──────────────────────────────────────────────────────────
            Self::AmbiguousConstraint { class, ty_var, .. } => {
                let ty_var = ty_var.render_in(tycons, namer);
                write!(
                    f,
                    "ambiguous constraint\n  cannot determine which instance of `{class}` to use for the type variable `{ty_var}` here\n  hint: add a type annotation to fix the type variable"
                )
            }

            // ── T031 ──────────────────────────────────────────────────────────
            Self::OrphanInstance {
                class,
                ty,
                instance_module,
                legal_modules,
                ..
            } => {
                let ty = ty.render_in(tycons, namer);
                if legal_modules.is_empty() {
                    write!(
                        f,
                        "orphan instance\n  `instance {class} {ty}` cannot be written here: `{class}` and `{ty}` are both built in, so no module in this workspace declares either of them\n  hint: wrap `{ty}` in a type of your own and write the instance for the wrapper"
                    )
                } else {
                    let homes = join_or(legal_modules);
                    write!(
                        f,
                        "orphan instance\n  `instance {class} {ty}` must be defined in {homes}; found in `{instance_module}`\n  hint: move the instance to one of those modules"
                    )
                }
            }

            // ── T032 ──────────────────────────────────────────────────────────
            Self::OverlappingInstance { class, ty, .. } => {
                let ty = ty.render_in(tycons, namer);
                write!(
                    f,
                    "overlapping instance\n  `instance {class} {ty}` is already defined; only one instance per class/type pair is allowed\n  hint: remove the duplicate instance"
                )
            }

            // ── T033 ──────────────────────────────────────────────────────────
            Self::MissingSuperclassInstance {
                class,
                ty,
                superclass,
                ..
            } => {
                let ty = ty.render_in(tycons, namer);
                write!(
                    f,
                    "missing superclass instance\n  `{class} {ty}` requires `{superclass} {ty}` but no such instance exists\n  hint: add `instance {superclass} {ty}` or add `{superclass}` to the `deriving` list"
                )
            }

            // ── T034 ──────────────────────────────────────────────────────────
            Self::ToTextConflict { ty, .. } => {
                let ty = ty.render_in(tycons, namer);
                write!(
                    f,
                    "conflicting ToText instances\n  `{ty}` already has a ToText instance auto-derived from its `pub fn toText`; remove one (either the `pub fn toText` function or the explicit `instance ToText {ty}`)"
                )
            }

            // ── T035 ──────────────────────────────────────────────────────────
            Self::SuperclassCycle { cycle, .. } => {
                write!(
                    f,
                    "superclass cycle detected\n  cycle: {}\n  hint: class hierarchies must be acyclic; remove one of the circular superclass requirements",
                    cycle.join(" -> ")
                )
            }

            // ── T036 ──────────────────────────────────────────────────────────
            Self::OpaqueFieldAccess { record, field, .. } => {
                write!(
                    f,
                    "field `{field}` of opaque type `{record}` cannot be reached outside its defining module\n  hint: call a function the module exports instead of touching the field directly"
                )
            }

            // ── T037 ──────────────────────────────────────────────────────────
            Self::RowMismatch {
                expected,
                found,
                missing_fields,
                extra_fields,
                ..
            } => {
                let expected = expected.render_in(tycons, namer);
                let found = found.render_in(tycons, namer);
                write!(
                    f,
                    "record shape mismatch\n  expected `{expected}`, got `{found}`"
                )?;
                if !extra_fields.is_empty() {
                    write!(f, "\n  unexpected field(s): {}", extra_fields.join(", "))?;
                }
                if !missing_fields.is_empty() {
                    write!(f, "\n  missing field(s): {}", missing_fields.join(", "))?;
                }
                Ok(())
            }

            // ── T038 ──────────────────────────────────────────────────────────
            Self::InstanceArityMismatch {
                class,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "wrong number of types in instance head\n  class `{class}` takes {expected} type parameter(s), but the instance head supplies {found}\n  hint: give the instance exactly {expected} type atom(s), parenthesising applied types like `(List a)`"
                )
            }

            // ── T039 ──────────────────────────────────────────────────────────
            Self::QuoteUnknownColumn {
                entity,
                column,
                suggestions,
                ..
            } => {
                write!(
                    f,
                    "`{column}` is not a column of `{entity}` in this quoted predicate"
                )?;
                if !suggestions.is_empty() {
                    write!(f, "\n  did you mean: {}", suggestions.join(", "))?;
                }
                Ok(())
            }

            // ── T040 ──────────────────────────────────────────────────────────
            Self::QuoteUnsupportedExpr { detail, .. } => {
                write!(
                    f,
                    "this is not supported inside a quoted predicate\n  {detail}\n  hint: a quoted predicate is built from column references, literals, comparisons, and `&&`/`||`"
                )
            }

            // ── T041 ──────────────────────────────────────────────────────────
            Self::QuoteComparisonMismatch { left, right, .. } => {
                let left = left.render_in(tycons, namer);
                let right = right.render_in(tycons, namer);
                write!(
                    f,
                    "the two sides of this comparison have different types\n  left is `{left}`, right is `{right}`"
                )
            }

            // ── T042 ──────────────────────────────────────────────────────────
            Self::QuoteEntityUnknown { .. } => {
                write!(
                    f,
                    "cannot tell which entity this quoted predicate is about\n  hint: annotate the predicate's parameter, e.g. `fn (u: User) -> u.age >= 18`"
                )
            }

            // ── T043 ──────────────────────────────────────────────────────────
            Self::RefutablePatternParam { witness, ty, .. } => {
                let ty = ty.render_in(tycons, namer);
                write!(
                    f,
                    "this parameter pattern does not match every value of `{ty}`\n  it would fail on `{witness}`\n  hint: a function parameter must be irrefutable; destructure in the body with `match`/`let`, or use a single-constructor pattern"
                )
            }

            // ── T044 ──────────────────────────────────────────────────────────
            Self::NotAConstructor { name, hint, .. } => {
                write!(f, "`{name}` is not a constructor\n  {hint}")
            }

            // ── T045 ──────────────────────────────────────────────────────────
            Self::UnknownFunDepVar { class, var, .. } => {
                write!(
                    f,
                    "unknown variable in functional dependency\n  `{var}` is not a type parameter of class `{class}`\n  hint: a functional dependency may only mention the class's own type parameters"
                )
            }

            // ── T046 ──────────────────────────────────────────────────────────
            Self::ConflictingFunDep {
                class, determining, ..
            } => {
                write!(
                    f,
                    "conflicting functional dependency\n  two instances of `{class}` agree on `{determining}` but determine different types, which the class's functional dependency forbids\n  hint: a determining type may map to only one determined type"
                )
            }

            // ── T047 ──────────────────────────────────────────────────────────
            Self::InsertShapeFullEntity {
                entity,
                companion,
                omitted,
                ..
            } => {
                write!(
                    f,
                    "insert expects the insert shape `{companion}`, not the full entity `{entity}`"
                )?;
                if !omitted.is_empty() {
                    let cols = omitted.join("`, `");
                    let plural = if omitted.len() == 1 { "" } else { "s" };
                    write!(
                        f,
                        "\n  `{companion}` drops the database-generated column{plural} `{cols}`; build a `{companion}` and leave {them} to the database",
                        them = if omitted.len() == 1 { "it" } else { "them" },
                    )?;
                }
                Ok(())
            }

            // ── T048 ──────────────────────────────────────────────────────────
            Self::ActorCallbackSignature {
                member,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "invalid `{member}` callback signature\n  `{member}` must declare ({expected}), but declares ({found})"
                )
            }

            // ── T049 ──────────────────────────────────────────────────────────
            Self::UnknownTypeVersion { name, ordinal, .. } => {
                write!(
                    f,
                    "no previous version known for `{name}@{ordinal}`\n  versioned references resolve against the previous build's snapshot; none records this version"
                )
            }

            // ── T050 ──────────────────────────────────────────────────────────
            Self::DuplicateMigration { name, ordinal, .. } => {
                write!(
                    f,
                    "duplicate `migrate` for `{name}@{ordinal}`\n  this version edge already has a hook"
                )
            }

            // ── T051 ──────────────────────────────────────────────────────────
            Self::UnsupportedInstanceHead { class, reason, .. } => {
                write!(f, "unsupported instance head for `{class}`\n  {reason}")
            }

            // ── T052 ──────────────────────────────────────────────────────────
            Self::ArithmeticOnNonNumeric { op, found, .. } => {
                let found = found.render_in(tycons, namer);
                if found == "Text" {
                    write!(
                        f,
                        "arithmetic on non-numeric type\n  `{op}` requires `Int` or `Float` operands, found `Text`\n  hint: use `++` to concatenate text"
                    )
                } else {
                    write!(
                        f,
                        "arithmetic on non-numeric type\n  `{op}` requires `Int` or `Float` operands, found `{found}`\n  hint: use `++` to concatenate text or lists"
                    )
                }
            }

            // ── T053 ──────────────────────────────────────────────────────────
            Self::MainHasParams { found, .. } => {
                write!(
                    f,
                    "`main` must not take parameters\n  `main` is the program entry point and is invoked with no arguments, but declares {found} parameter{s}\n  hint: declare it as `fn main () -> ...`\n  hint: to read command-line arguments, call `Cli.args ()` from `std.cli` — it needs the `env` capability, so write `fn {{env}} main` and add `\"env\"` to the manifest's `[capabilities] allow`",
                    s = if *found == 1 { "" } else { "s" }
                )
            }

            // ── T059 ──────────────────────────────────────────────────────────
            Self::MainErrorNotShowable { ty, fix_hint, .. } => {
                let ty = ty.render_in(tycons, namer);
                write!(
                    f,
                    "`main` cannot report the failure it returns\n  its error type `{ty}` has no `ToText` instance, so a run that ends in `Err` has nothing to print\n  hint: {fix_hint}"
                )
            }

            // ── T054 ──────────────────────────────────────────────────────────
            Self::FieldAccessOnNonRecord {
                ty,
                field,
                suggestion,
                ..
            } => {
                let ty = ty.render_in(tycons, namer);
                write!(
                    f,
                    "field access on non-record\n  `{ty}` has no field `{field}` — it is not a record"
                )?;
                if let Some(s) = suggestion {
                    write!(f, "\n  did you mean `{s}`?")?;
                }
                Ok(())
            }

            // ── T055 ──────────────────────────────────────────────────────────
            Self::MissingConstraint {
                decl,
                class,
                ty_var,
                fix_hint,
                ..
            } => {
                let ty_var = ty_var.render_in(tycons, namer);
                write!(
                    f,
                    "missing constraint\n  `{decl}` promises to work for every `{ty_var}`, but its body needs `{class} {ty_var}`\n  fix: {fix_hint}"
                )
            }

            // ── T058 ──────────────────────────────────────────────────────────
            Self::PropagateOutsideResultOrOption {
                found_ty, expected, ..
            } => {
                let found_ty = found_ty.render_in(tycons, namer);
                let expected = expected.render_in(tycons, namer);
                write!(
                    f,
                    "`?` used outside Result/Option context\n  found `{found_ty}`, enclosing function returns `{expected}`"
                )
            }

            // ── T999 ──────────────────────────────────────────────────────────
            Self::InternalTypeError { detail, .. } => {
                write!(
                    f,
                    "internal type error\n  {detail}\n  This is a compiler bug. Please report it."
                )
            }
        }
    }
}
// No `Display` / `std::error::Error` for `TypeError`: rendering needs the
// type-constructor table, which a `Display` cannot be handed. Nothing in the
// workspace used it as a `dyn Error`.

// ── HasErrorCode impl ─────────────────────────────────────────────────────────

impl HasErrorCode for TypeError {
    fn code(&self) -> &'static str {
        // Delegates to the existing code() method on TypeError.
        Self::code(self)
    }

    fn span(&self) -> Span {
        match self {
            Self::TypeMismatch { span, .. }
            | Self::TypeMismatchInCall { span, .. }
            | Self::ArityMismatch { span, .. }
            | Self::MissingField { span, .. }
            | Self::UnknownField { span, .. }
            | Self::WithOnNonRecord { span, .. }
            | Self::PatternTypeMismatch { span, .. }
            | Self::WrongConstructorArity { span, .. }
            | Self::OccursCheck { span, .. }
            | Self::RecursiveTypeAlias { span, .. }
            | Self::CapabilityNotDeclared { span, .. }
            | Self::UnknownActorHandler { span, .. }
            | Self::NonExhaustiveMatch { span, .. }
            | Self::RedundantPattern { span, .. }
            | Self::CallerCapabilityInsufficient { span, .. }
            | Self::ActorCapabilityLeak { span, .. }
            | Self::SendOnNonActor { span, .. }
            | Self::AskOnNonActor { span, .. }
            | Self::PropagateOutsideResultOrOption { span, .. }
            | Self::DiscardedResult { span, .. }
            | Self::RowVariableLeak { span, .. }
            | Self::SpawnArityMismatch { span, .. }
            | Self::AskTimeoutNotInt { span, .. }
            | Self::MailboxPolicyDropOldestNotShipped { span, .. }
            | Self::IncompleteRecordPattern { span, .. }
            | Self::NoInstance { span, .. }
            | Self::UnknownTypeName { span, .. }
            | Self::TupleWidthMismatch { span, .. }
            | Self::AmbiguousConstraint { span, .. }
            | Self::OrphanInstance { span, .. }
            | Self::OverlappingInstance {
                second_span: span, ..
            }
            | Self::MissingSuperclassInstance { span, .. }
            | Self::SuperclassCycle { span, .. }
            | Self::OpaqueFieldAccess { span, .. }
            | Self::RowMismatch { span, .. }
            | Self::InstanceArityMismatch { span, .. }
            | Self::QuoteUnknownColumn { span, .. }
            | Self::QuoteUnsupportedExpr { span, .. }
            | Self::QuoteComparisonMismatch { span, .. }
            | Self::QuoteEntityUnknown { span, .. }
            | Self::RefutablePatternParam { span, .. }
            | Self::NotAConstructor { span, .. }
            | Self::UnknownFunDepVar { span, .. }
            | Self::ConflictingFunDep {
                second_span: span, ..
            }
            | Self::InsertShapeFullEntity { span, .. }
            | Self::ActorCallbackSignature { span, .. }
            | Self::UnknownTypeVersion { span, .. }
            | Self::DuplicateMigration { span, .. }
            | Self::UnsupportedInstanceHead { span, .. }
            | Self::ArithmeticOnNonNumeric { span, .. }
            | Self::MainHasParams { span, .. }
            | Self::MainErrorNotShowable { span, .. }
            | Self::FieldAccessOnNonRecord { span, .. }
            | Self::MissingConstraint { span, .. }
            | Self::InternalTypeError { span, .. } => *span,

            // T034: uses `totext_span` (the explicit instance) as the primary span.
            Self::ToTextConflict { totext_span, .. } => *totext_span,

            // T013: uses `recursive_call_span` as the primary span.
            Self::PolymorphicRecursion {
                recursive_call_span,
                ..
            } => *recursive_call_span,

            // T023: uses `generalisation_site` as the primary span.
            Self::UnsolvedTypeVariable {
                generalisation_site,
                ..
            } => *generalisation_site,
        }
    }

    fn severity(&self) -> Severity {
        // T017 RedundantPattern and T022 DiscardedResult are
        // Warning-level; all other T### variants are hard errors.
        match self {
            Self::RedundantPattern { .. } | Self::DiscardedResult { .. } => Severity::Warning,
            _ => Severity::Error,
        }
    }
}

// ── emit_internal — T999 helper ──────────────────────────────────────────────

/// Emit a `T999 InternalTypeError` diagnostic (soft-error, no panic).
///
/// Pushes the error into `ctx.errors` and returns [`ridge_types::Type::Error`]
/// so downstream inference can continue without cascading failures.
///
/// For **true invariant-violation** sites where reaching the code path
/// indicates a compiler bug, use [`emit_internal_strict`] instead — it adds a
/// `debug_assert!` that panics in debug builds.
///
/// # Usage
///
/// Prefer this function over pushing [`TypeError::InternalTypeError`] directly.
///
/// ```ignore
/// let ty = emit_internal(ctx, "unexpected Expr shape in infer_expr", span);
/// ```
/// Whether to panic on T999 in debug builds.
///
/// `emit_internal` panics in debug when this flag is set.
/// Production callers that want the panic-on-T999 behaviour (for catching
/// true invariant violations) use [`emit_internal_strict`].  Scaffolding
/// stubs that deliberately emit T999 for deferred code paths use this
/// function directly — it is a no-op assert so tests can exercise the
/// error-absorption path.
#[must_use]
pub fn emit_internal(ctx: &mut InferCtx, msg: impl Into<String>, span: Span) -> ridge_types::Type {
    let detail = msg.into();
    ctx.errors
        .push(TypeError::InternalTypeError { detail, span });
    ridge_types::Type::Error
}

/// Strict variant of [`emit_internal`] that panics in debug builds.
///
/// Use this at **true invariant-violation** sites — places where reaching the
/// code path indicates a compiler bug. Scaffolding deferred-path stubs should
/// use [`emit_internal`] instead so that `cargo test` can exercise the
/// error-absorption path.
#[must_use]
pub fn emit_internal_strict(
    ctx: &mut InferCtx,
    msg: impl Into<String>,
    span: Span,
) -> ridge_types::Type {
    let detail = msg.into();
    debug_assert!(
        false,
        "T999 internal type error (invariant violation): {detail} at {span:?}",
    );
    ctx.errors
        .push(TypeError::InternalTypeError { detail, span });
    ridge_types::Type::Error
}

// ── Type rendering for hover ──────────────────────────────────────────────────

/// Render a [`ridge_types::Type`] to a human-readable string.
///
/// `tycons` is the workspace type-constructor table
/// ([`crate::TypedWorkspace::tycons`]), indexed by `TyConId.0`. Unlike the
/// internal diagnostic renderer in `exhaustiveness`, this completes the
/// function-type arm and names type variables with stable single letters, which
/// is what the language server shows on hover.
#[must_use]
pub fn render_type_with(ty: &ridge_types::Type, tycons: &[ridge_types::TyConDecl]) -> String {
    render_reserving(|namer| render_at_depth(ty, tycons, 0, namer))
}

/// Render under one shared namer, and render again with the author's own type
/// names reserved if the first pass met any.
///
/// A signature variable prints what the author wrote, and the generated letters
/// start at `a`, so the two can collide: a mismatch between the `a` of a
/// signature and a variable the body left open would read `expected a, found a`.
/// The author's spelling is the fixed one, so the letters are what has to move.
///
/// Knowing which names to avoid means knowing which ones get printed, and that
/// is what [`render_at_depth`] decides — it skips an alias body, truncates below
/// a depth bound, and flattens a join spine. A second traversal that tried to
/// predict all of it would drift from it, so the first render reports what it
/// printed and the second renders again knowing. When the type carries no
/// signature variable — every program that never writes one — the first render
/// is already the answer and the second never runs.
fn render_reserving<T>(mut render: impl FnMut(&mut VarNamer) -> T) -> T {
    let mut namer = VarNamer::default();
    let first = render(&mut namer);
    if namer.rigids.is_empty() {
        return first;
    }
    render(&mut VarNamer::reserving(namer.rigids))
}

/// Render a type variable and the type it would have to occur inside, naming
/// both in one pass.
///
/// The pair only means anything if the variable reads as the same letter on
/// both sides — `a` occurring inside `List a` is the whole message, and two
/// separate renders would each start their lettering from `a` and could hand
/// the same name to different variables.
///
/// Both arguments must already be resolved through the union-find. Lettering
/// keys on the raw variable id, so a variable that has been unified with the
/// one being named still reads as a different letter until it is resolved to
/// the same representative — which produced `` `a` would have to contain
/// itself: `List b` ``, a sentence disproved by the type printed beside it.
#[must_use]
pub fn render_occurs_pair(
    var: &ridge_types::Type,
    ty: &ridge_types::Type,
    tycons: &[ridge_types::TyConDecl],
) -> (String, String) {
    render_reserving(|namer| {
        let letter = render_at_depth(var, tycons, 0, namer);
        let inside = render_at_depth(ty, tycons, 0, namer);
        (letter, inside)
    })
}

/// Whether the reader can attach an instance to this type at all.
///
/// `def_module_raw` is `None` for built-ins and for stdlib declarations, and
/// those are exactly the types the orphan rule puts out of reach: there is no
/// declaration of theirs to hang a `deriving` clause on, and an instance written
/// anywhere else is an orphan (T031). A tuple or a function type has no
/// declaration to extend either.
#[must_use]
pub fn user_can_extend(ty: &ridge_types::Type, tycons: &[ridge_types::TyConDecl]) -> bool {
    match ty {
        ridge_types::Type::Con(id, _) => user_can_extend_tycon(*id, tycons),
        _ => false,
    }
}

/// The [`user_can_extend`] test for a bare constructor id.
///
/// The constraint solver reaches T029 holding a `TyConId` rather than a whole
/// type, so it needs this half directly.
#[must_use]
pub fn user_can_extend_tycon(id: ridge_types::TyConId, tycons: &[ridge_types::TyConDecl]) -> bool {
    tycons
        .iter()
        .find(|d| d.id == id)
        .is_some_and(|d| d.def_module_raw.is_some())
}

/// The T029 fix hint, naming both the class and the reader's own type.
///
/// The old text said "add `instance ToText T`". `T` was a literal from the
/// template rather than anything in the program, so the reader was told to add
/// an instance and never told what for. Naming the type is only half the job:
/// for a type they do not own, both of the offered fixes are refused by the
/// orphan rule, so following the advice exactly lands on a second error.
///
/// Callers with advice specific to how the constraint arose — interpolation, for
/// one — append it to this.
#[must_use]
pub fn no_instance_hint(class_name: &str, ty_name: &str, extendable: bool) -> String {
    if extendable {
        format!(
            "add `deriving ({class_name})` to `{ty_name}` where it is declared, \
             or write `instance {class_name} {ty_name}`"
        )
    } else {
        format!(
            "`{ty_name}` is declared outside your workspace: it takes no `deriving` \
             clause of yours, and an `instance {class_name} {ty_name}` here would be \
             an orphan (T031)"
        )
    }
}

/// Render an `expected`/`found` mismatch pair under one shared variable namer.
///
/// A variable that occurs in both halves is given the same letter in each, so
/// `expected Foo a` / `found Bar a` reads as the same `a`, not two unrelated
/// ones.
#[must_use]
pub fn render_type_pair_with(
    expected: &ridge_types::Type,
    found: &ridge_types::Type,
    tycons: &[ridge_types::TyConDecl],
) -> (String, String) {
    render_reserving(|namer| {
        let e = render_at_depth(expected, tycons, 0, namer);
        let f = render_at_depth(found, tycons, 0, namer);
        (e, f)
    })
}

/// Assigns readable letters to type variables in first-appearance order.
///
/// A rendered type then reads `Repo a b` regardless of the internal union-find
/// ids its variables carry. Without it the same logical type prints differently
/// from one inference run to the next — a variable that ended up with id 438
/// would show as `w16`. One namer is shared across every variable in a single
/// rendered type, and via [`render_type_pair_with`] across a related pair.
#[derive(Default)]
pub(crate) struct VarNamer {
    /// `(raw union-find id, canonical index)` in first-appearance order.
    seen: Vec<(u32, u32)>,
    next: u32,
    /// Signature variables printed so far, in the author's own spelling.
    rigids: Vec<String>,
    /// Spellings a generated letter may not take. Empty on a first pass; see
    /// [`render_reserving`].
    reserved: Vec<String>,
}

impl VarNamer {
    /// A namer whose generated letters step over the names in `reserved`.
    fn reserving(reserved: Vec<String>) -> Self {
        Self {
            reserved,
            ..Self::default()
        }
    }

    fn name(&mut self, v: u32) -> String {
        if let Some(&(_, i)) = self.seen.iter().find(|&&(raw, _)| raw == v) {
            return render_var(i);
        }
        // Step over any letter the author already spent on a signature
        // variable, so the two never print the same name.
        let mut i = self.next;
        while self.reserved.iter().any(|r| *r == render_var(i)) {
            i += 1;
        }
        self.next = i + 1;
        self.seen.push((v, i));
        render_var(i)
    }

    /// The author's own name for a signature variable, recorded so a second
    /// pass can keep the generated letters clear of it.
    fn rigid(&mut self, name: &str) -> String {
        if !self.rigids.iter().any(|r| r == name) {
            self.rigids.push(name.to_owned());
        }
        name.to_owned()
    }
}

/// Stable, readable name for a canonical variable index: `a`..`z`, then `a1`,
/// `b1`, … Callers pass the first-appearance index a [`VarNamer`] assigns, not
/// the raw union-find id, so the sequence always starts at `a`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "v % 26 is in 0..26, always fits a u8"
)]
fn render_var(v: u32) -> String {
    let letter = char::from(b'a' + (v % 26) as u8);
    if v < 26 {
        letter.to_string()
    } else {
        format!("{letter}{}", v / 26)
    }
}

/// Recognises the join-step tycons. For one join step it reports whether the
/// step makes its right (newly joined) leaf optional, whether it makes its left
/// (everything accumulated so far) side optional, and whether it is a composite
/// (a `source` plus one new table). Returns `None` for any other type.
///
/// `Join e f a` is a transparent alias for `Joined (Query e a) f a`; after
/// alias expansion the typechecker surfaces it as the composite `Joined`, so
/// there is no separate binary-base entry here.
fn join_family(name: &str) -> Option<(bool, bool, bool)> {
    Some(match name {
        "Joined" => (false, false, true),
        "LeftJoin" => (true, false, false),
        "LeftJoined" => (true, false, true),
        "RightJoin" => (false, true, false),
        "RightJoined" => (false, true, true),
        "FullJoin" => (true, true, false),
        "FullJoined" => (true, true, true),
        _ => return None,
    })
}

/// Flattens a join spine outermost-step inward, pushing each leaf table paired
/// with whether the decoded row leaves it optional. `left_optional` carries the
/// nullability an enclosing right/full step has already imposed on everything
/// beneath it. Returns `false` (and the caller falls back to the default
/// rendering) if the spine does not bottom out in a `Query e a` base — for
/// instance when the `source` is still an unresolved variable.
fn flatten_join_spine<'a>(
    ty: &'a ridge_types::Type,
    tycons: &[ridge_types::TyConDecl],
    left_optional: bool,
    out: &mut Vec<(&'a ridge_types::Type, bool)>,
) -> bool {
    use ridge_types::Type;
    if out.len() >= 16 {
        return false;
    }
    let Type::Con(id, args) = ty else {
        return false;
    };
    let Some(decl) = tycons.get(id.0 as usize) else {
        return false;
    };
    // One-leaf base: a single-table `Query e a` — push the entity type and stop.
    if decl.name == "Query" {
        if args.is_empty() {
            return false;
        }
        out.push((&args[0], left_optional));
        return true;
    }
    let Some((right_optional, source_optional, _is_composite)) = join_family(&decl.name) else {
        return false;
    };
    if args.len() != 3 {
        return false;
    }
    // Composite step [source, new table, adapter]: flatten the nested source first,
    // then push the right table.
    if !flatten_join_spine(&args[0], tycons, left_optional || source_optional, out) {
        return false;
    }
    out.push((&args[1], right_optional || left_optional));
    true
}

fn render_at_depth(
    ty: &ridge_types::Type,
    tycons: &[ridge_types::TyConDecl],
    depth: u8,
    namer: &mut VarNamer,
) -> String {
    use ridge_types::{TyConKind, Type};

    // Bound recursion so a pathological type cannot blow the hover budget.
    if depth >= 5 {
        return "…".to_owned();
    }

    match ty {
        Type::Con(id, args) => {
            let Some(decl) = tycons.get(id.0 as usize) else {
                return format!("?{}", id.0);
            };
            if decl.is_anon {
                if let TyConKind::Record(schema) = &decl.kind {
                    let fields: Vec<String> = schema
                        .record_fields()
                        .iter()
                        .map(|f| {
                            format!(
                                "{}: {}",
                                f.name,
                                render_at_depth(&f.ty, tycons, depth + 1, namer)
                            )
                        })
                        .collect();
                    return format!("{{ {} }}", fields.join(", "));
                }
            }
            // A multi-table join flattens its left-nested spine into the flat
            // list of tables it spans, so a four-table join reads
            // `Join (User, Post, Comment, Reaction) a` instead of nesting four
            // `Joined` constructors deep. Tables an outer join can leave absent
            // render as `Option <table>`. Only composites flatten; a two-table
            // binary join (`LeftJoin`/`RightJoin`/`FullJoin User Post a`) is
            // already flat and keeps its own name. Bails to default rendering
            // when the spine does not bottom out at a `Query`, so a half-built
            // type still prints.
            if matches!(join_family(&decl.name), Some((_, _, true))) && args.len() == 3 {
                let mut leaves: Vec<(&Type, bool)> = Vec::new();
                if flatten_join_spine(ty, tycons, false, &mut leaves) {
                    let tables: Vec<String> = leaves
                        .iter()
                        .map(|(leaf, optional)| {
                            let rendered = render_at_depth(leaf, tycons, depth + 1, namer);
                            match (optional, rendered.contains(' ')) {
                                (true, true) => format!("Option ({rendered})"),
                                (true, false) => format!("Option {rendered}"),
                                (false, _) => rendered,
                            }
                        })
                        .collect();
                    let adapter = render_at_depth(&args[2], tycons, depth + 1, namer);
                    return format!("Join ({}) {adapter}", tables.join(", "));
                }
            }
            if args.is_empty() {
                decl.name.clone()
            } else {
                let parts: Vec<String> = args
                    .iter()
                    .map(|a| render_at_depth(a, tycons, depth + 1, namer))
                    .collect();
                format!("{} {}", decl.name, parts.join(" "))
            }
        }
        Type::Tuple(ts) => {
            let parts: Vec<String> = ts
                .iter()
                .map(|t| render_at_depth(t, tycons, depth + 1, namer))
                .collect();
            format!("({})", parts.join(", "))
        }
        Type::Fn { params, ret, .. } => {
            let ps: Vec<String> = params
                .iter()
                .map(|p| render_at_depth(p, tycons, depth + 1, namer))
                .collect();
            format!(
                "({}) -> {}",
                ps.join(", "),
                render_at_depth(ret, tycons, depth + 1, namer)
            )
        }
        Type::Record { fields, tail } => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(label, fty)| {
                    format!(
                        "{label}: {}",
                        render_at_depth(fty, tycons, depth + 1, namer)
                    )
                })
                .collect();
            match tail {
                // Open row renders with a trailing `..`.
                ridge_types::RowTail::Open(_) if parts.is_empty() => "{ .. }".to_owned(),
                ridge_types::RowTail::Open(_) => format!("{{ {}, .. }}", parts.join(", ")),
                _ if parts.is_empty() => "{}".to_owned(),
                _ => format!("{{ {} }}", parts.join(", ")),
            }
        }
        Type::Var(v) => namer.name(v.0),
        // The author's own name for it. Falling through to the opaque arm
        // below would print `_`, turning "you promised `a`" into a sentence
        // about nothing. It goes through the namer so the generated letters can
        // be kept clear of it — see `render_reserving`.
        Type::Rigid { name, .. } => namer.rigid(name),
        Type::Alias { name, .. } => tycons
            .get(name.0 as usize)
            .map_or_else(|| format!("?{}", name.0), |d| d.name.clone()),
        Type::Error => "Error".to_owned(),
        // `Type` is #[non_exhaustive]; render any future variant opaquely.
        _ => "_".to_owned(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Capability, Span};
    use ridge_types::CapabilitySet;

    #[test]
    fn render_var_letters() {
        assert_eq!(render_var(0), "a");
        assert_eq!(render_var(1), "b");
        assert_eq!(render_var(25), "z");
        assert_eq!(render_var(26), "a1");
        assert_eq!(render_var(27), "b1");
    }

    #[test]
    fn render_tuple_of_vars() {
        use ridge_types::{TyVid, Type};
        let tup = Type::Tuple(vec![Type::Var(TyVid(0)), Type::Var(TyVid(1))]);
        assert_eq!(render_type_with(&tup, &[]), "(a, b)");
    }

    #[test]
    fn render_canonicalises_high_var_ids() {
        use ridge_types::{TyVid, Type};
        // A deep inference run leaves variables with large union-find ids
        // (438/439 would otherwise print as `w16 x16`). Canonicalisation maps
        // them back to first-appearance letters, so the same logical type
        // always reads `(a, b)`.
        let tup = Type::Tuple(vec![Type::Var(TyVid(438)), Type::Var(TyVid(439))]);
        assert_eq!(render_type_with(&tup, &[]), "(a, b)");
    }

    #[test]
    fn render_reuses_one_letter_per_variable() {
        use ridge_types::{TyVid, Type};
        // A repeated variable keeps its letter; a fresh one advances.
        let tup = Type::Tuple(vec![
            Type::Var(TyVid(50)),
            Type::Var(TyVid(50)),
            Type::Var(TyVid(7)),
        ]);
        assert_eq!(render_type_with(&tup, &[]), "(a, a, b)");
    }

    fn rigid(name: &str) -> ridge_types::Type {
        ridge_types::Type::Rigid {
            id: ridge_types::RigidId(0),
            name: name.into(),
        }
    }

    #[test]
    fn render_rigid_uses_the_authors_name() {
        // The opaque trailing arm would print `_`, and "you promised `a`" would
        // have become a sentence about nothing.
        assert_eq!(render_type_with(&rigid("a"), &[]), "a");
        assert_eq!(render_type_with(&rigid("elem"), &[]), "elem");
    }

    #[test]
    fn generated_letters_step_over_the_authors_names() {
        use ridge_types::{TyVid, Type};
        // Without this the mismatch between a signature's `a` and a variable
        // the body left open reads `expected a, found a`.
        let (e, f) = render_type_pair_with(&rigid("a"), &Type::Var(TyVid(0)), &[]);
        assert_eq!(e, "a");
        assert_eq!(f, "b");
    }

    #[test]
    fn a_variable_rendered_first_still_yields_the_authors_name() {
        use ridge_types::{TyVid, Type};
        // The reserving pass runs over the whole render rather than reserving
        // names as they are met: here the variable is named before the rigid is
        // ever seen, and it still has to give way.
        let (e, f) = render_type_pair_with(&Type::Var(TyVid(0)), &rigid("a"), &[]);
        assert_eq!(e, "b");
        assert_eq!(f, "a");
    }

    #[test]
    fn letters_skip_every_name_the_author_used() {
        use ridge_types::{RigidId, TyVid, Type};
        // Two signature variables spelled `a` and `b` push the generated letter
        // to `c`, not to `b`.
        let signature = Type::Tuple(vec![
            Type::Rigid {
                id: RigidId(0),
                name: "a".into(),
            },
            Type::Rigid {
                id: RigidId(1),
                name: "b".into(),
            },
        ]);
        let (e, f) = render_type_pair_with(&signature, &Type::Var(TyVid(9)), &[]);
        assert_eq!(e, "(a, b)");
        assert_eq!(f, "c");
    }

    #[test]
    fn render_pair_shares_variable_letters() {
        use ridge_types::{TyVid, Type};
        // A variable shared by both halves of a mismatch reads as one letter in
        // each; a half-only variable takes the next free one.
        let expected = Type::Tuple(vec![Type::Var(TyVid(100)), Type::Var(TyVid(200))]);
        let found = Type::Var(TyVid(200));
        let (e, f) = render_type_pair_with(&expected, &found, &[]);
        assert_eq!(e, "(a, b)");
        assert_eq!(f, "b");
    }

    #[test]
    fn render_depth_is_bounded() {
        use ridge_types::{TyVid, Type};
        // Nest tuples past the depth cap; the inner type collapses to `…`.
        let mut t = Type::Var(TyVid(0));
        for _ in 0..8 {
            t = Type::Tuple(vec![t]);
        }
        assert!(
            render_type_with(&t, &[]).contains('…'),
            "deeply nested type must truncate"
        );
    }

    // ── Join-spine flat rendering ─────────────────────────────────────────────

    /// A nullary tycon decl named `name` at slot `id` — all the renderer reads.
    fn tc(id: u32, name: &str) -> ridge_types::TyConDecl {
        ridge_types::TyConDecl {
            id: ridge_types::TyConId(id),
            name: name.to_owned(),
            arity: 0,
            kind: ridge_types::TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        }
    }

    /// Tycon table for the join tests: three entity types, the join families
    /// under test, a bare `Query` tycon, and an adapter — each at the slot
    /// matching its id.
    ///
    /// Slot layout:
    ///   0 = User, 1 = Post, 2 = Comment,
    ///   3 = `Query`, 4 = `Joined`, 5 = `LeftJoin`,
    ///   6 = `LeftJoined`, 7 = `RightJoined`, 8 = Mem
    fn join_tycons() -> Vec<ridge_types::TyConDecl> {
        vec![
            tc(0, "User"),
            tc(1, "Post"),
            tc(2, "Comment"),
            tc(3, "Query"),
            tc(4, "Joined"),
            tc(5, "LeftJoin"),
            tc(6, "LeftJoined"),
            tc(7, "RightJoined"),
            tc(8, "Mem"),
        ]
    }

    fn leaf(id: u32) -> ridge_types::Type {
        ridge_types::Type::Con(ridge_types::TyConId(id), vec![])
    }

    /// `Con id [a, b, c]` — the `[source, new table, adapter]` shape every
    /// composite join tycon carries.
    fn join3(
        id: u32,
        a: ridge_types::Type,
        b: ridge_types::Type,
        c: ridge_types::Type,
    ) -> ridge_types::Type {
        ridge_types::Type::Con(ridge_types::TyConId(id), vec![a, b, c])
    }

    /// `Query entity adapter` — the one-leaf base of a join spine.
    fn query(entity: ridge_types::Type, adapter: ridge_types::Type) -> ridge_types::Type {
        ridge_types::Type::Con(ridge_types::TyConId(3), vec![entity, adapter])
    }

    #[test]
    fn binary_join_keeps_its_natural_name() {
        // A two-table LeftJoin is already flat; it renders by its own name.
        let t = join3(5, leaf(0), leaf(1), leaf(8)); // LeftJoin User Post Mem
        assert_eq!(
            render_type_with(&t, &join_tycons()),
            "LeftJoin User Post Mem"
        );
    }

    #[test]
    fn inner_composite_flattens_to_table_list() {
        // Joined (Query User Mem) Post Mem — two-table inner join via composite.
        let base = query(leaf(0), leaf(8));
        let t = join3(4, base, leaf(1), leaf(8));
        assert_eq!(
            render_type_with(&t, &join_tycons()),
            "Join (User, Post) Mem"
        );
    }

    #[test]
    fn three_table_inner_composite_flattens() {
        // Joined (Joined (Query User Mem) Post Mem) Comment Mem — three tables.
        let base = query(leaf(0), leaf(8));
        let mid = join3(4, base, leaf(1), leaf(8));
        let t = join3(4, mid, leaf(2), leaf(8));
        assert_eq!(
            render_type_with(&t, &join_tycons()),
            "Join (User, Post, Comment) Mem"
        );
    }

    #[test]
    fn left_joined_leaf_renders_optional() {
        // LeftJoined (Joined (Query User Mem) Post Mem) Comment Mem
        // — the new table (Comment) may be absent.
        let base = query(leaf(0), leaf(8));
        let mid = join3(4, base, leaf(1), leaf(8));
        let t = join3(6, mid, leaf(2), leaf(8));
        assert_eq!(
            render_type_with(&t, &join_tycons()),
            "Join (User, Post, Option Comment) Mem"
        );
    }

    #[test]
    fn right_joined_makes_the_accumulated_side_optional() {
        // RightJoined (Joined (Query User Mem) Post Mem) Comment Mem
        // — the whole left side becomes optional, the newly joined table stays.
        let base = query(leaf(0), leaf(8));
        let mid = join3(4, base, leaf(1), leaf(8));
        let t = join3(7, mid, leaf(2), leaf(8));
        assert_eq!(
            render_type_with(&t, &join_tycons()),
            "Join (Option User, Option Post, Comment) Mem"
        );
    }

    fn sp() -> Span {
        Span::point(0)
    }

    // ── T001 Display ──────────────────────────────────────────────────────────

    #[test]
    fn display_t001_typemismatch() {
        let err = TypeError::TypeMismatch {
            expected: "Int".into(),
            found: "Text".into(),
            span: sp(),
            hint: None,
        };
        let s = err.render(&[]);
        assert!(s.contains("T001"), "should contain code: {s}");
        assert!(s.contains("Int"), "should contain expected type: {s}");
        assert!(s.contains("Text"), "should contain found type: {s}");
        assert!(s.contains("expected"), "should contain 'expected': {s}");
        assert!(s.contains("got"), "should contain 'got': {s}");
    }

    // ── T003 Display — the callee, and what to print without one ──────────────

    #[test]
    fn display_t003_names_the_callee() {
        let err = TypeError::ArityMismatch {
            callee: "add".into(),
            expected: 2,
            found: 3,
            span: sp(),
            hint: None,
        };
        let s = err.render(&[]);
        assert!(s.contains("`add` expects 2 arguments, got 3"), "{s}");
    }

    /// An annotation mismatch and a lambda applied in place have no name to
    /// print. The message used to open with an empty pair of backticks, which
    /// reads as a name the compiler lost rather than one that never existed.
    #[test]
    fn display_t003_without_a_callee_drops_the_backticks() {
        let err = TypeError::ArityMismatch {
            callee: String::new(),
            expected: 1,
            found: 2,
            span: sp(),
            hint: None,
        };
        let s = err.render(&[]);
        assert!(s.contains("expects 1 argument, got 2"), "{s}");
        assert!(!s.contains("``"), "empty backticks must not survive: {s}");
    }

    // ── T057 Display ──────────────────────────────────────────────────────────

    /// The wording deliberately does not mention arguments. `takesPair (1, 2, 3)`
    /// passes one argument, and calling it an arity mismatch sent the reader
    /// looking for a fourth argument nobody wrote.
    #[test]
    fn display_t057_speaks_about_components_not_arguments() {
        let err = TypeError::TupleWidthMismatch {
            expected: 2,
            found: 3,
            span: sp(),
        };
        let s = err.render(&[]);
        assert!(s.contains("T057"), "{s}");
        assert!(
            s.contains("this tuple has 3 components, but 2 are expected"),
            "{s}"
        );
        assert!(!s.contains("argument"), "not an argument count: {s}");
    }

    /// Both counts can be one, and the sentence has to survive it.
    #[test]
    fn display_t057_agrees_in_number() {
        let one = TypeError::TupleWidthMismatch {
            expected: 1,
            found: 1,
            span: sp(),
        }
        .render(&[]);
        assert!(one.contains("has 1 component, but 1 is expected"), "{one}");
    }

    // ── T014 Display — spec §5.3 exact text shape ─────────────────────────────

    /// The spec §5.3 text shape for T014:
    ///
    /// ```text
    /// T014: capability not declared
    ///   function `procesarConfig` declared as `fn {io}` uses capability `{fs}`
    ///   Options:
    ///     - Add `{fs}` to the signature: `fn {fs io} procesarConfig`
    ///     - Remove the call requiring `{fs}`
    /// ```
    #[test]
    fn display_t014_capabilitynotdeclared_matches_spec() {
        let declared = CapabilitySet::singleton(Capability::Io);
        let missing = CapabilitySet::singleton(Capability::Fs);
        let inferred = {
            let mut s = CapabilitySet::singleton(Capability::Io);
            s.insert(Capability::Fs);
            s
        };
        let err = TypeError::CapabilityNotDeclared {
            decl: "procesarConfig".into(),
            kind: CapDeclKind::Fn,
            declared,
            inferred,
            missing,
            span: sp(),
        };
        let s = err.render(&[]);
        assert!(s.contains("T014"), "code: {s}");
        assert!(s.contains("procesarConfig"), "decl name: {s}");
        assert!(s.contains("fn {io}"), "declared caps: {s}");
        assert!(s.contains("{fs}"), "missing caps: {s}");
        // The specification writes this very suggestion as
        // `fn io fs procesarConfig` — bare names, no braces, because that is
        // what a declaration accepts.
        assert!(
            s.contains("`fn io fs procesarConfig`"),
            "suggestion must be written as a declaration: {s}"
        );
        assert!(s.contains("Options:"), "options header: {s}");
        assert!(s.contains("Add"), "add option: {s}");
        assert!(s.contains("Remove"), "remove option: {s}");
    }

    /// The same error speaks in the declaration's own syntax for actor
    /// members and inner fns.
    #[test]
    fn display_t014_decl_kinds() {
        let base = |kind: CapDeclKind, decl: &str| TypeError::CapabilityNotDeclared {
            decl: decl.into(),
            kind,
            declared: CapabilitySet::PURE,
            inferred: CapabilitySet::singleton(Capability::Io),
            missing: CapabilitySet::singleton(Capability::Io),
            span: sp(),
        };

        let handler = base(CapDeclKind::Handler, "increment").render(&[]);
        assert!(
            handler.contains("handler `increment`"),
            "handler: {handler}"
        );
        // The suggestion is written the way a declaration is written. It used
        // to carry the set braces — `on {io} increment` — which is a record
        // type where a capability was meant, so the fix offered did not parse.
        assert!(handler.contains("on io increment"), "handler: {handler}");
        assert!(
            !handler.contains("on {io}"),
            "the suggestion must not carry set braces: {handler}"
        );
        // The prose above it keeps set notation, which reads correctly there.
        assert!(
            handler.contains("uses capability `{io}`"),
            "handler: {handler}"
        );

        let init = base(CapDeclKind::Init, "init").render(&[]);
        assert!(init.contains("init block"), "init: {init}");
        assert!(init.contains("`init io`"), "init: {init}");
        assert!(
            !init.contains("`init {io}`"),
            "the suggestion must not carry set braces: {init}"
        );

        let inner = base(CapDeclKind::InnerFn, "helper").render(&[]);
        assert!(inner.contains("inner function `helper`"), "inner: {inner}");
        assert!(
            inner.contains("enclosing signature"),
            "inner points at the enclosing decl: {inner}"
        );
    }

    // ── T016 Display — spec §5.4 with witnesses ───────────────────────────────

    #[test]
    fn display_t016_nonexhaustivematch_with_witnesses() {
        let err = TypeError::NonExhaustiveMatch {
            scrutinee_ty: "Shape".into(),
            witnesses: vec![
                "Circle _".into(),
                "Triangle _ _ _".into(),
                "Rectangle _ _".into(),
            ],
            total_missing: 3,
            span: sp(),
        };
        let s = err.render(&[]);
        assert!(s.contains("T016"), "code: {s}");
        assert!(s.contains("Shape"), "scrutinee type: {s}");
        assert!(s.contains("Missing cases:"), "header: {s}");
        assert!(s.contains("Circle _"), "first witness: {s}");
        assert!(s.contains("Triangle _ _ _"), "second witness: {s}");
        assert!(s.contains("Rectangle _ _"), "third witness: {s}");
        // No truncation — total_missing == witnesses.len()
        assert!(!s.contains("more"), "should not truncate: {s}");
    }

    // ── T016 Display — "and N more" suffix ───────────────────────────────────

    #[test]
    fn display_t016_nonexhaustivematch_truncated() {
        let err = TypeError::NonExhaustiveMatch {
            scrutinee_ty: "Color".into(),
            witnesses: vec!["Red".into(), "Green".into(), "Blue".into()],
            // 8 total missing, 3 shown → "and 5 more"
            total_missing: 8,
            span: sp(),
        };
        let s = err.render(&[]);
        assert!(s.contains("T016"), "code: {s}");
        assert!(s.contains("Missing cases:"), "header: {s}");
        assert!(s.contains("Red"), "first witness: {s}");
        assert!(s.contains("... and 5 more"), "truncation suffix: {s}");
    }

    // ── T015 Display — did-you-mean ───────────────────────────────────────────

    #[test]
    fn display_t015_unknownactorhandler_with_didyoumean() {
        let err = TypeError::UnknownActorHandler {
            actor: "Counter".into(),
            handler: "incremento".into(),
            suggestions: vec!["increment".into()],
            span: sp(),
        };
        let s = err.render(&[]);
        assert!(s.contains("T015"), "code: {s}");
        assert!(s.contains("incremento"), "handler name: {s}");
        assert!(s.contains("Counter"), "actor name: {s}");
        assert!(s.contains("did you mean: increment?"), "suggestion: {s}");
    }

    // ── T005 Display — did-you-mean ───────────────────────────────────────────

    #[test]
    fn display_t005_unknownfield_with_didyoumean() {
        let err = TypeError::UnknownField {
            record: "User".into(),
            field: "nme".into(),
            suggestions: vec!["name".into()],
            span: sp(),
        };
        let s = err.render(&[]);
        assert!(s.contains("T005"), "code: {s}");
        assert!(s.contains("nme"), "field name: {s}");
        assert!(s.contains("User"), "record name: {s}");
        assert!(s.contains("did you mean: name?"), "suggestion: {s}");
    }

    // ── Severity correctness ──────────────────────────────────────────────────

    #[test]
    fn severity_warnings_correct() {
        let warn_t017 = TypeError::RedundantPattern {
            arm_index: 0,
            span: sp(),
        };
        let warn_t022 = TypeError::DiscardedResult {
            ty: "Result Unit Err".into(),
            span: sp(),
        };
        let err_t001 = TypeError::TypeMismatch {
            expected: "Int".into(),
            found: "Text".into(),
            span: sp(),
            hint: None,
        };

        assert_eq!(
            <TypeError as HasErrorCode>::severity(&warn_t017),
            Severity::Warning,
            "T017 should be Warning"
        );
        assert_eq!(
            <TypeError as HasErrorCode>::severity(&warn_t022),
            Severity::Warning,
            "T022 should be Warning"
        );
        assert_eq!(
            <TypeError as HasErrorCode>::severity(&err_t001),
            Severity::Error,
            "T001 should be Error"
        );
    }

    // ── HasErrorCode compile check ────────────────────────────────────────────

    /// Verifies at the type level that `TypeError`: `HasErrorCode`.
    /// If this compiles, the trait impl is wired correctly.
    #[test]
    fn has_error_code_trait_impls_compile() {
        fn assert_has_error_code<T: HasErrorCode>(_: &T) {}
        let err = TypeError::TypeMismatch {
            expected: "Int".into(),
            found: "Text".into(),
            span: sp(),
            hint: None,
        };
        assert_has_error_code(&err);
        // Also verify the code/span/severity methods are callable
        assert_eq!(<TypeError as HasErrorCode>::code(&err), "T001");
        assert_eq!(<TypeError as HasErrorCode>::span(&err), sp());
        assert_eq!(<TypeError as HasErrorCode>::severity(&err), Severity::Error);
    }

    // ── One of each ────────────────────────────────────────────────────────────

    /// Names every variant, so a new one stops the build until it is added to
    /// the list below instead of quietly escaping every test that walks it.
    fn assert_every_variant_is_named(e: &TypeError) {
        match e {
            TypeError::TypeMismatch { .. }
            | TypeError::TypeMismatchInCall { .. }
            | TypeError::ArityMismatch { .. }
            | TypeError::MissingField { .. }
            | TypeError::UnknownField { .. }
            | TypeError::WithOnNonRecord { .. }
            | TypeError::PatternTypeMismatch { .. }
            | TypeError::WrongConstructorArity { .. }
            | TypeError::OccursCheck { .. }
            | TypeError::RecursiveTypeAlias { .. }
            | TypeError::PolymorphicRecursion { .. }
            | TypeError::CapabilityNotDeclared { .. }
            | TypeError::UnknownActorHandler { .. }
            | TypeError::NonExhaustiveMatch { .. }
            | TypeError::RedundantPattern { .. }
            | TypeError::CallerCapabilityInsufficient { .. }
            | TypeError::ActorCapabilityLeak { .. }
            | TypeError::SendOnNonActor { .. }
            | TypeError::AskOnNonActor { .. }
            | TypeError::PropagateOutsideResultOrOption { .. }
            | TypeError::DiscardedResult { .. }
            | TypeError::UnsolvedTypeVariable { .. }
            | TypeError::RowVariableLeak { .. }
            | TypeError::SpawnArityMismatch { .. }
            | TypeError::AskTimeoutNotInt { .. }
            | TypeError::MailboxPolicyDropOldestNotShipped { .. }
            | TypeError::IncompleteRecordPattern { .. }
            | TypeError::NoInstance { .. }
            | TypeError::AmbiguousConstraint { .. }
            | TypeError::OrphanInstance { .. }
            | TypeError::OverlappingInstance { .. }
            | TypeError::MissingSuperclassInstance { .. }
            | TypeError::ToTextConflict { .. }
            | TypeError::SuperclassCycle { .. }
            | TypeError::OpaqueFieldAccess { .. }
            | TypeError::RowMismatch { .. }
            | TypeError::InstanceArityMismatch { .. }
            | TypeError::QuoteUnknownColumn { .. }
            | TypeError::QuoteUnsupportedExpr { .. }
            | TypeError::QuoteComparisonMismatch { .. }
            | TypeError::QuoteEntityUnknown { .. }
            | TypeError::RefutablePatternParam { .. }
            | TypeError::NotAConstructor { .. }
            | TypeError::UnknownFunDepVar { .. }
            | TypeError::ConflictingFunDep { .. }
            | TypeError::InsertShapeFullEntity { .. }
            | TypeError::ActorCallbackSignature { .. }
            | TypeError::UnknownTypeVersion { .. }
            | TypeError::DuplicateMigration { .. }
            | TypeError::UnsupportedInstanceHead { .. }
            | TypeError::ArithmeticOnNonNumeric { .. }
            | TypeError::MainHasParams { .. }
            | TypeError::MainErrorNotShowable { .. }
            | TypeError::FieldAccessOnNonRecord { .. }
            | TypeError::MissingConstraint { .. }
            | TypeError::UnknownTypeName { .. }
            | TypeError::TupleWidthMismatch { .. }
            | TypeError::InternalTypeError { .. } => {}
        }
    }

    /// Every `TypeError` once, plus one case per branch of the arms that render
    /// more than one sentence.
    #[expect(clippy::too_many_lines, reason = "one entry per variant, on purpose")]
    fn one_of_each() -> Vec<TypeError> {
        let s = || "x".to_owned();
        let td = || TypeDesc::Text("x".to_owned());
        let caps = || CapabilitySet::singleton(Capability::Io);

        let mut all = vec![
            TypeError::TypeMismatch {
                expected: td(),
                found: td(),
                span: sp(),
                hint: None,
            },
            TypeError::TypeMismatch {
                expected: td(),
                found: td(),
                span: sp(),
                hint: Some(s()),
            },
            TypeError::TypeMismatchInCall {
                callee: s(),
                arg_index: 0,
                expected: td(),
                found: td(),
                span: sp(),
            },
            TypeError::ArityMismatch {
                callee: s(),
                expected: 2,
                found: 3,
                span: sp(),
                hint: None,
            },
            TypeError::ArityMismatch {
                callee: String::new(),
                expected: 1,
                found: 2,
                span: sp(),
                hint: Some(s()),
            },
            TypeError::MissingField {
                record: s(),
                field: s(),
                span: sp(),
            },
            TypeError::UnknownField {
                record: s(),
                field: s(),
                suggestions: vec![s()],
                span: sp(),
            },
            TypeError::UnknownField {
                record: s(),
                field: s(),
                suggestions: Vec::new(),
                span: sp(),
            },
            TypeError::WithOnNonRecord {
                ty: td(),
                span: sp(),
            },
            TypeError::PatternTypeMismatch {
                expected: td(),
                pattern: td(),
                span: sp(),
            },
            TypeError::WrongConstructorArity {
                ctor: s(),
                expected: 1,
                found: 2,
                span: sp(),
            },
            TypeError::OccursCheck {
                var: td(),
                ty: td(),
                span: sp(),
            },
            TypeError::RecursiveTypeAlias {
                cycle: vec![s(), s()],
                span: sp(),
            },
            TypeError::PolymorphicRecursion {
                decl: s(),
                fix_hint: s(),
                recursive_call_span: sp(),
            },
            TypeError::UnknownActorHandler {
                actor: s(),
                handler: s(),
                suggestions: vec![s()],
                span: sp(),
            },
            TypeError::UnknownActorHandler {
                actor: s(),
                handler: s(),
                suggestions: Vec::new(),
                span: sp(),
            },
            TypeError::NonExhaustiveMatch {
                scrutinee_ty: td(),
                witnesses: vec![s()],
                total_missing: 1,
                span: sp(),
            },
            TypeError::NonExhaustiveMatch {
                scrutinee_ty: td(),
                witnesses: vec![s(), s(), s()],
                total_missing: 9,
                span: sp(),
            },
            TypeError::RedundantPattern {
                arm_index: 1,
                span: sp(),
            },
            TypeError::CallerCapabilityInsufficient {
                caller: s(),
                callee: s(),
                missing: caps(),
                span: sp(),
            },
            TypeError::ActorCapabilityLeak {
                actor: s(),
                handler: s(),
                leaking_caps: caps(),
                span: sp(),
            },
            TypeError::SendOnNonActor {
                found_ty: td(),
                span: sp(),
            },
            TypeError::AskOnNonActor {
                found_ty: td(),
                span: sp(),
            },
            TypeError::DiscardedResult {
                ty: td(),
                span: sp(),
            },
            TypeError::UnsolvedTypeVariable {
                var: s(),
                generalisation_site: sp(),
            },
            TypeError::RowVariableLeak {
                decl: s(),
                span: sp(),
            },
            TypeError::SpawnArityMismatch {
                actor: s(),
                expected: 1,
                found: 2,
                span: sp(),
            },
            TypeError::AskTimeoutNotInt {
                found: td(),
                span: sp(),
            },
            TypeError::MailboxPolicyDropOldestNotShipped {
                actor: s(),
                span: sp(),
            },
            TypeError::IncompleteRecordPattern {
                record: s(),
                missing_fields: vec![s()],
                span: sp(),
            },
            TypeError::NoInstance {
                class: s(),
                ty: td(),
                span: sp(),
                fix_hint: s(),
            },
            TypeError::AmbiguousConstraint {
                class: s(),
                ty_var: td(),
                span: sp(),
            },
            TypeError::OrphanInstance {
                class: s(),
                ty: td(),
                instance_module: s(),
                legal_modules: vec![s()],
                span: sp(),
            },
            TypeError::OverlappingInstance {
                class: s(),
                ty: td(),
                first_span: sp(),
                second_span: sp(),
            },
            TypeError::MissingSuperclassInstance {
                class: s(),
                ty: td(),
                superclass: s(),
                span: sp(),
            },
            TypeError::ToTextConflict {
                ty: td(),
                totext_span: sp(),
                auto_promote_span: sp(),
            },
            TypeError::SuperclassCycle {
                cycle: vec![s(), s()],
                span: sp(),
            },
            TypeError::OpaqueFieldAccess {
                record: s(),
                field: s(),
                span: sp(),
            },
            TypeError::RowMismatch {
                expected: td(),
                found: td(),
                missing_fields: vec![s()],
                extra_fields: vec![s()],
                span: sp(),
            },
            TypeError::RowMismatch {
                expected: td(),
                found: td(),
                missing_fields: Vec::new(),
                extra_fields: Vec::new(),
                span: sp(),
            },
            TypeError::InstanceArityMismatch {
                class: s(),
                expected: 1,
                found: 2,
                span: sp(),
            },
            TypeError::QuoteUnknownColumn {
                entity: s(),
                column: s(),
                suggestions: vec![s()],
                span: sp(),
            },
            TypeError::QuoteUnknownColumn {
                entity: s(),
                column: s(),
                suggestions: Vec::new(),
                span: sp(),
            },
            TypeError::QuoteUnsupportedExpr {
                detail: s(),
                span: sp(),
            },
            TypeError::QuoteComparisonMismatch {
                left: td(),
                right: td(),
                span: sp(),
            },
            TypeError::QuoteEntityUnknown { span: sp() },
            TypeError::RefutablePatternParam {
                witness: s(),
                ty: td(),
                span: sp(),
            },
            TypeError::NotAConstructor {
                name: s(),
                hint: s(),
                span: sp(),
            },
            TypeError::UnknownFunDepVar {
                class: s(),
                var: s(),
                span: sp(),
            },
            TypeError::ConflictingFunDep {
                class: s(),
                determining: s(),
                first_span: sp(),
                second_span: sp(),
            },
            TypeError::InsertShapeFullEntity {
                entity: s(),
                companion: s(),
                omitted: vec![s()],
                span: sp(),
            },
            TypeError::ActorCallbackSignature {
                member: "init",
                expected: s(),
                found: s(),
                span: sp(),
            },
            TypeError::UnknownTypeVersion {
                name: s(),
                ordinal: 2,
                span: sp(),
            },
            TypeError::DuplicateMigration {
                name: s(),
                ordinal: 2,
                span: sp(),
            },
            TypeError::UnsupportedInstanceHead {
                class: s(),
                reason: s(),
                span: sp(),
            },
            // `Text` gets advice of its own: `++`, not `+`.
            TypeError::ArithmeticOnNonNumeric {
                op: "+",
                found: TypeDesc::Text("Text".to_owned()),
                span: sp(),
            },
            TypeError::ArithmeticOnNonNumeric {
                op: "+",
                found: TypeDesc::Text("Bool".to_owned()),
                span: sp(),
            },
            TypeError::MainHasParams {
                found: 1,
                span: sp(),
            },
            TypeError::MainErrorNotShowable {
                ty: td(),
                fix_hint: s(),
                decl_site: None,
                span: sp(),
            },
            TypeError::FieldAccessOnNonRecord {
                ty: td(),
                field: s(),
                suggestion: Some(s()),
                span: sp(),
            },
            TypeError::FieldAccessOnNonRecord {
                ty: td(),
                field: s(),
                suggestion: None,
                span: sp(),
            },
            TypeError::MissingConstraint {
                decl: s(),
                class: s(),
                ty_var: td(),
                fix_hint: s(),
                span: sp(),
            },
            TypeError::UnknownTypeName {
                name: s(),
                span: sp(),
                suggestions: vec![s()],
            },
            TypeError::UnknownTypeName {
                name: s(),
                span: sp(),
                suggestions: Vec::new(),
            },
            TypeError::TupleWidthMismatch {
                expected: 2,
                found: 3,
                span: sp(),
            },
            TypeError::PropagateOutsideResultOrOption {
                found_ty: td(),
                expected: td(),
                span: sp(),
            },
            TypeError::InternalTypeError {
                detail: s(),
                span: sp(),
            },
        ];

        // T014 writes six different sentences, one per declaration site.
        for kind in [
            CapDeclKind::Fn,
            CapDeclKind::Handler,
            CapDeclKind::Init,
            CapDeclKind::Terminate,
            CapDeclKind::OnDown,
            CapDeclKind::InnerFn,
        ] {
            all.push(TypeError::CapabilityNotDeclared {
                decl: s(),
                kind,
                declared: CapabilitySet::PURE,
                inferred: caps(),
                missing: caps(),
                span: sp(),
            });
        }

        for e in &all {
            assert_every_variant_is_named(e);
        }
        all
    }

    /// The list reaches every code the enum can answer with.
    #[test]
    fn one_of_each_reaches_every_code() {
        let mut seen: Vec<&'static str> = one_of_each().iter().map(TypeError::code).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 58, "codes reached: {seen:?}");
    }

    /// One code, one variant.
    ///
    /// A code is the handle its reader has on a failure — the search box, the
    /// changelog entry, `ridge explain`. Two failures behind one number make
    /// every one of those ambiguous, and the reader has no way to tell which
    /// half was meant. `T021` was shared by `AskOnNonActor` and
    /// `PropagateOutsideResultOrOption` for four releases; the second now
    /// answers `T058`.
    ///
    /// Stated against the list rather than as a count, so it keeps holding as
    /// variants are added. If a genuine one-failure-two-paths case ever turns
    /// up here — the parser has one, `P021` — this is the assertion to argue
    /// with, deliberately, rather than to discover by accident.
    #[test]
    fn no_two_variants_answer_with_the_same_code() {
        use std::collections::HashMap;
        use std::mem::discriminant;

        let all = one_of_each();
        let mut variants_per_code: HashMap<&'static str, Vec<_>> = HashMap::new();
        for e in &all {
            let claimants = variants_per_code.entry(e.code()).or_default();
            let d = discriminant(e);
            if !claimants.contains(&d) {
                claimants.push(d);
            }
        }

        let shared: Vec<&str> = variants_per_code
            .iter()
            .filter(|(_, claimants)| claimants.len() > 1)
            .map(|(code, _)| *code)
            .collect();
        assert!(shared.is_empty(), "claimed by two variants: {shared:?}");
    }

    // ── The code is written in one place ────────────────────────────────────────────

    /// A message opens with the code its variant declares.
    ///
    /// True by construction now that `Display` writes the prefix from `code()`,
    /// which is the point: this is here so the construction stays.
    #[test]
    fn every_message_opens_with_the_code_its_variant_declares() {
        for e in one_of_each() {
            let text = e.render(&[]);
            let want = format!("{}: ", e.code());
            assert!(text.starts_with(&want), "expected `{want}`, got: {text}");
        }
    }

    /// No message says a code twice.
    ///
    /// This is the assertion that can fail. Fifty-five messages used to type
    /// their own prefix, in a different file from the `code()` that decides it.
    /// One that drifted would reach the reader as two disagreeing codes on one
    /// line, because `Diagnostic::message_parts` strips only the first.
    #[test]
    fn no_message_repeats_the_code_the_frame_already_wrote() {
        for e in one_of_each() {
            let text = e.render(&[]);
            // `strip_prefix`, not `trim_start_matches`: the latter removes
            // every repetition, so `T001: T001: …` came back clean and this
            // test passed against the exact defect it exists to catch.
            let rest = text
                .strip_prefix(&format!("{}: ", e.code()))
                .unwrap_or(&text);
            let first_word = rest.split_whitespace().next().unwrap_or_default();
            assert!(
                !looks_like_a_code(first_word),
                "`{}` writes a code into its own message: {text}",
                e.code()
            );
        }
    }

    /// `T123` or `T123:` — what a reader takes for an error code.
    fn looks_like_a_code(word: &str) -> bool {
        let w = word.trim_end_matches(':');
        w.len() == 4 && w.starts_with('T') && w[1..].bytes().all(|b| b.is_ascii_digit())
    }
}
