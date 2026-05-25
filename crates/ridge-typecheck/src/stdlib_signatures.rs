//! Stdlib signature table: maps `(StdlibModuleId, symbol-name)` to `Scheme`.
//!
//! This file is a generated-output gateway (T10).
//
// The stdlib signature implementation is produced at build time by
// `crates/ridge-typecheck/build.rs`, which reads
// `src/stdlib_signatures_impl.rs` (the hand-curated Phase 4 table) and
// copies it to `${OUT_DIR}/stdlib_signatures.rs`.
//
// Future tasks (T12+) will make the generation smarter (auto-derived from
// the real stdlib `.ridge` sources).  For now the source of truth is
// `stdlib_signatures_impl.rs`.
//
// Do not add code to this file directly; edit `stdlib_signatures_impl.rs`.

// The signature table is one match arm per stdlib symbol. Different symbols
// may carry identical type schemes (for example, several `Text -> Bool`
// predicates), so the lint that flags arms with identical bodies would
// suggest merging unrelated entries. Each arm intentionally maps a single
// stdlib symbol; suppress the lint for the included table.
#![allow(clippy::match_same_arms)]

include!(concat!(env!("OUT_DIR"), "/stdlib_signatures.rs"));
