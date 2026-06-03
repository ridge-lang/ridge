//! Inline synthesis of prelude `Encode`/`Decode` instance dictionaries.
//!
//! Deriving on a *concrete* container (`List Text`, `Option Int`) recurses
//! structurally at compile time and never touches a runtime dictionary. The
//! parametric prelude instances (`instance Encode (List a) where Encode a`)
//! are the complement: they dispatch through a runtime element dictionary so
//! that a value whose element type is only known at runtime — a `List a` with
//! `a` flowing in through a constrained call — can still encode.
//!
//! Two facts make these dictionaries special:
//!
//! 1. The prelude primitive instances (`Encode Int`, `Decode Text`, …) are
//!    registered in the instance environment but have **no runtime value**:
//!    the deriving path inlines `JInt`/`JText`/… directly and never builds a
//!    `$inst_Encode_Int` map. A parametric instance applied to a primitive
//!    element (`List Int`) needs that element dictionary as a real value.
//! 2. The parametric container instances themselves are registered in Rust
//!    (`register_prelude_instances`) with no source body, so there is no
//!    `$inst_Encode_List` constant to reference either.
//!
//! Both gaps are closed the same way: the dictionary map is **synthesised
//! inline at the use site**. When the constraint solver resolves a constraint
//! to one of these prelude-reserved instances, `dict_plan_to_expr` (in
//! `core.rs`) asks this module to build the dictionary expression directly
//! rather than emitting a symbol reference to a constant that does not exist.
//!
//! The synthesised dictionary is a plain BEAM map `#{'encode' => fun …}` —
//! structurally identical to the dictionaries `lower_instance` emits for
//! hand-written instances, so the rest of the pipeline (method projection via
//! `maps:get`, application per element) is unchanged.

use ridge_ast::Span;
use ridge_ir::{IrExpr, IrLit, IrParam, SymbolRef};
use ridge_types::{ClassId, TyConId, Type, DECODE_CLASS, ENCODE_CLASS};

use crate::ctx::LowerCtx;

// TyConId assignments from `ridge_types::BuiltinTyCons::allocate`.
const TYCON_INT: u32 = 0;
const TYCON_FLOAT: u32 = 1;
const TYCON_BOOL: u32 = 2;
const TYCON_TEXT: u32 = 3;
const TYCON_LIST: u32 = 6;
const TYCON_MAP: u32 = 7;
const TYCON_OPTION: u32 = 9;
const TYCON_RESULT: u32 = 10;
const TYCON_ERROR: u32 = 12;

/// True if `(class, tycon)` is a prelude-reserved `Encode`/`Decode` instance
/// whose dictionary has no module-level constant and must be synthesised.
///
/// Covers the four JSON primitives (`Int`/`Float`/`Bool`/`Text`) and the four
/// parametric containers (`List`/`Option`/`Map`/`Result`). Every other
/// instance — including user-written ones and derived user types — keeps the
/// existing `$inst_` symbol path.
#[must_use]
pub fn is_prelude_codec_instance(class: ClassId, tycon: TyConId) -> bool {
    if class != ENCODE_CLASS && class != DECODE_CLASS {
        return false;
    }
    matches!(
        tycon.0,
        TYCON_INT
            | TYCON_FLOAT
            | TYCON_BOOL
            | TYCON_TEXT
            | TYCON_LIST
            | TYCON_OPTION
            | TYCON_MAP
            | TYCON_RESULT
    )
}

