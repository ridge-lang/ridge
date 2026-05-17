-- std.bool — Boolean utilities (Tier 1, no stdlib imports).
--
-- All four functions are direct @ffi wrappers to BEAM erlang:*.
-- D044: prefix `!` is exclusively send; Bool.not is the only logical-negation surface.

-- Logical negation.
@ffi("erlang", "not", 1)
pub fn not (b: Bool) -> Bool

-- Logical conjunction (strict — both arguments are always evaluated).
@ffi("erlang", "and", 2)
pub fn and (a: Bool) (b: Bool) -> Bool

-- Logical disjunction (strict — both arguments are always evaluated).
@ffi("erlang", "or", 2)
pub fn or (a: Bool) (b: Bool) -> Bool

-- Convert a boolean to its text representation ("true" or "false").
@ffi("ridge_rt", "bool_to_text", 1)
pub fn toText (b: Bool) -> Text
