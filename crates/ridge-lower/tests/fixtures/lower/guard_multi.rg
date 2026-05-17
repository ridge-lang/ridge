fn guard_multi (x: Int) (y: Int) -> Int =
    guard (x > 0) else return 0
    guard (y > 0) else return 0
    x + y
