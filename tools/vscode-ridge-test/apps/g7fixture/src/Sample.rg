-- tools/vscode-ridge-test/apps/g7fixture/src/Sample.rg
--
-- Manual VS Code diagnostics fixture.  Open this file in VS Code with the
-- Ridge extension installed and verify three diagnostics appear in the
-- Problems panel within ~250 ms.
--
-- DIAGNOSTIC #1 — R013 ForbidViolation
-- ------------------------------------
-- The workspace `forbid` rule (`g7fixture.** -> std.fs`) blocks this import.
-- Expected diagnostic on the line below: R013 ForbidViolation pointing at
-- `std.fs`.
import std.fs as Fs

-- DIAGNOSTIC #2 — R016 CapabilityNotAllowed
-- -----------------------------------------
-- The project manifest declares `capabilities.allow = []`, so any function
-- that lists a capability triggers R016.  `fn io needs_io () -> Int = 42`
-- declares the `io` capability and is therefore rejected.
-- Expected diagnostic on the line below: R016 CapabilityNotAllowed (cap=io).
pub fn io needs_io () -> Int = 42

-- DIAGNOSTIC #3 — T001 TypeMismatch
-- --------------------------------
-- `Int + Text` is a type-mismatch on the second operand of `+`.
-- Expected diagnostic on the line below: T001 TypeMismatch (Int / Text).
pub fn bad_add (a : Int) (b : Int) -> Int = a + "hello"
