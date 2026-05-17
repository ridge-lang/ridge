type Point = { x: Int, y: Int }

fn origin () -> Point = Point { x = 0, y = 0 }

fn moveRight (p: Point) -> Point = p with { x = p.x+1 }