/// Synthesise the runtime dictionary expression for a prelude `Encode`/`Decode`
/// instance.
///
/// `sub_dicts` carries the already-synthesised element dictionaries for the
/// parametric containers, in `head_var_positions` order: `[elem]` for
/// `List`/`Option`, `[value]` for `Map Text a`, `[ok, err]` for `Result a e`.
/// It is empty for the primitive instances.
///
/// Returns `None` when `(class, tycon)` is not a prelude-reserved codec
/// instance; the caller then falls back to the `$inst_` symbol path.
#[must_use]
pub fn synth_prelude_dict(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    tycon: TyConId,
    sub_dicts: Vec<IrExpr>,
    span: Span,
) -> Option<IrExpr> {
    if !is_prelude_codec_instance(class, tycon) {
        return None;
    }
    let is_encode = class == ENCODE_CLASS;
    let method = if is_encode { "encode" } else { "decode" };

    let method_fn = match tycon.0 {
        TYCON_INT | TYCON_FLOAT | TYCON_BOOL | TYCON_TEXT => {
            if is_encode {
                encode_prim_lambda(ctx, tycon, span)
            } else {
                decode_prim_lambda(ctx, tycon, span)
            }
        }
        TYCON_LIST => {
            let elem = sub_dicts
                .into_iter()
                .next()
                .unwrap_or_else(|| unit(ctx, span));
            if is_encode {
                encode_list_lambda(ctx, elem, span)
            } else {
                decode_list_lambda(ctx, elem, span)
            }
        }
        TYCON_OPTION => {
            let elem = sub_dicts
                .into_iter()
                .next()
                .unwrap_or_else(|| unit(ctx, span));
            if is_encode {
                encode_option_lambda(ctx, elem, span)
            } else {
                decode_option_lambda(ctx, elem, span)
            }
        }
        TYCON_MAP => {
            // Map Text a — the value dictionary sits at head position 1, which
            // is the sole entry in `sub_dicts` (the Text key carries no dict).
            let val = sub_dicts
                .into_iter()
                .next()
                .unwrap_or_else(|| unit(ctx, span));
            if is_encode {
                encode_map_lambda(ctx, val, span)
            } else {
                decode_map_lambda(ctx, val, span)
            }
        }
        TYCON_RESULT => {
            let mut it = sub_dicts.into_iter();
            let ok = it.next().unwrap_or_else(|| unit(ctx, span));
            let err = it.next().unwrap_or_else(|| unit(ctx, span));
            if is_encode {
                encode_result_lambda(ctx, ok, err, span)
            } else {
                decode_result_lambda(ctx, ok, err, span)
            }
        }
        _ => return None,
    };

    Some(dict_map(ctx, method, method_fn, span))
}

// ── Shared IR helpers ─────────────────────────────────────────────────────────

/// `#{ 'method' => method_fn }` — a one-entry dictionary map.
fn dict_map(ctx: &mut LowerCtx<'_>, method: &str, method_fn: IrExpr, span: Span) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: ridge_ir::CtorKind::Record,
            owner_type: TyConId(0), // untyped — dicts are plain maps in the IR
            name: format!("$synth_dict_{method}"),
            variant: 0,
        },
        fields: vec![(method.to_string(), method_fn)],
        span,
    }
}

fn unit(ctx: &mut LowerCtx<'_>, span: Span) -> IrExpr {
    IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Unit,
        span,
    }
}

fn local(ctx: &mut LowerCtx<'_>, name: &str, span: Span) -> IrExpr {
    IrExpr::Local {
        id: ctx.fresh_id(None),
        name: name.to_string(),
        span,
    }
}

fn prelude_call(ctx: &mut LowerCtx<'_>, name: &str, args: Vec<IrExpr>, span: Span) -> IrExpr {
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Prelude {
                name: name.to_string(),
            },
            span,
        }),
        args,
        span,
    }
}

fn prelude_ctor(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    fields: Vec<(String, IrExpr)>,
    span: Span,
) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: name.to_string(),
        },
        fields,
        span,
    }
}

fn stdlib_call(
    ctx: &mut LowerCtx<'_>,
    module: &str,
    name: &str,
    args: Vec<IrExpr>,
    span: Span,
) -> IrExpr {
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: module.to_string(),
                name: name.to_string(),
            },
            span,
        }),
        args,
        span,
    }
}

fn text_lit(ctx: &mut LowerCtx<'_>, s: &str, span: Span) -> IrExpr {
    IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Text(s.to_string()),
        span,
    }
}

const fn param(name: String, span: Span) -> IrParam {
    IrParam {
        name,
        ty: Type::Error,
        span,
    }
}

fn lambda(ctx: &mut LowerCtx<'_>, params: Vec<IrParam>, body: IrExpr, span: Span) -> IrExpr {
    IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params,
        body: Box::new(body),
        caps: ridge_types::CapabilitySet::PURE,
        span,
    }
}

/// `(maps:get('method', dict))(arg)` — project a method from a runtime
/// dictionary and apply it to one argument.
fn apply_dict_method(
    ctx: &mut LowerCtx<'_>,
    dict: IrExpr,
    method: &str,
    arg: IrExpr,
    span: Span,
) -> IrExpr {
    let projected = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dict),
        field: method.to_string(),
        span,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(projected),
        args: vec![arg],
        span,
    }
}

