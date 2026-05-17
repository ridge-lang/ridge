-- expect: R017
-- T11 / OQ-R005: a handler parameter named identically to a state field
-- shadows that state field = R017 StateFieldShadowedByLocal (warn-level).
actor Counter =
    state count: Int = 0
    on set (count: Int) =
        count
