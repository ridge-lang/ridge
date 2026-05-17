-- expect: T012
-- T012 ToTextNotDerivable: string interpolation with a record type
-- that is not in the closed ToText set (Int, Float, Bool, Text, Timestamp).
type Point = { x: Int, y: Int }
fn bad (p: Point) -> Text = $"Point is ${p}"