// ── Encode method bodies ──────────────────────────────────────────────────────

/// `fun(X) -> JInt(X) end` (or JFloat/JBool/JText by primitive).
fn encode_prim_lambda(ctx: &mut LowerCtx<'_>, tycon: TyConId, span: Span) -> IrExpr {
    let ctor = match tycon.0 {
        TYCON_INT => "JInt",
        TYCON_FLOAT => "JFloat",
        TYCON_BOOL => "JBool",
        _ => "JText",
    };
    let x = local(ctx, "__enc_x", span);
    let body = prelude_call(ctx, ctor, vec![x], span);
    lambda(ctx, vec![param("__enc_x".to_string(), span)], body, span)
}

/// `fun(Xs) -> JList(std.list.map(\e -> (elem.encode)(e), Xs)) end`.
fn encode_list_lambda(ctx: &mut LowerCtx<'_>, elem_dict: IrExpr, span: Span) -> IrExpr {
    let e = local(ctx, "__enc_e", span);
    let mapped_elem = apply_dict_method(ctx, elem_dict, "encode", e, span);
    let map_fn = lambda(
        ctx,
        vec![param("__enc_e".to_string(), span)],
        mapped_elem,
        span,
    );
    let xs = local(ctx, "__enc_xs", span);
    let mapped = stdlib_call(ctx, "std.list", "map", vec![map_fn, xs], span);
    let jlist = prelude_call(ctx, "JList", vec![mapped], span);
    lambda(ctx, vec![param("__enc_xs".to_string(), span)], jlist, span)
}

/// `fun(O) -> match O { Some x -> (elem.encode)(x); None -> JNull } end`.
fn encode_option_lambda(ctx: &mut LowerCtx<'_>, elem_dict: IrExpr, span: Span) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};
    let x = local(ctx, "__enc_ov", span);
    let some_body = apply_dict_method(ctx, elem_dict, "encode", x, span);
    let some_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Some".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__enc_ov".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: some_body,
        span,
    };
    let jnull = prelude_call(ctx, "JNull", vec![], span);
    let none_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "None".to_string(),
            },
            fields: vec![],
            args: vec![],
            span,
        },
        when: None,
        body: jnull,
        span,
    };
    let o = local(ctx, "__enc_o", span);
    let body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(o),
        arms: vec![some_arm, none_arm],
        span,
    };
    lambda(ctx, vec![param("__enc_o".to_string(), span)], body, span)
}

/// `fun(M) -> JObject(std.map.map(\_k v -> (val.encode)(v), M)) end`.
fn encode_map_lambda(ctx: &mut LowerCtx<'_>, val_dict: IrExpr, span: Span) -> IrExpr {
    let v = local(ctx, "__enc_mv", span);
    let mapped_val = apply_dict_method(ctx, val_dict, "encode", v, span);
    let map_fn = lambda(
        ctx,
        vec![
            param("__enc_mk".to_string(), span),
            param("__enc_mv".to_string(), span),
        ],
        mapped_val,
        span,
    );
    let m = local(ctx, "__enc_m", span);
    let mapped = stdlib_call(ctx, "std.map", "map", vec![map_fn, m], span);
    let jobject = prelude_call(ctx, "JObject", vec![mapped], span);
    lambda(ctx, vec![param("__enc_m".to_string(), span)], jobject, span)
}

/// `fun(R) -> match R { Ok x -> tagged("Ok", (ok.encode)(x)); Err e -> tagged("Err", (err.encode)(e)) } end`.
fn encode_result_lambda(
    ctx: &mut LowerCtx<'_>,
    ok_dict: IrExpr,
    err_dict: IrExpr,
    span: Span,
) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};
    let xo = local(ctx, "__enc_rok", span);
    let ok_encoded = apply_dict_method(ctx, ok_dict, "encode", xo, span);
    let ok_body = result_variant_object(ctx, "Ok", ok_encoded, span);
    let ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__enc_rok".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: ok_body,
        span,
    };
    let xe = local(ctx, "__enc_rerr", span);
    let err_encoded = apply_dict_method(ctx, err_dict, "encode", xe, span);
    let err_body = result_variant_object(ctx, "Err", err_encoded, span);
    let err_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__enc_rerr".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: err_body,
        span,
    };
    let r = local(ctx, "__enc_r", span);
    let body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(r),
        arms: vec![ok_arm, err_arm],
        span,
    };
    lambda(ctx, vec![param("__enc_r".to_string(), span)], body, span)
}

