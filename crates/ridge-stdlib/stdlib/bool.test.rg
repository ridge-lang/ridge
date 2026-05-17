-- Private FFI bridges for std.bool test suite.
-- These replicate the bool.rg FFI declarations in local scope so that
-- test functions can call them without a cross-module qualified reference
-- (which is unsupported for WorkspaceModule imports in the current typecheck —
-- T17+ deferred per crates/ridge-typecheck/src/stdlib_env.rs:87-92).
@ffi("erlang", "not", 1)
fn _not (b: Bool) -> Bool

@ffi("erlang", "and", 2)
fn _and (a: Bool) (b: Bool) -> Bool

@ffi("erlang", "or", 2)
fn _or (a: Bool) (b: Bool) -> Bool

@ffi("ridge_rt", "bool_to_text", 1)
fn _boolToText (b: Bool) -> Text

pub fn test_smoke_bool () -> Result Unit Text = Ok ()

pub fn test_not_true_is_false () -> Result Unit Text =
    if _not true == false then Ok () else Err "Bool.not true should be false"

pub fn test_not_false_is_true () -> Result Unit Text =
    if _not false == true then Ok () else Err "Bool.not false should be true"

pub fn test_and_truth_table () -> Result Unit Text =
    if _and true true == true then
        if _and true false == false then
            if _and false true == false then
                if _and false false == false then Ok ()
                else Err "Bool.and false false should be false"
            else Err "Bool.and false true should be false"
        else Err "Bool.and true false should be false"
    else Err "Bool.and true true should be true"

pub fn test_or_truth_table () -> Result Unit Text =
    if _or true true == true then
        if _or true false == true then
            if _or false true == true then
                if _or false false == false then Ok ()
                else Err "Bool.or false false should be false"
            else Err "Bool.or false true should be true"
        else Err "Bool.or true false should be true"
    else Err "Bool.or true true should be true"

pub fn test_toText_round_trip () -> Result Unit Text =
    if _boolToText true == "true" then
        if _boolToText false == "false" then Ok ()
        else Err "Bool.toText false should be false"
    else Err "Bool.toText true should be true"
