-- Private helpers for std.result test suite.
-- Pure-Ridge helpers replicate result.rg (T17+ deferred; no FFI needed since
-- all result.rg functions are pure Ridge).
fn _resMap (f: fn a -> b) (r: Result a e) -> Result b e =
    match r
        Ok v -> Ok (f v)
        Err e -> Err e

fn _mapErr (f: fn e -> e2) (r: Result a e) -> Result a e2 =
    match r
        Ok v -> Ok v
        Err e -> Err (f e)

fn _flatMap (f: fn a -> Result b e) (r: Result a e) -> Result b e =
    match r
        Ok v -> f v
        Err e -> Err e

fn _withDefault (d: a) (r: Result a e) -> a =
    match r
        Ok v -> v
        Err _ -> d

fn _isOk (r: Result a e) -> Bool =
    match r
        Ok _ -> true
        Err _ -> false

fn _isErr (r: Result a e) -> Bool =
    match r
        Ok _ -> false
        Err _ -> true

fn _discard (r: Result a e) -> Unit =
    match r
        Ok _ -> ()
        Err _ -> ()

fn _okInt (n: Int) -> Result Int Text = Ok n
fn _errInt (msg: Text) -> Result Int Text = Err msg

pub fn test_smoke_result () -> Result Unit Text = Ok ()

pub fn test_map_ok () -> Result Unit Text =
    if _resMap (fn x -> x + 1) (_okInt 5) == Ok 6 then Ok ()
    else Err "Result.map (+1) (Ok 5) should be Ok 6"

pub fn test_map_err_propagates () -> Result Unit Text =
    if _resMap (fn x -> x + 1) (_errInt "fail") == Err "fail" then Ok ()
    else Err "Result.map on Err should propagate Err"

pub fn test_mapErr_err () -> Result Unit Text =
    if _mapErr (fn _ -> "renamed") (_errInt "x") == Err "renamed" then Ok ()
    else Err "Result.mapErr should rename the error"

pub fn test_flatMap_ok_chain () -> Result Unit Text =
    if _flatMap (fn x -> Ok (x * 2)) (_okInt 5) == Ok 10 then Ok ()
    else Err "Result.flatMap (*2) (Ok 5) should be Ok 10"

pub fn test_flatMap_err_short_circuits () -> Result Unit Text =
    if _flatMap (fn x -> Ok (x * 2)) (_errInt "no") == Err "no" then Ok ()
    else Err "Result.flatMap on Err should short-circuit"

pub fn test_withDefault_ok () -> Result Unit Text =
    if _withDefault 0 (_okInt 5) == 5 then Ok ()
    else Err "Result.withDefault 0 (Ok 5) should be 5"

pub fn test_withDefault_err () -> Result Unit Text =
    if _withDefault 0 (_errInt "x") == 0 then Ok ()
    else Err "Result.withDefault 0 (Err x) should be 0"

pub fn test_isOk_isErr () -> Result Unit Text =
    if _isOk (_okInt 1) then
        if _isErr (_errInt "x") then Ok ()
        else Err "Result.isErr (Err x) should be true"
    else Err "Result.isOk (Ok 1) should be true"
