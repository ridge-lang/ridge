//! Fixtures shared by the crate's unit tests.

use ridge_types::{BuiltinTyCons, TyConArena};
use std::sync::OnceLock;

/// The built-in `TyConId` handles, allocated once per test binary.
///
/// A unit test builds a [`crate::ctx::LowerCtx`] without a `TypedWorkspace`,
/// but it still needs real handles: `LowerCtx::builtins` is what the code
/// under test reads to name `Int`, and a test that answered it with a
/// hand-written id would be checking the mirror rather than the code. Running
/// the same allocation the compiler runs means a test keeps testing the same
/// thing after the arena re-orders.
pub fn builtins() -> &'static BuiltinTyCons {
    static TABLE: OnceLock<BuiltinTyCons> = OnceLock::new();
    TABLE.get_or_init(|| BuiltinTyCons::allocate(&mut TyConArena::default()))
}
