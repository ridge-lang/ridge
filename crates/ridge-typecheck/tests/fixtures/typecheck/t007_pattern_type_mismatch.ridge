-- expect: T001
-- T007 scenario (subsumed as T001): matching Int scrutinee against Text literal.
-- PatternTypeMismatch is emitted as TypeMismatch by unification in 0.1.0.
fn f (x: Int) -> Int =
    match x
        "hello" -> 1
        _       -> 2
