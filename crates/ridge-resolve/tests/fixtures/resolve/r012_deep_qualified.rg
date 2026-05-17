-- expect: R012
-- T15 / §5.1 R012 (variant 2): the three-segment qualified name
-- `Unknown.Nested.field` cannot be resolved — `Unknown` is not a bound alias,
-- actor type, or constructor in scope.
fn f = Unknown.Nested.field
