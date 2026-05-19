-- expect: T009
-- T008 scenario: using the wrong arity on a known constructor is the
-- closest reachable error. T008 (UnknownConstructor) is defensive — Phase 3
-- catches truly unknown constructors before Phase 4. Instead we exercise
-- T009 (WrongConstructorArity) which is reliably emitted via a pattern.
fn bad -> Int =
    match Some 1
        Some n m -> n
        None     -> 0
