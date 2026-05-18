fn inner_fn_basic (xs: List Int) -> Int =
    fn sum (acc: Int) (rest: List Int) -> Int =
        match rest
            []      -> acc
            x :: tl -> sum (acc + x) tl
    sum 0 xs
