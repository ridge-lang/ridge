fn safe_head (xs: List Int) -> Option Int =
    None

fn propagate_option (xs: List Int) -> Option Int =
    let h = safe_head xs ?
    Some (h + 1)
