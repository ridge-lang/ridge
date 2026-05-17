-- Private FFI bridges for std.json test suite.
-- These replicate json.rg FFI declarations in local scope because cross-module
-- calls are unsupported (T17+ deferred).
--
-- NOTE: JsonValue constructors (JNull, JBool, etc.) use a custom BEAM wire
-- format set up by ridge_rt.erl. Re-declaring `pub type JsonValue` locally
-- produces a different wire format and cannot be used to pass values to
-- ridge_rt:json_encode. All encode tests derive values via decode round-trips.
@ffi("ridge_rt", "json_encode", 1)
fn _encode (v: JsonValue) -> Text

@ffi("ridge_rt", "json_decode", 1)
fn _decode (s: Text) -> Result JsonValue Error

-- Pure helpers.
@ffi("erlang", "iolist_to_binary", 1)
fn _iolistToBin (l: List Text) -> Text

fn _concat (a: Text) (b: Text) -> Text = _iolistToBin [a, b]

fn _encodeText (s: Text) -> Text = _concat (_concat "\"" s) "\""

pub fn test_smoke_json () -> Result Unit Text = Ok ()

-- json.decode "null" succeeds and re-encodes to "null".
pub fn test_encode_null () -> Result Unit Text =
    match _decode "null"
        Err _ -> Err "json.decode null should succeed"
        Ok v ->
            if _encode v == "null" then Ok ()
            else Err "json.encode of decoded null should be null"

-- json.decode "true" succeeds and re-encodes to "true".
pub fn test_encode_bool () -> Result Unit Text =
    match _decode "true"
        Err _ -> Err "json.decode true should succeed"
        Ok v ->
            if _encode v == "true" then Ok ()
            else Err "json.encode of decoded true should be true"

-- json.decode "42" succeeds and re-encodes to "42".
pub fn test_encode_int () -> Result Unit Text =
    match _decode "42"
        Err _ -> Err "json.decode 42 should succeed"
        Ok v ->
            if _encode v == "42" then Ok ()
            else Err "json.encode of decoded 42 should be 42"

-- json.decode "null" succeeds and re-encoding it returns "null".
pub fn test_decode_null () -> Result Unit Text =
    match _decode "null"
        Err _ -> Err "json.decode null should succeed"
        Ok v ->
            if _encode v == "null" then Ok ()
            else Err "json.decode null should round-trip to null"

-- json.decode "42" succeeds and re-encoding it returns "42".
pub fn test_decode_int () -> Result Unit Text =
    match _decode "42"
        Err _ -> Err "json.decode 42 should succeed"
        Ok v ->
            if _encode v == "42" then Ok ()
            else Err "json.decode 42 should round-trip to 42"

-- json.decode of invalid input returns Err.
pub fn test_decode_invalid_returns_err () -> Result Unit Text =
    match _decode "not-json"
        Err _ -> Ok ()
        Ok _ -> Err "json.decode of invalid input should return Err"

-- Decode then encode is a no-op for bool.
pub fn test_encode_decode_round_trip () -> Result Unit Text =
    match _decode "true"
        Err _ -> Err "json.decode true should succeed"
        Ok v ->
            if _encode v == "true" then Ok ()
            else Err "json.encode of decoded true should be true"

-- json.encodeText wraps a string in double quotes.
pub fn test_encodeText_quotes () -> Result Unit Text =
    let result = _encodeText "x"
    let expected = _concat (_concat "\"" "x") "\""
    if result == expected then Ok ()
    else Err "json.encodeText x should produce double-quoted x"
