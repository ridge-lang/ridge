-- expect-clean
-- D058: an inner `fn` whose declared caps are a subset of the enclosing fn's
-- declared caps typechecks cleanly.  Here `inner` declares `{io}` and the
-- outer declares `{env, fs, io}` — `{io} ⊆ {env, fs, io}`.  Spec §6.7.
import std.io as Io

fn env fs io outer (msg: Text) -> Unit =
    fn io inner () -> Unit = Io.println msg
    inner ()
