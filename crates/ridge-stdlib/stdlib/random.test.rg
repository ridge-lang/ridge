-- Private FFI bridges for std.random test suite.
-- These replicate random.rg FFI declarations in local scope because cross-module
-- calls are unsupported (T17+ deferred).
@ffi("ridge_rt", "random_seed", 1)
fn _seed (s: Int) -> Unit

@ffi("ridge_rt", "random_int", 2)
fn _randomInt (lo: Int) (hi: Int) -> Int

@ffi("ridge_rt", "random_float", 1)
fn _randomFloat (_unit: Unit) -> Float

@ffi("ridge_rt", "random_alphanumeric", 1)
fn _alphanumeric (n: Int) -> Text

@ffi("ridge_rt", "random_choice", 1)
fn _choice (xs: List a) -> Option a

@ffi("erlang", "byte_size", 1)
fn _byteSize (s: Text) -> Int

fn _isSome (o: Option a) -> Bool =
    match o
        Some _ -> true
        None -> false

fn _isNone (o: Option a) -> Bool =
    match o
        Some _ -> false
        None -> true

fn _emptyIntList (_u: Unit) -> List Int = []

pub fn test_smoke_random () -> Result Unit Text = Ok ()

-- random.int returns a value within [lo, hi].
pub fn random test_int_within_range () -> Result Unit Text =
    let _ = _seed 1
    let v = _randomInt 0 9
    if v >= 0 then
        if v <= 9 then Ok ()
        else Err "random.int should be <= 9"
    else Err "random.int should be >= 0"

-- The same seed produces the same first value.
pub fn random test_seed_reproducibility () -> Result Unit Text =
    let _ = _seed 42
    let v1 = _randomInt 0 1000000
    let _ = _seed 42
    let v2 = _randomInt 0 1000000
    if v1 == v2 then Ok ()
    else Err "random.int with same seed should produce same value"

-- random.alphanumeric returns a Text of exactly n bytes.
pub fn random test_alphanumeric_length () -> Result Unit Text =
    let s = _alphanumeric 10
    if _byteSize s == 10 then Ok ()
    else Err "random.alphanumeric 10 should have byteSize 10"

-- random.choice returns Some for a non-empty list; None for empty.
pub fn random test_choice_some_for_nonempty () -> Result Unit Text =
    if _isSome (_choice [1, 2, 3]) then
        if _isNone (_choice (_emptyIntList ())) then Ok ()
        else Err "random.choice [] should be None"
    else Err "random.choice [1,2,3] should be Some"
