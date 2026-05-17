-- Private helpers for std.set test suite.
-- FFI bridges + pure-Ridge helpers replicate set.rg (T17+ deferred).
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

fn _fromListAcc (xs: List a) (acc: Set a) -> Set a =
    match xs
        [] -> acc
        x :: rest -> _fromListAcc rest (_setMapPut x true acc)

fn _fromList (xs: List a) -> Set a = _fromListAcc xs (_setMapNew ())

fn _toList (s: Set a) -> List a = _setMapKeys s
fn _insert (x: a) (s: Set a) -> Set a = _setMapPut x true s
fn _remove (x: a) (s: Set a) -> Set a = _setMapRemove x s
fn _contains (x: a) (s: Set a) -> Bool = _setMapIsKey x s
fn _union (a: Set x) (b: Set x) -> Set x = _setMapMerge a b
fn _size (s: Set a) -> Int = _setMapSize s

fn _intersectFilter (xs: List x) (b: Set x) (acc: Set x) -> Set x =
    match xs
        [] -> acc
        k :: rest ->
            if _setMapIsKey k b then _intersectFilter rest b (_setMapPut k true acc)
            else _intersectFilter rest b acc

fn _intersect (a: Set x) (b: Set x) -> Set x =
    let aKeys = _setMapKeys a
    _intersectFilter aKeys b (_setMapNew ())

fn _differenceFilter (xs: List x) (b: Set x) (acc: Set x) -> Set x =
    match xs
        [] -> acc
        k :: rest ->
            if _setMapIsKey k b then _differenceFilter rest b acc
            else _differenceFilter rest b (_setMapPut k true acc)

fn _difference (a: Set x) (b: Set x) -> Set x =
    let aKeys = _setMapKeys a
    _differenceFilter aKeys b (_setMapNew ())

@ffi("erlang", "length", 1)
fn _listLength (xs: List a) -> Int

pub fn test_smoke_set () -> Result Unit Text = Ok ()

pub fn test_empty_size_zero () -> Result Unit Text =
    if _size Set.empty == 0 then Ok ()
    else Err "Set.empty size should be 0"

pub fn test_fromList_dedups () -> Result Unit Text =
    if _size (_fromList [1, 2, 1, 2]) == 2 then Ok ()
    else Err "Set.fromList [1,2,1,2] should deduplicate to size 2"

pub fn test_insert_then_contains () -> Result Unit Text =
    if _contains 5 (_insert 5 Set.empty) then Ok ()
    else Err "Set.insert 5 then contains should be true"

pub fn test_remove_then_contains_false () -> Result Unit Text =
    if _contains 5 (_remove 5 (_insert 5 Set.empty)) then Err "Set.remove 5 should make contains false"
    else Ok ()

pub fn test_union_combines () -> Result Unit Text =
    if _size (_union (_fromList [1, 2]) (_fromList [2, 3])) == 3 then Ok ()
    else Err "Set.union [1,2] [2,3] should have size 3"

pub fn test_intersect_overlap () -> Result Unit Text =
    if _size (_intersect (_fromList [1, 2]) (_fromList [2, 3])) == 1 then Ok ()
    else Err "Set.intersect [1,2] [2,3] should have size 1"

pub fn test_difference_excludes () -> Result Unit Text =
    if _contains 1 (_difference (_fromList [1, 2]) (_fromList [2, 3])) then Ok ()
    else Err "Set.difference [1,2] [2,3] should contain 1"

pub fn test_toList_round_trips_count () -> Result Unit Text =
    if _listLength (_toList (_fromList [1, 2, 3])) == 3 then Ok ()
    else Err "Set.toList fromList [1,2,3] should have length 3"
