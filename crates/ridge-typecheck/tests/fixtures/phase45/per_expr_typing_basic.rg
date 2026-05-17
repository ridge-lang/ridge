-- Phase 4.5 T6 fixture: per-expression type recording.
-- This fn contains multiple expression positions (literal, ident-ref, call)
-- so that the T3 node_types write-back can be verified: node_types_populated >= 5.

fn double (n: Int) -> Int =
  n + n

fn greet (name: Text) -> Text =
  "Hello " ++ name
