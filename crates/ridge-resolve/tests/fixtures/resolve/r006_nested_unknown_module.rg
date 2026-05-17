-- expect: R006
-- T15 / §5.1 R006 (variant 2): a multi-segment import path whose root
-- segment does not match any workspace project or stdlib module triggers R006.
import alpha.beta.gamma as G

fn noop = ()
