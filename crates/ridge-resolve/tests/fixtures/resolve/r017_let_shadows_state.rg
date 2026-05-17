-- expect: R017
-- T11 / OQ-R005: `var count = ...` inside an actor handler shadows the
-- enclosing actor's state field `count` = R017 StateFieldShadowedByLocal
-- (warn-level, not a hard error).
actor Counter =
    state count: Int = 0
    on inc =
        var count = 5
        count