/// `JObject(fromList([("tag", JText tag), ("values", JList [payload])]))`.
fn result_variant_object(
    ctx: &mut LowerCtx<'_>,
    tag: &str,
    encoded_payload: IrExpr,
    span: Span,
) -> IrExpr {
    let tag_text = text_lit(ctx, "tag", span);
    let tag_val = {
        let t = text_lit(ctx, tag, span);
        prelude_call(ctx, "JText", vec![t], span)
    };
    let tag_pair = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![tag_text, tag_val],
        span,
    };
    let values_text = text_lit(ctx, "values", span);
    let values_list = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: vec![encoded_payload],
        span,
    };
    let values_val = prelude_call(ctx, "JList", vec![values_list], span);
    let values_pair = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![values_text, values_val],
        span,
    };
    let pairs = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: vec![tag_pair, values_pair],
        span,
    };
    let map = stdlib_call(ctx, "std.map", "fromList", vec![pairs], span);
    prelude_call(ctx, "JObject", vec![map], span)
}

// ── Decode method bodies ──────────────────────────────────────────────────────

/// `fun(J) -> match J { JInt n -> Ok n; _ -> Err(decode.expected_int) } end`.
fn decode_prim_lambda(ctx: &mut LowerCtx<'_>, tycon: TyConId, span: Span) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};
    let (ctor, code, kind) = match tycon.0 {
        TYCON_INT => ("JInt", "decode.expected_int", "JInt"),
        TYCON_FLOAT => ("JFloat", "decode.expected_float", "JFloat"),
        TYCON_BOOL => ("JBool", "decode.expected_bool", "JBool"),
        _ => ("JText", "decode.expected_string", "JText"),
    };
    let n = local(ctx, "__dec_pv", span);
    let ok_n = ok(ctx, n, span);
    let ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: ctor.to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_pv".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: ok_n,
        span,
    };
    let err = decode_error(ctx, code, &format!("expected a JSON {kind} value"), span);
    let wild_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: err,
        span,
    };
    let j = local(ctx, "__dec_j", span);
    let body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(j),
        arms: vec![ok_arm, wild_arm],
        span,
    };
    lambda(ctx, vec![param("__dec_j".to_string(), span)], body, span)
}

/// `fun(J) -> match J { JList xs -> <fold-decode each via elem.decode>; _ -> Err } end`.
fn decode_list_lambda(ctx: &mut LowerCtx<'_>, elem_dict: IrExpr, span: Span) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};
    let xs = local(ctx, "__dec_xs", span);
    let fold = decode_list_fold(ctx, elem_dict, xs, span);
    let ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "JList".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_xs".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: fold,
        span,
    };
    let err = decode_error(ctx, "decode.expected_array", "expected a JSON array", span);
    let wild_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: err,
        span,
    };
    let j = local(ctx, "__dec_j", span);
    let body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(j),
        arms: vec![ok_arm, wild_arm],
        span,
    };
    lambda(ctx, vec![param("__dec_j".to_string(), span)], body, span)
}

