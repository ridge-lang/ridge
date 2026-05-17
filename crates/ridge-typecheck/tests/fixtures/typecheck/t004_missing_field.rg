-- expect: T004
-- T004 MissingField: Point requires x and y but only x is provided.
type Point = { x: Int, y: Int }
fn bad -> Point = Point { x = 1 }
