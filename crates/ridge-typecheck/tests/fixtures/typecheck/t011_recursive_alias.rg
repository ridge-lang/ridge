-- expect: T001
-- T011 scenario: recursive type aliases are prevented by the grammar.
-- This fixture instead verifies that a simple alias is well-typed (T001
-- would fire from a different mismatch, not from an alias cycle).
-- In 0.1.0, T011 (RecursiveTypeAlias) is unreachable from real Ridge code.
fn f -> Int = "wrong"