/// Fold a `List JsonValue` into `Result (List T) Error`, decoding each element
/// through `elem.decode` and short-circuiting the accumulator on the first
/// `Err`. The accumulator threads the error (no `Return` inside the fold
/// lambda), mirroring the derived-decode fold.
#[expect(
    clippy::too_many_lines,
    reason = "flat accumulator-Result-fold IR construction; splitting would not reduce complexity"
)]
fn decode_list_fold(ctx: &mut LowerCtx<'_>, elem_dict: IrExpr, xs: IrExpr, span: Span) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};

    // \acc elem -> match acc { Err _ -> acc; Ok done -> match (elem_dict.decode)(elem) { Ok v -> Ok [v | done]; Err e -> Err e } }
    // ridge_rt:list_fold calls F(Acc, Elem) — acc first, elem second.
    let acc = local(ctx, "__dec_acc", span);
    let elem = local(ctx, "__dec_el", span);
    let decoded = apply_dict_method(ctx, elem_dict, "decode", elem, span);

    // Ok done arm — prepend the decoded value: [v | done]
    let cons = IrExpr::Cons {
        id: ctx.fresh_id(None),
        head: Box::new(local(ctx, "__dec_v", span)),
        tail: Box::new(local(ctx, "__dec_done", span)),
        span,
    };
    let ok_cons = ok(ctx, cons, span);
    let inner_ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_v".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: ok_cons,
        span,
    };
    let e = local(ctx, "__dec_e", span);
    let err_e = err_wrap(ctx, e, span);
    let inner_err_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_e".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: err_e,
        span,
    };
    let inner_match = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(decoded),
        arms: vec![inner_ok_arm, inner_err_arm],
        span,
    };
    let acc_ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_done".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: inner_match,
        span,
    };
    // Err _ -> acc (pass through)
    let acc_passthrough = local(ctx, "__dec_acc", span);
    let acc_err_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: acc_passthrough,
        span,
    };
    let fold_body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(acc),
        arms: vec![acc_ok_arm, acc_err_arm],
        span,
    };
    let fold_fn = lambda(
        ctx,
        vec![
            param("__dec_acc".to_string(), span),
            param("__dec_el".to_string(), span),
        ],
        fold_body,
        span,
    );

    // seed = Ok []
    let empty = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: vec![],
        span,
    };
    let seed = ok(ctx, empty, span);
    // std.list.fold(fold_fn, seed, xs) -> Result (reversed List) Error
    let folded = stdlib_call(ctx, "std.list", "fold", vec![fold_fn, seed, xs], span);

    // The fold prepends, so reverse the accumulated list inside the Ok.
    // match folded { Ok acc -> Ok(std.list.reverse(acc)); Err e -> Err e }
    let acc2 = local(ctx, "__dec_facc", span);
    let reversed = stdlib_call(ctx, "std.list", "reverse", vec![acc2], span);
    let ok_rev = ok(ctx, reversed, span);
    let final_ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_facc".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: ok_rev,
        span,
    };
    let fe = local(ctx, "__dec_fe", span);
    let final_err = err_wrap(ctx, fe, span);
    let final_err_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_fe".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: final_err,
        span,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(folded),
        arms: vec![final_ok_arm, final_err_arm],
        span,
    }
}

/// `fun(J) -> match J { JNull -> Ok None; jv -> match (elem.decode)(jv) { Ok v -> Ok (Some v); Err e -> Err e } } end`.
fn decode_option_lambda(ctx: &mut LowerCtx<'_>, elem_dict: IrExpr, span: Span) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};
    let none_val = prelude_ctor(ctx, "None", vec![], span);
    let ok_none = ok(ctx, none_val, span);
    let null_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "JNull".to_string(),
            },
            fields: vec![],
            args: vec![],
            span,
        },
        when: None,
        body: ok_none,
        span,
    };
    let jv = local(ctx, "__dec_ojv", span);
    let decoded = apply_dict_method(ctx, elem_dict, "decode", jv, span);
    let v = local(ctx, "__dec_ov", span);
    let some_val = prelude_ctor(ctx, "Some", vec![("$0".to_string(), v)], span);
    let ok_some = ok(ctx, some_val, span);
    let cont = seq_ok(ctx, decoded, "__dec_ov", ok_some, span);
    let wild_arm = IrArm {
        pat: IrPat::Bind {
            name: "__dec_ojv".to_string(),
            inner: None,
            span,
        },
        when: None,
        body: cont,
        span,
    };
    let j = local(ctx, "__dec_j", span);
    let body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(j),
        arms: vec![null_arm, wild_arm],
        span,
    };
    lambda(ctx, vec![param("__dec_j".to_string(), span)], body, span)
}

/// `fun(J) -> match J { JObject m -> <fold-decode values via val.decode>; _ -> Err } end`.
fn decode_map_lambda(ctx: &mut LowerCtx<'_>, val_dict: IrExpr, span: Span) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};
    let m = local(ctx, "__dec_m", span);
    let fold = decode_map_fold(ctx, val_dict, m, span);
    let ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "JObject".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_m".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: fold,
        span,
    };
    let err = decode_error(
        ctx,
        "decode.expected_object",
        "expected a JSON object",
        span,
    );
    let wild_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: err,
        span,
    };
    let j = local(ctx, "__dec_j", span);
    let body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(j),
        arms: vec![ok_arm, wild_arm],
        span,
    };
    lambda(ctx, vec![param("__dec_j".to_string(), span)], body, span)
}

