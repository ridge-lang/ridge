-- std.option — Option helpers (Tier 1, no stdlib imports).
--
-- The Option a type (Some a | None) is declared in the language prelude;
-- this module provides helper functions only (§4.3).
-- D027: withDefault naming reaffirmed.
-- D060: discard provided.

-- Return the contained value, or the default if None.
pub fn withDefault (d: a) (o: Option a) -> a =
    match o
        Some v -> v
        None   -> d

-- Apply a function to the contained value, or return None.
pub fn map (f: fn a -> b) (o: Option a) -> Option b =
    match o
        Some v -> Some (f v)
        None   -> None

-- Apply a function that returns an Option, flattening one level.
pub fn flatMap (f: fn a -> Option b) (o: Option a) -> Option b =
    match o
        Some v -> f v
        None   -> None

-- Return the first Option if it is Some, otherwise return the alternative.
pub fn orElse (alt: Option a) (o: Option a) -> Option a =
    match o
        Some _ -> o
        None   -> alt

-- Return true if the Option contains a value.
pub fn isSome (o: Option a) -> Bool =
    match o
        Some _ -> true
        None   -> false

-- Return true if the Option is empty.
pub fn isNone (o: Option a) -> Bool =
    match o
        Some _ -> false
        None   -> true

-- Discard the Option value, returning Unit.
-- D060: use this when you want to ignore a value without triggering a warning.
pub fn discard (o: Option a) -> Unit =
    match o
        Some _ -> ()
        None   -> ()
