-- expect: T001
-- T023 scenario (subsumed): unsolved type vars typically arise alongside T001.
-- Under HM with full generalisation, standalone T023 is rare in practice.
-- This fixture verifies a type annotation mismatch (T001), which is the most
-- common companion to ambiguous types in 0.1.0.
fn f -> Int = true