/// Fold a `Map Text JsonValue` into `Result (Map Text T) Error` by decoding each
/// value via `val.decode`. Threads errors through a `Result (List (Text, T))`
/// accumulator over `std.map.toList`, then rebuilds via `std.map.fromList`.
#[expect(
    clippy::too_many_lines,
    reason = "flat accumulator-Result-fold IR construction; splitting would not reduce complexity"
)]
fn decode_map_fold(ctx: &mut LowerCtx<'_>, val_dict: IrExpr, m: IrExpr, span: Span) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};

    // pairs = std.map.toList m  : List (Text, JsonValue)
    let pairs = stdlib_call(ctx, "std.map", "toList", vec![m], span);

    // \acc kv -> match acc { Err _ -> acc; Ok done -> let (k, jv) = kv in match (val.decode)(jv) { Ok v -> Ok [(k, v) | done]; Err e -> Err e } }
    // ridge_rt:list_fold calls F(Acc, Elem) — acc first, kv second.
    let acc = local(ctx, "__dec_macc", span);

    // destructure kv into k, jv via a Tuple pattern in an outer match arm.
    let jv = local(ctx, "__dec_mjv", span);
    let decoded = apply_dict_method(ctx, val_dict, "decode", jv, span);
    let v = local(ctx, "__dec_mv", span);
    let k = local(ctx, "__dec_mk", span);
    let new_pair = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![k, v],
        span,
    };
    let cons = IrExpr::Cons {
        id: ctx.fresh_id(None),
        head: Box::new(new_pair),
        tail: Box::new(local(ctx, "__dec_mdone", span)),
        span,
    };
    let ok_cons = ok(ctx, cons, span);
    let inner_ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_mv".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: ok_cons,
        span,
    };
    let e = local(ctx, "__dec_me", span);
    let err_e = err_wrap(ctx, e, span);
    let inner_err_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_me".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: err_e,
        span,
    };
    let inner_match = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(decoded),
        arms: vec![inner_ok_arm, inner_err_arm],
        span,
    };
    // Ok done arm — but we must first destructure kv into (k, jv). Wrap inner_match
    // in a tuple-pattern match on the kv param.
    let kv = local(ctx, "__dec_kv", span);
    let kv_destructure = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(kv),
        arms: vec![IrArm {
            pat: IrPat::Tuple {
                elems: vec![
                    IrPat::Bind {
                        name: "__dec_mk".to_string(),
                        inner: None,
                        span,
                    },
                    IrPat::Bind {
                        name: "__dec_mjv".to_string(),
                        inner: None,
                        span,
                    },
                ],
                span,
            },
            when: None,
            body: inner_match,
            span,
        }],
        span,
    };
    let acc_ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_mdone".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: kv_destructure,
        span,
    };
    let acc_passthrough = local(ctx, "__dec_macc", span);
    let acc_err_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: acc_passthrough,
        span,
    };
    let fold_body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(acc),
        arms: vec![acc_ok_arm, acc_err_arm],
        span,
    };
    let fold_fn = lambda(
        ctx,
        vec![
            param("__dec_macc".to_string(), span),
            param("__dec_kv".to_string(), span),
        ],
        fold_body,
        span,
    );
    let empty = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: vec![],
        span,
    };
    let seed = ok(ctx, empty, span);
    let folded = stdlib_call(ctx, "std.list", "fold", vec![fold_fn, seed, pairs], span);

    // match folded { Ok acc -> Ok(std.map.fromList(acc)); Err e -> Err e }
    let acc2 = local(ctx, "__dec_mfacc", span);
    let rebuilt = stdlib_call(ctx, "std.map", "fromList", vec![acc2], span);
    let ok_map = ok(ctx, rebuilt, span);
    let final_ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_mfacc".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: ok_map,
        span,
    };
    let fe = local(ctx, "__dec_mfe", span);
    let final_err = err_wrap(ctx, fe, span);
    let final_err_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_mfe".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: final_err,
        span,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(folded),
        arms: vec![final_ok_arm, final_err_arm],
        span,
    }
}

