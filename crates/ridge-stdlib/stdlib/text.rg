-- std.text — Text (byte-string) utilities (Tier 2).
--
-- `byteSize` is the 0.1.0 surface name; `length` is reserved for
-- 0.2.0 codepoint-aware semantics.
-- Single-line strings only in 0.1.0.
-- String interpolation set is closed.
-- All functions are pure (no capability required).

-- Return the byte size of a text value.
-- Renamed from `length`; codepoint-aware `length` is deferred to 0.2.0.
@ffi("erlang", "byte_size", 1)
pub fn byteSize (s: Text) -> Int

-- Raw iolist-to-binary bridge (private — used by concat to avoid self-recursion).
-- `a ++ b` in text.rg lowered to `std.text:concat(a,b)` by ridge-lower,
-- which is a self-call.  Use erlang:iolist_to_binary/1 as the canonical
-- binary-concatenation primitive instead.
@ffi("erlang", "iolist_to_binary", 1)
fn _iolistToBin (l: List Text) -> Text

-- Concatenate two text values.
pub fn concat (a: Text) (b: Text) -> Text =
    _iolistToBin [a, b]

-- Raw binary:split/3 bridge (private — called with options list).
-- Kept for the non-global splitN helper below.
@ffi("binary", "split", 3)
fn _binarySplit3 (s: Text) (sep: Text) (opts: List Text) -> List Text

-- Split-all bridge via ridge_rt so we get
-- binary:split(_, _, [global]).  Passing the `global` Erlang atom from Ridge
-- is fragile (no atom literals), so the runtime wrapper is the clean path.
@ffi("ridge_rt", "text_split_all", 2)
fn _splitAll (sep: Text) (s: Text) -> List Text

-- Split text on a separator, returning all parts (every occurrence).
pub fn split (sep: Text) (s: Text) -> List Text =
    _splitAll sep s

-- Split text on a separator, returning at most n+1 parts.
pub fn splitN (n: Int) (sep: Text) (s: Text) -> List Text =
    let parts = split sep s
    _take n parts

-- Helper: take at most n elements from a list (private).
fn _take (n: Int) (xs: List a) -> List a =
    if n <= 0 then []
    else
        match xs
            []          -> []
            x :: rest   -> x :: _take (n - 1) rest

-- Split text on any of the given separators (fold over each sep).
pub fn splitAny (seps: List Text) (s: Text) -> List Text =
    _splitAnyAcc seps [s]

fn _splitAnyAcc (seps: List Text) (acc: List Text) -> List Text =
    match seps
        []          -> acc
        sep :: rest -> _splitAnyAcc rest (_flatMapSplit sep acc)

fn _flatMapSplit (sep: Text) (xs: List Text) -> List Text =
    match xs
        []          -> []
        x :: rest   -> _appendLists (split sep x) (_flatMapSplit sep rest)

fn _appendLists (xs: List a) (ys: List a) -> List a =
    match xs
        []          -> ys
        x :: rest   -> x :: _appendLists rest ys

-- Split into lines on "\n"; strip trailing "\r" from each segment.
pub fn lines (s: Text) -> List Text =
    let parts = split "\n" s
    _stripCrAll parts

fn _stripCrAll (xs: List Text) -> List Text =
    match xs
        []          -> []
        x :: rest   -> _stripTrailingCr x :: _stripCrAll rest

-- Raw binary:part/3 bridge (private).
@ffi("binary", "part", 3)
fn _binaryPart (s: Text) (start: Int) (len: Int) -> Text

fn _stripTrailingCr (s: Text) -> Text =
    let n = byteSize s
    if n > 0 then
        let last = _binaryPart s (n - 1) 1
        if last == "\r" then _binaryPart s 0 (n - 1)
        else s
    else s

-- Remove leading and trailing whitespace.
@ffi("string", "trim", 1)
pub fn trim (s: Text) -> Text

-- Convert to upper case.
@ffi("string", "uppercase", 1)
pub fn toUpper (s: Text) -> Text

-- Convert to lower case.
@ffi("string", "lowercase", 1)
pub fn toLower (s: Text) -> Text

-- Return true if the text starts with the given prefix.
pub fn startsWith (prefix: Text) (s: Text) -> Bool =
    let pLen = byteSize prefix
    let sLen = byteSize s
    if pLen > sLen then false
    else _binaryPart s 0 pLen == prefix

-- Return true if the text ends with the given suffix.
pub fn endsWith (suffix: Text) (s: Text) -> Bool =
    let sufLen = byteSize suffix
    let sLen   = byteSize s
    if sufLen > sLen then false
    else _binaryPart s (sLen - sufLen) sufLen == suffix

-- Return true if the text contains the needle.
pub fn contains (needle: Text) (s: Text) -> Bool =
    let parts = split needle s
    _moreThanOne parts

fn _moreThanOne (xs: List a) -> Bool =
    match xs
        []          -> false
        _ :: []     -> false
        _ :: _ :: _ -> true

-- Raw binary:replace/4 bridge (private — 4th arg = options list).
@ffi("binary", "replace", 4)
fn _binaryReplace4 (s: Text) (from: Text) (to: Text) (opts: List Text) -> Text

-- Replace all occurrences of `from` with `to`.
pub fn replace (from: Text) (to: Text) (s: Text) -> Text =
    _binaryReplace4 s from to []

-- Raw binary:copy/2 bridge (private).
@ffi("binary", "copy", 2)
fn _binaryCopy (s: Text) (n: Int) -> Text

-- Pad the text on the left to at least `n` bytes.
pub fn padLeft (n: Int) (pad: Text) (s: Text) -> Text =
    let current = byteSize s
    if current >= n then s
    else concat (_binaryCopy pad (n - current)) s

-- Pad the text on the right to at least `n` bytes.
pub fn padRight (n: Int) (pad: Text) (s: Text) -> Text =
    let current = byteSize s
    if current >= n then s
    else concat s (_binaryCopy pad (n - current))

-- Return true if the text is empty (zero bytes).
pub fn isEmpty (s: Text) -> Bool =
    byteSize s == 0
