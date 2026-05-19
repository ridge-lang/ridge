-- expect: R008
-- T15 / §5.1 R008 (variant 2): `std.map` is a recognised stdlib module but
-- `nonExistentKey` is not one of its exports, so R008 UnresolvedImportItem
-- fires on the named-import item.
import std.map (nonExistentKey)

fn noop = ()