/// `fun(J) -> match J { JObject m -> <read tag/values, dispatch Ok/Err via ok.decode/err.decode>; _ -> Err } end`.
fn decode_result_lambda(
    ctx: &mut LowerCtx<'_>,
    ok_dict: IrExpr,
    err_dict: IrExpr,
    span: Span,
) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};

    let m = local(ctx, "__dec_rm", span);
    let dispatch = decode_result_dispatch(ctx, ok_dict, err_dict, m, span);
    let ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "JObject".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_rm".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: dispatch,
        span,
    };
    let err = decode_error(
        ctx,
        "decode.expected_object",
        "expected a JSON object",
        span,
    );
    let wild_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: err,
        span,
    };
    let j = local(ctx, "__dec_j", span);
    let body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(j),
        arms: vec![ok_arm, wild_arm],
        span,
    };
    lambda(ctx, vec![param("__dec_j".to_string(), span)], body, span)
}

/// Read `"tag"` and `"values"` from the object map, then dispatch on the tag
/// string to decode the single payload value with the matching dictionary.
fn decode_result_dispatch(
    ctx: &mut LowerCtx<'_>,
    ok_dict: IrExpr,
    err_dict: IrExpr,
    m: IrExpr,
    span: Span,
) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};

    // tag_opt = std.map.get "tag" m  : Option JsonValue
    let tag_key = text_lit(ctx, "tag", span);
    let m_for_tag = m;
    let tag_opt = stdlib_call(
        ctx,
        "std.map",
        "get",
        vec![tag_key, m_for_tag.clone()],
        span,
    );

    // The "values" map.get is re-read inside each branch (m is a plain local; re-using
    // the same local name keeps codegen simple since m is bound by the JObject pattern).

    // Build the Ok branch and Err branch payload readers.
    let ok_branch = decode_result_payload(ctx, ok_dict, "Ok", &m_for_tag, span);
    let err_branch = decode_result_payload(ctx, err_dict, "Err", &m_for_tag, span);

    // match tag_opt { Some (JText t) -> <if t=="Ok" ok_branch else if t=="Err" err_branch else Err(unknown_tag)>; _ -> Err(missing tag) }
    let t_for_ok = local(ctx, "__dec_rtag", span);
    let ok_lit = text_lit(ctx, "Ok", span);
    let is_ok = stdlib_call(ctx, "std.op", "eq", vec![t_for_ok, ok_lit], span);
    let t2 = local(ctx, "__dec_rtag", span);
    let err_lit = text_lit(ctx, "Err", span);
    let is_err = stdlib_call(ctx, "std.op", "eq", vec![t2, err_lit], span);
    let unknown = decode_error(ctx, "decode.unknown_tag", "unknown Result tag", span);
    // if t=="Err" then err_branch else unknown
    let err_or_unknown = bool_match(ctx, is_err, err_branch, unknown, span);
    // if t=="Ok" then ok_branch else (err_or_unknown)
    let ok_chain = bool_match(ctx, is_ok, ok_branch, err_or_unknown, span);

    let some_jtext_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Some".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Ctor {
                sym: SymbolRef::Prelude {
                    name: "JText".to_string(),
                },
                fields: vec![],
                args: vec![IrPat::Bind {
                    name: "__dec_rtag".to_string(),
                    inner: None,
                    span,
                }],
                span,
            }],
            span,
        },
        when: None,
        body: ok_chain,
        span,
    };
    let missing = decode_error(ctx, "decode.unknown_tag", "missing Result tag", span);
    let missing_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: missing,
        span,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(tag_opt),
        arms: vec![some_jtext_arm, missing_arm],
        span,
    }
}

