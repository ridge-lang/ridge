-- Phase 4.5 T6 fixture: lambda body typing via infer_expr write-back.
-- The fn body contains a lambda expression; T3's infer_expr shim stamps the
-- lambda's type in node_types, confirming that nested expressions are covered.

fn applyTwice (f: Int -> Int) (x: Int) -> Int =
  f (f x)

fn doubler () -> Int -> Int =
  \n -> n + n
