-- expect: T001
-- T002 scenario: passing Text where Int expected at a call site.
-- T001 subsumes T002 for inferred code in 0.1.0.
fn add (x: Int) (y: Int) -> Int = x + y
fn bad = add "hello" 2