/// Read `values[0]` from the object map and decode it with `payload_dict`,
/// wrapping the decoded value in `Ok(Ok v)` / `Ok(Err v)` per `ctor`.
fn decode_result_payload(
    ctx: &mut LowerCtx<'_>,
    payload_dict: IrExpr,
    ctor: &str,
    m: &IrExpr,
    span: Span,
) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};

    // vals_opt = std.map.get "values" m  : Option JsonValue
    let vals_key = text_lit(ctx, "values", span);
    let vals_opt = stdlib_call(ctx, "std.map", "get", vec![vals_key, m.clone()], span);

    // match vals_opt { Some (JList vs) -> match std.list.head vs { Some v0 -> <decode v0>; None -> Err(bad_arity) }; _ -> Err(bad_arity) }
    let vs = local(ctx, "__dec_rvs", span);
    let head = stdlib_call(ctx, "std.list", "head", vec![vs], span);
    let v0 = local(ctx, "__dec_rv0", span);
    let decoded = apply_dict_method(ctx, payload_dict, "decode", v0, span);
    let inner = local(ctx, "__dec_rin", span);
    let wrapped = prelude_ctor(ctx, ctor, vec![("$0".to_string(), inner)], span);
    let ok_wrapped = ok(ctx, wrapped, span);
    let cont = seq_ok(ctx, decoded, "__dec_rin", ok_wrapped, span);
    let some_v0_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Some".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_rv0".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: cont,
        span,
    };
    let bad_arity_1 = decode_error(ctx, "decode.bad_arity", "Result expects 1 value", span);
    let none_v0_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: bad_arity_1,
        span,
    };
    let head_match = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(head),
        arms: vec![some_v0_arm, none_v0_arm],
        span,
    };
    let some_jlist_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Some".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Ctor {
                sym: SymbolRef::Prelude {
                    name: "JList".to_string(),
                },
                fields: vec![],
                args: vec![IrPat::Bind {
                    name: "__dec_rvs".to_string(),
                    inner: None,
                    span,
                }],
                span,
            }],
            span,
        },
        when: None,
        body: head_match,
        span,
    };
    let bad_arity_2 = decode_error(
        ctx,
        "decode.bad_arity",
        "Result expects a values array",
        span,
    );
    let other_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: bad_arity_2,
        span,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(vals_opt),
        arms: vec![some_jlist_arm, other_arm],
        span,
    }
}

// ── Decode helpers (Result construction + sequencing) ─────────────────────────

/// `Ok(v)`.
fn ok(ctx: &mut LowerCtx<'_>, v: IrExpr, span: Span) -> IrExpr {
    prelude_ctor(ctx, "Ok", vec![("$0".to_string(), v)], span)
}

/// `Err(e)` where `e` is an already-built Error value (a forwarded error,
/// not a freshly-coded one).
fn err_wrap(ctx: &mut LowerCtx<'_>, e: IrExpr, span: Span) -> IrExpr {
    prelude_ctor(ctx, "Err", vec![("$0".to_string(), e)], span)
}

/// `Err(#{code, message})` — a freshly-constructed decode error.
fn decode_error(ctx: &mut LowerCtx<'_>, code: &str, message: &str, span: Span) -> IrExpr {
    let err_record = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: ridge_ir::CtorKind::Record,
            owner_type: TyConId(TYCON_ERROR),
            name: "Error".to_string(),
            variant: 0,
        },
        fields: vec![
            ("code".to_string(), text_lit(ctx, code, span)),
            ("message".to_string(), text_lit(ctx, message, span)),
        ],
        span,
    };
    err_wrap(ctx, err_record, span)
}

/// Sequence a fallible sub-decode: `match sub { Ok name -> cont; Err e -> Err e }`.
///
/// Unlike the derived-decode `decode_seq`, this does NOT use `IrExpr::Return`
/// (the synthesised method body is a lambda; `Return` would escape the lambda).
/// It threads the `Err` explicitly through a value-level match instead.
fn seq_ok(ctx: &mut LowerCtx<'_>, sub: IrExpr, ok_name: &str, cont: IrExpr, span: Span) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};
    let ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: ok_name.to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: cont,
        span,
    };
    let e = local(ctx, "__dec_seqe", span);
    let err_e = err_wrap(ctx, e, span);
    let err_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dec_seqe".to_string(),
                inner: None,
                span,
            }],
            span,
        },
        when: None,
        body: err_e,
        span,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(sub),
        arms: vec![ok_arm, err_arm],
        span,
    }
}

/// `match cond { true -> then_e; false -> else_e }` — an `if` over a Bool.
fn bool_match(
    ctx: &mut LowerCtx<'_>,
    cond: IrExpr,
    then_e: IrExpr,
    else_e: IrExpr,
    span: Span,
) -> IrExpr {
    use ridge_ir::{IrArm, IrLit as L, IrPat};
    let true_arm = IrArm {
        pat: IrPat::Lit {
            value: L::Bool(true),
            span,
        },
        when: None,
        body: then_e,
        span,
    };
    let false_arm = IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: else_e,
        span,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(cond),
        arms: vec![true_arm, false_arm],
        span,
    }
}
