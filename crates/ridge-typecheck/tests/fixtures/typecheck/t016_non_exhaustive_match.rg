-- expect: T016
-- T016 NonExhaustiveMatch: match on Color doesn't cover Blue.
type Color = Red | Green | Blue
fn f (c: Color) -> Int =
    match c
        Red   -> 1
        Green -> 2
