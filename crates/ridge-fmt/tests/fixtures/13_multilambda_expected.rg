import std.list as List

fn processAll (xs: List Int) -> List Int =
    xs |> List.map (fn x ->
        x * 2)
