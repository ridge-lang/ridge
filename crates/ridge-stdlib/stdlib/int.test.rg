-- Private helpers for std.int test suite.
-- FFI bridges replicate int.rg declarations in local scope (T17+ cross-module
-- typecheck deferred — see ridge-typecheck/src/stdlib_env.rs:87-92).
-- Pure-Ridge helpers replicate int.rg bodies for wrappingAdd/saturatingAdd.
@ffi("erlang", "integer_to_binary", 1)
fn _toText (n: Int) -> Text

@ffi("ridge_rt", "int_parse", 1)
fn _parse (s: Text) -> Option Int

@ffi("erlang", "abs", 1)
fn _abs (n: Int) -> Int

@ffi("erlang", "-", 1)
fn _neg (n: Int) -> Int

fn _min (a: Int) (b: Int) -> Int = if a <= b then a else b
fn _max (a: Int) (b: Int) -> Int = if a >= b then a else b

fn _wrappingAdd (a: Int) (b: Int) -> Int =
    let s = a + b
    let maxVal = 9223372036854775807
    let minVal = -9223372036854775808
    let modulus = 18446744073709551616
    if s > maxVal then s - modulus
    else if s < minVal then s + modulus
    else s

fn _saturatingAdd (a: Int) (b: Int) -> Int =
    let s = a + b
    let maxVal = 9223372036854775807
    let minVal = -9223372036854775808
    if s > maxVal then maxVal
    else if s < minVal then minVal
    else s

pub fn test_smoke_int () -> Result Unit Text = Ok ()

pub fn test_toText_zero () -> Result Unit Text =
    if _toText 0 == "0" then Ok ()
    else Err "Int.toText 0 should be 0"

pub fn test_toText_negative () -> Result Unit Text =
    if _toText (_neg 42) == "-42" then Ok ()
    else Err "Int.toText (-42) should be -42"

pub fn test_parse_decimal () -> Result Unit Text =
    if _parse "123" == Some 123 then Ok ()
    else Err "Int.parse 123 should be Some 123"

pub fn test_parse_invalid_is_none () -> Result Unit Text =
    if _parse "abc" == None then Ok ()
    else Err "Int.parse abc should be None"

pub fn test_abs_positive () -> Result Unit Text =
    if _abs 5 == 5 then
        if _abs (_neg 5) == 5 then Ok ()
        else Err "Int.abs (-5) should be 5"
    else Err "Int.abs 5 should be 5"

pub fn test_min_picks_smaller () -> Result Unit Text =
    if _min 3 7 == 3 then
        if _min 7 3 == 3 then Ok ()
        else Err "Int.min 7 3 should be 3"
    else Err "Int.min 3 7 should be 3"

pub fn test_max_picks_larger () -> Result Unit Text =
    if _max 3 7 == 7 then Ok ()
    else Err "Int.max 3 7 should be 7"

pub fn test_add_sub_mul_div () -> Result Unit Text =
    let result = ((10 + 5) - 3) * 2 / 4
    if result == 6 then Ok ()
    else Err "((10 + 5) - 3) * 2 / 4 should be 6"

pub fn test_neg_round_trip () -> Result Unit Text =
    if _neg (_neg 42) == 42 then Ok ()
    else Err "Int.neg (Int.neg 42) should be 42"

pub fn test_wrappingAdd_no_overflow () -> Result Unit Text =
    if _wrappingAdd 3 4 == 7 then Ok ()
    else Err "wrappingAdd 3 4 should be 7 (no overflow)"

pub fn test_saturatingAdd_clamps_high () -> Result Unit Text =
    let maxVal = 9223372036854775807
    if _saturatingAdd maxVal 1 == maxVal then Ok ()
    else Err "saturatingAdd maxVal 1 should clamp to maxVal"

pub fn test_saturatingAdd_clamps_low () -> Result Unit Text =
    let minVal = -9223372036854775808
    if _saturatingAdd minVal (_neg 1) == minVal then Ok ()
    else Err "saturatingAdd minVal (-1) should clamp to minVal"
