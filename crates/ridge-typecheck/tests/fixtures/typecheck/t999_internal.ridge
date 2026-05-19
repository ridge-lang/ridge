-- expect: T001
-- T999 (InternalTypeError) is a defensive catch-all for type-checker invariant
-- violations. It is synthesised internally and never emitted by valid Ridge code.
-- This fixture exercises T001 TypeMismatch as the closest user-visible error.
fn f -> Int = "not an int"
