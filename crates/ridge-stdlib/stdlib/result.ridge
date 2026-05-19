-- std.result — Result helpers (Tier 1, no stdlib imports).
--
-- The Result a e type (Ok a | Err e) is declared in the language prelude;
-- this module provides helper functions only (§4.3).
-- discard provided.

-- Apply a function to the Ok value, propagating Err unchanged.
pub fn map (f: fn a -> b) (r: Result a e) -> Result b e =
    match r
        Ok v  -> Ok (f v)
        Err e -> Err e

-- Apply a function to the Err value, propagating Ok unchanged.
pub fn mapErr (f: fn e -> e2) (r: Result a e) -> Result a e2 =
    match r
        Ok v  -> Ok v
        Err e -> Err (f e)

-- Apply a function that returns a Result, flattening one level.
pub fn flatMap (f: fn a -> Result b e) (r: Result a e) -> Result b e =
    match r
        Ok v  -> f v
        Err e -> Err e

-- Return the Ok value, or the default if Err.
pub fn withDefault (d: a) (r: Result a e) -> a =
    match r
        Ok v  -> v
        Err _ -> d

-- Return true if the Result is Ok.
pub fn isOk (r: Result a e) -> Bool =
    match r
        Ok _  -> true
        Err _ -> false

-- Return true if the Result is Err.
pub fn isErr (r: Result a e) -> Bool =
    match r
        Ok _  -> false
        Err _ -> true

-- Discard the Result value, returning Unit.
-- Use this when you want to ignore a value without triggering a warning.
pub fn discard (r: Result a e) -> Unit =
    match r
        Ok _  -> ()
        Err _ -> ()
