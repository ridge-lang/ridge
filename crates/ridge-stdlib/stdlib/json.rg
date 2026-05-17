-- std.json — JSON encoding and decoding (Tier 4, imports std.text).
--
-- §3.17: OTP-27 native `json` module chosen on security
-- grounds (hand-rolled parsers are a CVE-class surface).  The `decode`
-- function bridges to `ridge_rt:json_decode/1` which wraps OTP 27's
-- `json:decode/1` and maps the result into `Result JsonValue Error`.
--
-- JsonValue is the Ridge JSON value ADT (§3.17).
-- Records and generic-derive are out of scope for 0.2.0.
-- Error is a pre-allocated built-in record (§3.11):
--   { code: Text, message: Text }
--
-- encode strategy: ridge_rt:json_encode/1 (avoids recursive-type limitations
-- of the Phase-7 single-tier pipeline; the Erlang side walks the JsonValue
-- BEAM tuple tree directly).
--
-- decode strategy: ridge_rt:json_decode/1 wrapping OTP-27 json:decode/1.

-- The Ridge JSON value type.
-- Self-referential variants (JList, JObject) reference JsonValue; the Erlang
-- wire representation is a nested tagged-tuple tree built by the bridge.
pub type JsonValue = JNull | JBool Bool | JInt Int | JFloat Float | JText Text | JList (List JsonValue) | JObject (Map Text JsonValue)

-- Encode a JsonValue to its JSON text representation.
-- Bridge: ridge_rt:json_encode/1 — walks the tagged-tuple tree recursively.
@ffi("ridge_rt", "json_encode", 1)
pub fn encode (v: JsonValue) -> Text

-- Decode a JSON text into a JsonValue.
-- Bridge: ridge_rt:json_decode/1 — wraps OTP-27 json:decode/1, maps
--         {ok, V} -> Ok(V) and {error, ...} -> Err(Error).
-- Returns Err(Error) on malformed input.
@ffi("ridge_rt", "json_decode", 1)
pub fn decode (s: Text) -> Result JsonValue Error

-- Encode an integer as a JSON number text.
-- Pure Ridge: delegates to Int.toText.
pub fn encodeInt (n: Int) -> Text = Int.toText n

-- Encode a boolean as a JSON boolean text ("true" or "false").
-- Pure Ridge: if/then/else over the Bool value.
pub fn encodeBool (b: Bool) -> Text =
    if b then "true" else "false"

-- Encode a text value as a JSON string (with surrounding double quotes).
-- Pure Ridge: Text.concat of the surrounding quotes and the value.
-- NOTE: does not escape special characters — strings must be ASCII-safe
-- for Phase 7 (full escaping deferred to the Erlang-side bridge, Phase 8+).
pub fn encodeText (s: Text) -> Text =
    Text.concat (Text.concat "\"" s) "\""

-- ── JsonValue constructors via FFI shims. ────────────────────────────────────
--
-- Cross-module resolution of type-variant constructors (e.g. `Json.JInt 42`)
-- is deferred to 0.2.0: the resolver returns `Binding::StdlibSymbol`, the
-- lower has no path for StdlibSymbol-as-non-prelude-constructor, and the
-- json_encode wire format uses lowercase-snake atoms (`json_int`) that
-- wouldn't match the constructor names anyway.
--
-- These shims let user code construct JsonValue trees today by routing
-- through `ridge_rt:json_*` wrappers that emit the exact tuple shape
-- `json_encode/1` consumes.  Decoded values from `Json.decode` are
-- already in this shape, so user-constructed and decoded values round-trip
-- through `Json.encode` interchangeably.

@ffi("ridge_rt", "json_null", 1)
pub fn jNull (_unit: Unit) -> JsonValue

@ffi("ridge_rt", "json_bool", 1)
pub fn jBool (b: Bool) -> JsonValue

@ffi("ridge_rt", "json_int", 1)
pub fn jInt (n: Int) -> JsonValue

@ffi("ridge_rt", "json_float", 1)
pub fn jFloat (f: Float) -> JsonValue

@ffi("ridge_rt", "json_text", 1)
pub fn jText (s: Text) -> JsonValue

@ffi("ridge_rt", "json_list", 1)
pub fn jList (xs: List JsonValue) -> JsonValue

@ffi("ridge_rt", "json_object", 1)
pub fn jObject (m: Map Text JsonValue) -> JsonValue
