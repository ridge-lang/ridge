-- expect: T014
-- D058: an inner `fn` whose declared caps are NOT a subset of the enclosing
-- fn's effective caps fires T014.  Here `outer` declares only `{io}` while
-- `leaky` declares `{fs}` — `{fs} ⊄ {io}`.  Spec §6.7 / D058.
import std.fs as Fs

fn io outer (path: Text) -> Result Text Text =
    fn fs leaky () -> Result Text Text = Fs.readFile path
    leaky ()
