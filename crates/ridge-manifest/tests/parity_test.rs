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
//! Once `ridge-resolve` re-exports from `ridge-manifest`, this test becomes
//! redundant and is removed alongside the `ridge-resolve` `[dev-dependencies]`
//! line in `ridge-manifest/Cargo.toml`.
//!
//! Both the happy path and the error path are covered. The error path matters
//! more: agreeing on which manifests to accept says nothing about agreeing on
//! *why* they were rejected, and a reader who reports a diagnostic code is
//! reporting whichever parser happened to run.

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

// ── Error-path parity ─────────────────────────────────────────────────────────

/// A manifest both parsers must reject, and the code they must agree on.
///
/// The expected code is asserted as well as the agreement, and both assertions
/// earn their place. Agreement alone is not coverage: every workspace case has
/// to clear the required-field step (`name`, `version`, `members`) before it
/// reaches the check it is written for, and a case that forgets one dies at
/// `M006` instead — where both parsers agree perfectly, on the wrong error. A
/// suite asserting only agreement would have exercised `M006` ten times and
/// reported full coverage.
struct ErrorCase {
    /// What the source does wrong, for the failure message.
    trigger: &'static str,
    /// The code both parsers are expected to report.
    code: &'static str,
    /// The manifest source.
    toml: &'static str,
}

/// Workspace manifests, one per code `parse_workspace` can report.
const WORKSPACE_ERRORS: &[ErrorCase] = &[
    ErrorCase {
        trigger: "a value is missing after `=`",
        code: "M001",
        toml: "[workspace]\nname =\n",
    },
    ErrorCase {
        trigger: "there is no `[workspace]` table",
        code: "M002",
        toml: "[project]\nname = \"p\"\nversion = \"0.1.0\"\n",
    },
    ErrorCase {
        trigger: "a member glob does not compile",
        code: "M005",
        toml: "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\nmembers = [\"apps/[\"]\n",
    },
    ErrorCase {
        trigger: "`name` is absent",
        code: "M006",
        toml: "[workspace]\nversion = \"0.1.0\"\nmembers = []\n",
    },
    ErrorCase {
        trigger: "a forbid rule declares `from` without `to`",
        code: "M008",
        toml: "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\nmembers = []\n\n[workspace.rules]\nforbid = [{ from = \"a.**\" }]\n",
    },
    ErrorCase {
        trigger: "a dependency names no shape at all",
        code: "M009",
        toml: "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\nmembers = []\n\n[workspace.dependencies]\nfoo = {}\n",
    },
    ErrorCase {
        trigger: "a denied capability is not a capability",
        code: "M011",
        toml: "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\nmembers = []\n\n[workspace.capabilities]\ndeny = [\"telepathy\"]\n",
    },
    ErrorCase {
        trigger: "a git dependency pins both a tag and a branch",
        code: "M016",
        toml: "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\nmembers = []\n\n[workspace.dependencies]\nfoo = { git = \"https://example.invalid/foo.git\", tag = \"v1\", branch = \"main\" }\n",
    },
    ErrorCase {
        trigger: "a dependency comes from hex",
        code: "M018",
        toml: "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\nmembers = []\n\n[workspace.dependencies]\nfoo = { hex = \"1.0\" }\n",
    },
    ErrorCase {
        trigger: "`members` is misspelled",
        code: "M019",
        toml: "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\nmembrs = []\n",
    },
];

/// Project manifests, one per code `parse_project` can report.
const PROJECT_ERRORS: &[ErrorCase] = &[
    ErrorCase {
        trigger: "a value is missing after `=`",
        code: "M001",
        toml: "[project]\nname =\n",
    },
    ErrorCase {
        trigger: "there is no `[project]` table",
        code: "M003",
        toml: "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\n",
    },
    ErrorCase {
        trigger: "`version` is absent",
        code: "M006",
        toml: "[project]\nname = \"p\"\nkind = \"library\"\n",
    },
    ErrorCase {
        trigger: "`kind` is not one of the four kinds",
        code: "M007",
        toml: "[project]\nname = \"p\"\nversion = \"0.1.0\"\nkind = \"libary\"\n",
    },
    ErrorCase {
        trigger: "a dependency names no shape at all",
        code: "M009",
        toml: "[project]\nname = \"p\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[dependencies]\nfoo = {}\n",
    },
    ErrorCase {
        trigger: "an export pattern does not compile",
        code: "M014",
        toml: "[project]\nname = \"p\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"[\"]\n",
    },
    ErrorCase {
        trigger: "a dependency comes from hex",
        code: "M018",
        toml: "[project]\nname = \"p\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[dependencies]\nfoo = { hex = \"1.0\" }\n",
    },
    ErrorCase {
        trigger: "`kind` is misspelled",
        code: "M019",
        toml: "[project]\nname = \"p\"\nversion = \"0.1.0\"\nkindd = \"library\"\n",
    },
];

/// Every code the two parsers can report from a manifest source. A code missing
/// from this list is a code no parity case exercises, which is how the two
/// copies drifted unnoticed in the first place.
const COVERED: &[&str] = &[
    "M001", "M002", "M003", "M005", "M006", "M007", "M008", "M009", "M011", "M014", "M016", "M018",
    "M019",
];

#[test]
fn parity_workspace_error_cases() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic.toml");

    for case in WORKSPACE_ERRORS {
        let mfst = rm::parse_workspace(case.toml, &path);
        let resv = rr::parse_workspace_manifest(case.toml, &path);

        let m = mfst.as_ref().err().map(rm::ManifestError::code);
        let r = resv.as_ref().err().map(ridge_resolve::ManifestError::code);

        assert_eq!(
            m, r,
            "the two parsers disagree when {}: ridge-manifest says {m:?}, \
             ridge-resolve says {r:?}",
            case.trigger
        );
        assert_eq!(
            m,
            Some(case.code),
            "expected {} when {}",
            case.code,
            case.trigger
        );
    }
}

#[test]
fn parity_project_error_cases() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic.toml");

    for case in PROJECT_ERRORS {
        let mfst = rm::parse_project(case.toml, &path);
        let resv = rr::parse_project_manifest(case.toml, &path, ProjectId(0));

        let m = mfst.as_ref().err().map(rm::ManifestError::code);
        let r = resv.as_ref().err().map(ridge_resolve::ManifestError::code);

        assert_eq!(
            m, r,
            "the two parsers disagree when {}: ridge-manifest says {m:?}, \
             ridge-resolve says {r:?}",
            case.trigger
        );
        assert_eq!(
            m,
            Some(case.code),
            "expected {} when {}",
            case.code,
            case.trigger
        );
    }
}

/// Every code in `COVERED` is exercised by at least one case, so the list
/// cannot claim coverage it does not have.
#[test]
fn every_covered_code_has_a_case() {
    for code in COVERED {
        let hit = WORKSPACE_ERRORS
            .iter()
            .chain(PROJECT_ERRORS)
            .any(|c| c.code == *code);
        assert!(hit, "{code} is listed as covered but has no case");
    }
}
