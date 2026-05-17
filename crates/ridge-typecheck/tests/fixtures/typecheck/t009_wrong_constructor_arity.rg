-- expect: T009
-- T009 WrongConstructorArity: Some takes 1 arg but pattern has 2.
fn bad -> Int =
    match Some 1
        Some n m -> n
        None     -> 0
