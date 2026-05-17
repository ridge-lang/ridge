-- expect: R004
-- T15 / §5.1 R004: a module may not import itself.
-- The fixture harness places this file at `apps/demo/src/r004_self_import.rg`,
-- so its FQN is `demo.r004_self_import`.  Importing that exact path triggers
-- R004 SelfImport via `module_graph::detect_cycles` step (a → a edge).
import demo.r004_self_import

fn noop = ()
