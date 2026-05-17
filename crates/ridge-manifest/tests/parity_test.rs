//! Parity guard-rail — `ridge-manifest` vs `ridge-resolve::manifest`.
//!
//! While `ridge-resolve` retains its own copy of the manifest parser (the
//! consumption-side wiring is deferred), this test parses the same fixtures with
//! both parsers and asserts structural equivalence on every observable field.
//!
//! Why this exists: duplication of validation logic is a classic bypass
//! vector — if a future fix lands in one parser but not the other, the LSP
//! and the compiler tell the user different stories about the same
//! `ridge.toml`.  The parity test fails CI the moment the two diverge.
//!
//! When T2 lands and `ridge-resolve` re-exports from `ridge-manifest`, this
//! test becomes redundant and is removed alongside the `ridge-resolve`
//! `[dev-dependencies]` line in `ridge-manifest/Cargo.toml`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn
)]

use std::path::PathBuf;

use ridge_manifest as rm;
use ridge_resolve::{manifest as rr, ProjectId};

const FIXTURE_DIR: &str = "tests/fixtures";

fn load(name: &str) -> (String, PathBuf) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURE_DIR)
        .join(name);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"));
    (src, path)
}

fn project_kind_str(k: rm::ProjectKind) -> &'static str {
    match k {
        rm::ProjectKind::Library => "library",
        rm::ProjectKind::App => "app",
        rm::ProjectKind::Service => "service",
        rm::ProjectKind::Test => "test",
    }
}

fn rr_project_kind_str(k: rr::ProjectKind) -> &'static str {
    match k {
        rr::ProjectKind::Library => "library",
        rr::ProjectKind::App => "app",
        rr::ProjectKind::Service => "service",
        rr::ProjectKind::Test => "test",
    }
}

#[test]
fn parity_workspace_happy_fixtures() {
    let fixtures = [
        "ws_single_project.toml",
        "ws_multi_member.toml",
        "ws_with_forbid_rules.toml",
        "ws_with_deps.toml",
        "ws_with_capabilities.toml",
    ];

    for f in fixtures {
        let (src, path) = load(f);
        let mfst = rm::parse_workspace(&src, &path)
            .unwrap_or_else(|e| panic!("ridge-manifest failed on {f}: {e:?}"));
        let resv = rr::parse_workspace_manifest(&src, &path)
            .unwrap_or_else(|e| panic!("ridge-resolve failed on {f}: {e:?}"));

        assert_eq!(mfst.name, resv.name, "name mismatch on {f}");
        assert_eq!(mfst.version, resv.version, "version mismatch on {f}");
        assert_eq!(
            mfst.members_globs, resv.members_globs,
            "members_globs mismatch on {f}"
        );
        assert_eq!(
            mfst.dependencies.len(),
            resv.dependencies.len(),
            "dependencies len mismatch on {f}"
        );
        assert_eq!(
            mfst.forbid_rules.len(),
            resv.forbid_rules.len(),
            "forbid_rules len mismatch on {f}"
        );
        assert_eq!(
            mfst.capabilities_deny, resv.capabilities_deny,
            "capabilities_deny mismatch on {f}"
        );
        assert_eq!(
            mfst.source_path, resv.source_path,
            "source_path mismatch on {f}"
        );
    }
}

#[test]
fn parity_project_happy_fixtures() {
    let fixtures = [
        "proj_library.toml",
        "proj_app.toml",
        "proj_service.toml",
        "proj_test.toml",
        "proj_with_exports.toml",
    ];

    for f in fixtures {
        let (src, path) = load(f);
        let mfst = rm::parse_project(&src, &path)
            .unwrap_or_else(|e| panic!("ridge-manifest failed on {f}: {e:?}"));
        let resv = rr::parse_project_manifest(&src, &path, ProjectId(0))
            .unwrap_or_else(|e| panic!("ridge-resolve failed on {f}: {e:?}"));

        assert_eq!(mfst.name, resv.name, "name mismatch on {f}");
        assert_eq!(mfst.version, resv.version, "version mismatch on {f}");
        assert_eq!(
            project_kind_str(mfst.kind),
            rr_project_kind_str(resv.kind),
            "kind mismatch on {f}"
        );
        // Note: `ridge_resolve::Project` validates `entry` (rejects missing
        // entry on App/Service kinds) but does not store it.  `ridge-manifest`
        // both validates AND stores it.  Parity here means: both parsers
        // accept the same fixtures (already asserted above by `unwrap`).
        assert_eq!(
            mfst.manifest_path, resv.manifest_path,
            "manifest_path mismatch on {f}"
        );
        assert_eq!(mfst.src_root, resv.src_root, "src_root mismatch on {f}");
        assert_eq!(
            mfst.exports_public.len(),
            resv.exports_public.len(),
            "exports_public len mismatch on {f}"
        );
        assert_eq!(
            mfst.exports_internal.len(),
            resv.exports_internal.len(),
            "exports_internal len mismatch on {f}"
        );
        assert_eq!(
            mfst.dependencies.len(),
            resv.dependencies.len(),
            "dependencies len mismatch on {f}"
        );
        assert_eq!(
            mfst.capabilities_allow, resv.capabilities_allow,
            "capabilities_allow mismatch on {f}"
        );
        assert_eq!(
            mfst.capabilities_deny, resv.capabilities_deny,
            "capabilities_deny mismatch on {f}"
        );
    }
}
