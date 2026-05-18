-- expect: R021
-- T15 / §5.1 R021: actor `Counter` has a state field with no default and no
-- `init` block, so it is not constructible — emit R021
-- ActorStateMissingDefaultOrInit anchored at the actor's name span.
actor Counter =
    state count: Int

    on inc () -> Unit =
        ()
