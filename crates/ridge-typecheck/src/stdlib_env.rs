//! Stdlib environment seeding (T17 pipeline wiring).
//!
//! Seeds the [`InferCtx`] environment with:
//! 1. Prelude constructor schemes (`Some`, `None`, `Ok`, `Err`).
//! 2. Stdlib qualified-name bindings from the module's resolved imports.
//!    e.g. `import std.io as Io` → `"Io.println"` → scheme, `"Io.eprintln"` → scheme, …
//!
//! This is the "stdlib wiring" that makes `Expr::Qualified("Io.println")` resolve
//! to its correct type scheme during T6 inference, instead of falling through to
//! the T999 "qualified name unresolved" fallback.

use ridge_resolve::{Binding, BuiltinStdlibModule, ImportResolution, ImportTarget, BUILTINS};
use ridge_types::{BuiltinTyCons, TyConId};
use rustc_hash::FxHashMap;

use crate::class_env::ClassTable;
use crate::ctx::InferCtx;
use crate::prelude::prelude_types;
use crate::stdlib_signatures::stdlib_signature;
use crate::stdlib_types::{reconciled_ctor_scheme, reconciled_fn_scheme};

// ── Public API ────────────────────────────────────────────────────────────────

/// Seed `ctx.env` (innermost frame) with:
/// 1. Prelude value bindings (`Some`, `None`, `Ok`, `Err`).
/// 2. Qualified stdlib bindings from `imports` (e.g. `"Io.println"` → Scheme).
/// 3. Bare stdlib bindings from `import std.text (split, trim, lines)` style.
///
/// `reconciled` maps reconciled stdlib type names to their reserved-block ids,
/// so a constructor imported from such a type (`import std.m (MkT)`) that has no
/// hand-curated stdlib signature gets its scheme derived from the arena decl.
///
/// Must be called after `ctx.env.push_frame()` and before `typecheck_module_decls`.
#[expect(
    clippy::implicit_hasher,
    reason = "callers always pass the workspace's FxHashMap; generalising over the hasher adds noise for no caller benefit"
)]
pub fn seed_stdlib_env(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    imports: &[ImportResolution],
    reconciled: &FxHashMap<String, TyConId>,
    classes: Option<&ClassTable>,
) {
    // 1. Prelude constructor schemes.
    let (prelude_values, _) = prelude_types(b);
    for (name, scheme) in prelude_values {
        ctx.env.bind(name, scheme);
    }

    // 2. Per-import qualified + bare bindings.
    for import in imports {
        if let ImportTarget::BuiltinStdlib(stdlib_id) = &import.target {
            let Some(module) = BUILTINS.get(stdlib_id.0 as usize) else {
                continue;
            };

            // Determine the local alias (e.g. `Io`, `Fs`, `List`).
            let alias: Option<&str> = import.alias.as_deref();

            // Walk all effective_bindings from the import resolution.
            // These include both `ModuleAlias` and `StdlibSymbol` bindings.
            for eb in &import.effective_bindings {
                match &eb.binding {
                    // `import std.io as Io` → local_name = "Io", binding = ModuleAlias
                    // Bind every export as "Io.<name>".
                    //
                    // IMPORTANT: use `mid` (the binding's own target) to look up the
                    // correct module, NOT `module` (which is derived from `import.target`
                    // and differs for synthetic prelude-alias entries where
                    // `import.target = StdlibModuleId(0)` but the alias targets other ids).
                    Binding::ModuleAlias {
                        target: ImportTarget::BuiltinStdlib(mid),
                        ..
                    } => {
                        let local = eb.local_name.as_str();
                        if let Some(alias_module) = BUILTINS.get(mid.0 as usize) {
                            bind_module_qualified(
                                ctx,
                                b,
                                alias_module,
                                local,
                                *mid,
                                reconciled,
                                classes,
                            );
                        }
                    }
                    // `import std.text (split, trim)` → local_name = "split", binding = StdlibSymbol
                    Binding::StdlibSymbol { module: mid, name } => {
                        // Record the compiler-known `std.actor.tryAsk`
                        // under its bare local name so the Call inference arm
                        // can recognise it (see `ctx.tryask_names`).
                        if name == "tryAsk"
                            && BUILTINS
                                .get(mid.0 as usize)
                                .is_some_and(|m| m.name == "std.actor")
                        {
                            ctx.tryask_names.insert(eb.local_name.clone());
                        }
                        if let Some(scheme) = stdlib_signature(*mid, name, b) {
                            ctx.env.bind(eb.local_name.clone(), scheme);
                        } else {
                            // No hand-curated signature: either a constructor of
                            // a reconciled stdlib type, or a stdlib function whose
                            // signature references one. Both are derived from the
                            // reconciled arena block; the function lookup is keyed
                            // on the declaring module so same-named verbs in
                            // different modules (e.g. std.repo / std.data `all`)
                            // resolve to the right scheme.
                            let module_name = BUILTINS.get(mid.0 as usize).map(|m| m.name);
                            let recon = reconciled_ctor_scheme(&ctx.tycon_decls, reconciled, name)
                                .or_else(|| {
                                    module_name.and_then(|m| {
                                        reconciled_fn_scheme(m, name, reconciled, b, classes)
                                    })
                                });
                            if let Some(scheme) = recon {
                                ctx.env.bind(eb.local_name.clone(), scheme);
                            }
                        }
                    }
                    _ => {}
                }
            }

            // If the import has an alias and effective_bindings are empty (e.g.
            // the whole-module import case without explicit items), fall back to
            // binding all exports under the alias.
            let has_module_alias_binding = import
                .effective_bindings
                .iter()
                .any(|eb| matches!(&eb.binding, Binding::ModuleAlias { .. }));

            if let Some(local_alias) = alias {
                if !has_module_alias_binding {
                    bind_module_qualified(
                        ctx,
                        b,
                        module,
                        local_alias,
                        *stdlib_id,
                        reconciled,
                        classes,
                    );
                }
            }
        } else {
            // WorkspaceModule or External — user-module bindings are handled
            // differently (symbol table lookup, not stdlib_signatures).
            // For 0.1.0, we only seed stdlib bindings here.
            // User-module cross-import type resolution is deferred to T17+.
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Bind all exports of `module` under the qualified prefix `local_alias`.
///
/// For each export name `n` in `module.exports`, binds `"<local_alias>.<n>"` to
/// its scheme. The scheme comes from the hand-curated `stdlib_signature` table,
/// or — for an export with no entry there — from the reconciled arena block: a
/// constructor of a reconciled `pub type`, or a function whose signature names
/// one (e.g. `std.query.orderSql`, typed over `SortOrder`). Without this
/// fallback a qualified `Query.orderSql` would not be in the env and would hit
/// the T999 "qualified name unresolved" path even though it resolves bare.
fn bind_module_qualified(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    module: &BuiltinStdlibModule,
    local_alias: &str,
    stdlib_id: ridge_resolve::StdlibModuleId,
    reconciled: &FxHashMap<String, TyConId>,
    classes: Option<&ClassTable>,
) {
    for &export_name in module.exports {
        let scheme = stdlib_signature(stdlib_id, export_name, b).or_else(|| {
            reconciled_ctor_scheme(&ctx.tycon_decls, reconciled, export_name)
                .or_else(|| reconciled_fn_scheme(module.name, export_name, reconciled, b, classes))
        });
        if let Some(scheme) = scheme {
            ctx.env.bind(format!("{local_alias}.{export_name}"), scheme);
        }
        // Record the compiler-known `std.actor.tryAsk` under its
        // alias-qualified local name (see `ctx.tryask_names`).
        if export_name == "tryAsk" && module.name == "std.actor" {
            ctx.tryask_names
                .insert(format!("{local_alias}.{export_name}"));
        }
    }
}
