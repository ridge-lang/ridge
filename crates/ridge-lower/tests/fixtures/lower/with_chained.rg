type Point = { x: Int, y: Int, z: Int }

fn with_chained (p: Point) -> Point =
    let p1 = p with { x = 1 }
    p1 with { y = 2 }
