// build.rs — ridge-typecheck signature generator (T10).
//
// Emits `${OUT_DIR}/stdlib_signatures.rs` by reading the implementation from
// `src/stdlib_signatures_impl.rs` (the hand-curated Phase 4 signature table).
// The source file `src/stdlib_signatures.rs` delegates to this generated file
// via `include!(concat!(env!("OUT_DIR"), "/stdlib_signatures.rs"))`.
//
// # Cycle-break rationale
//
// ridge-stdlib depends on ridge-typecheck (regular + build-deps), so
// ridge-typecheck cannot depend on ridge-stdlib (even as build-dep) without
// creating a Cargo cycle.  This build script performs its own work without
// depending on ridge-stdlib.  For T10 the generated content is the
// hand-curated Phase 4 table verbatim — future tasks (T12+) will make the
// generation smarter once a cycle-safe codegen crate is available.
//
// T201 / T202 errors: surfaced via eprintln! + process::exit(1) (no panic!
// per §1.3 hard constraint #5).

use std::path::{Path, PathBuf};

fn main() {
    // Re-run whenever the impl source changes.
    println!("cargo:rerun-if-changed=src/stdlib_signatures_impl.rs");

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let impl_path = manifest_dir.join("src").join("stdlib_signatures_impl.rs");

    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| {
        eprintln!("T202 SignatureDrift: OUT_DIR not set");
        std::process::exit(1);
    });
    let out_path = PathBuf::from(&out_dir).join("stdlib_signatures.rs");

    match generate_signatures(&impl_path, &out_path) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn generate_signatures(impl_path: &Path, out_path: &Path) -> Result<(), String> {
    let content = std::fs::read_to_string(impl_path).map_err(|e| {
        format!(
            "T202 SignatureDrift: could not read {}: {e}",
            impl_path.display()
        )
    })?;

    std::fs::write(out_path, content).map_err(|e| {
        format!(
            "T202 SignatureDrift: could not write {}: {e}",
            out_path.display()
        )
    })?;

    Ok(())
}
