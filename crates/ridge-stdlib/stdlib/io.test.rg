-- Private FFI bridges for std.io test suite.
-- These replicate io.rg FFI declarations in local scope because cross-module
-- calls are unsupported (T17+ deferred).
@ffi("ridge_rt", "print", 1)
fn _print (s: Text) -> Unit

@ffi("ridge_rt", "println", 1)
fn _println (s: Text) -> Unit

@ffi("ridge_rt", "eprintln", 1)
fn _eprintln (s: Text) -> Unit

pub fn test_smoke_io () -> Result Unit Text = Ok ()

-- io.println returns Unit and does not crash.
pub fn io test_println_returns_unit () -> Result Unit Text =
    let _ = _println "test"
    Ok ()

-- io.print (no newline) returns Unit and does not crash.
pub fn io test_print_no_newline () -> Result Unit Text =
    let _ = _print "test"
    Ok ()

-- io.eprintln (stderr) returns Unit and does not crash.
pub fn io test_eprintln_to_stderr () -> Result Unit Text =
    let _ = _eprintln "test"
    Ok ()
