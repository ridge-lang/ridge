-- expect: T001
-- T013 scenario (subsumed as T001): polymorphic recursion is unreachable from
-- inferred Ridge code (no type annotations on recursive fns). This fixture
-- demonstrates attempted recursive use at different types, which unification
-- catches as a TypeMismatch (T001) rather than T013 PolymorphicRecursion.
fn f (x: Int) -> Int =
    f "wrong"
