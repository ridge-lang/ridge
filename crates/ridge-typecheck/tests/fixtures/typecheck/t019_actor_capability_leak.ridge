-- expect: T019
-- T019 ActorCapabilityLeak: init declares {io} but actor_caps (union of handler
-- caps = {fs}) does not include {io}.
-- The init block leaks {io} outside the actor's effective capability boundary.
import std.io as Io
actor Logger =
    state log: Text = ""
    init io (greeting: Text) =
        Io.println greeting
        log <- greeting
    on fs save (v: Text) =
        log <- v
