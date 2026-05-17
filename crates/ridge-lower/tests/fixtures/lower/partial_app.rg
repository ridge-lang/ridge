import std.list as List

fn add (x: Int) (y: Int) -> Int =
    x + y

fn add5 -> Int -> Int =
    add 5

fn map_add (n: Int) (xs: List Int) -> List Int =
    List.map (add n) xs
