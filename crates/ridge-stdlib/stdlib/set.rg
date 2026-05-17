-- std.set — Set utilities (Tier 2).
--
-- D113: Set a is implemented as Map a Bool (Erlang map with `true` values).
-- Set a is a language built-in (prelude). This module provides helper
-- functions; the type itself does not need to be declared here.
--
-- Implemented using private BEAM bridges to avoid an SCC cycle with std.map.

-- Private BEAM map bridges (mirrors std.map internals to avoid SCC cycle).
@ffi("maps", "new", 0)
fn _setMapNew () -> Set a

@ffi("maps", "put", 3)
fn _setMapPut (k: a) (v: Bool) (m: Set a) -> Set a

@ffi("maps", "remove", 2)
fn _setMapRemove (k: a) (m: Set a) -> Set a

@ffi("maps", "is_key", 2)
fn _setMapIsKey (k: a) (m: Set a) -> Bool

@ffi("maps", "keys", 1)
fn _setMapKeys (m: Set a) -> List a

@ffi("maps", "size", 1)
fn _setMapSize (m: Set a) -> Int

@ffi("maps", "merge", 2)
fn _setMapMerge (a: Set x) (b: Set x) -> Set x

-- Return an empty set.
pub fn empty -> Set a = _setMapNew ()

-- Construct a set from a list of elements.
pub fn fromList (xs: List a) -> Set a =
    _fromListAcc xs (_setMapNew ())

fn _fromListAcc (xs: List a) (acc: Set a) -> Set a =
    match xs
        []      -> acc
        x :: rest -> _fromListAcc rest (_setMapPut x true acc)

-- Convert a set to a list of elements (order not guaranteed).
pub fn toList (s: Set a) -> List a =
    _setMapKeys s

-- Insert an element into the set.
pub fn insert (x: a) (s: Set a) -> Set a =
    _setMapPut x true s

-- Remove an element from the set.
pub fn remove (x: a) (s: Set a) -> Set a =
    _setMapRemove x s

-- Return true if the element is in the set.
pub fn contains (x: a) (s: Set a) -> Bool =
    _setMapIsKey x s

-- Return the union of two sets (elements from either).
pub fn union (a: Set x) (b: Set x) -> Set x =
    _setMapMerge a b

-- Return the intersection of two sets (elements in both).
pub fn intersect (a: Set x) (b: Set x) -> Set x =
    let aKeys = _setMapKeys a
    _intersectFilter aKeys b (_setMapNew ())

fn _intersectFilter (xs: List x) (b: Set x) (acc: Set x) -> Set x =
    match xs
        []      -> acc
        k :: rest ->
            if _setMapIsKey k b then _intersectFilter rest b (_setMapPut k true acc)
            else _intersectFilter rest b acc

-- Return the difference of two sets (elements in `a` but not in `b`).
pub fn difference (a: Set x) (b: Set x) -> Set x =
    let aKeys = _setMapKeys a
    _differenceFilter aKeys b (_setMapNew ())

fn _differenceFilter (xs: List x) (b: Set x) (acc: Set x) -> Set x =
    match xs
        []      -> acc
        k :: rest ->
            if _setMapIsKey k b then _differenceFilter rest b acc
            else _differenceFilter rest b (_setMapPut k true acc)

-- Return the number of elements in the set.
pub fn size (s: Set a) -> Int =
    _setMapSize s
