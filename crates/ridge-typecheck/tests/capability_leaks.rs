//! Capability-leak fixture harness for `ridge-typecheck`.
//!
//! Seven fixtures under `tests/fixtures/capability/*.ridge` each exercise one
//! capability rule (Model B, file-private declarations, higher-order
//! functions, and inner functions). Each
//! fixture starts with one of two directives:
//!
//! - `-- expect: T###` — assert this T-code appears among the emitted errors.
//! - `-- expect-clean` — assert *no* T-errors are emitted.
//!
//! The harness mirrors the structure of `tests/errors.rs` but supports the
//! "clean" form so positive (well-typed) capability scenarios are first-class.
//!
//! All fixtures must be present and the suite must pass.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};
use tempfile::TempDir;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/capability");

const REQUIRED_FIXTURES: usize = 18;

// ── Workspace setup helpers ──────────────────────────────────────────────────

fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

fn build_single_module_workspace(stem: &str, src: &str) -> TempDir {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/ridge.toml",
        "[project]\n\
         name = \"demo\"\n\
         version = \"0.1.0\"\n\
         kind = \"library\"\n\
         \n\
         [project.exports]\n\
         public = [\"**\"]\n",
    );
    write_file(td.path(), &format!("apps/demo/src/{stem}.ridge"), src);
    td
}

fn run_typecheck_on_source(stem: &str, src: &str) -> Vec<TypeError> {
    let td = build_single_module_workspace(stem, src);
    let disc = discover_workspace(td.path());
    let ws_graph = disc.graph.expect("fixture workspace must be discovered");
    let resolved = resolve_workspace(ws_graph);
    assert!(
        resolved.parse_errors.is_empty(),
        "{:?}",
        resolved.parse_errors
    );
    let result = typecheck_workspace(&resolved);
    result.errors.into_iter().map(|(_, e)| e).collect()
}

// ── Directive parser ────────────────────────────────────────────────────────

#[derive(Debug)]
enum Expectation {
    /// `-- expect-clean` — no T-errors must be emitted.
    Clean,
    /// `-- expect: T###` — the listed code must appear among the emitted
    /// errors (other codes are ignored).
    Codes(Vec<String>),
}

fn parse_expectation(src: &str) -> Option<Expectation> {
    let mut codes: Vec<String> = Vec::new();
    let mut clean = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("--") {
            break;
        }
        let after_dashes = trimmed.trim_start_matches('-').trim();
        if after_dashes == "expect-clean" {
            clean = true;
            continue;
        }
        if let Some(rest) = after_dashes.strip_prefix("expect:") {
            if let Some(code) = rest.split_whitespace().next() {
                codes.push(code.to_uppercase());
            }
        }
    }
    if clean {
        return Some(Expectation::Clean);
    }
    if !codes.is_empty() {
        return Some(Expectation::Codes(codes));
    }
    None
}

// ── Fixture-driven test ──────────────────────────────────────────────────────

#[test]
fn constants_check_contracts_and_keep_latent_permissions() {
    for initializer in [
        "fn (n: Int) -> Int =\n    Io.println \"effect\"\n    n",
        "perform",
    ] {
        for alias in [false, true] {
            for permitted in [false, true] {
                let source = format!(
                    "import std.io as Io\npub fn io perform (n: Int) -> Int =\n    Io.println \"effect\"\n    n\npub const f: fn io Int -> Int = {initializer}\npub fn {}caller () -> Int =\n{}    {} 7\n",
                    if permitted { "io " } else { "" },
                    if alias { "    let g = f\n" } else { "" },
                    if alias { "g" } else { "f" },
                );
                let errors = run_typecheck_on_source("Main", &source);
                if permitted {
                    assert!(errors.is_empty(), "{source}\n{errors:?}");
                } else {
                    assert!(
                        errors.iter().any(|e| e.code() == "T014"),
                        "{source}\n{errors:?}"
                    );
                }
            }
        }
    }
    for source in [
        "pub const x: Int = \"wrong\"\n",
        "import std.io as Io\npub const f: fn Int -> Int = fn (n: Int) -> Int =\n    Io.println \"effect\"\n    n\n",
    ] {
        let errors = run_typecheck_on_source("Main", source);
        assert!(errors.iter().any(|e| e.code() == "T001"), "{source}\n{errors:?}");
    }
}

#[test]
fn capability_fixtures_pass() {
    let dir = PathBuf::from(FIXTURE_DIR);
    assert!(
        dir.is_dir(),
        "capability fixture directory does not exist: {}",
        dir.display()
    );

    let mut entries: Vec<_> = fs::read_dir(&dir)
        .expect("read fixture dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "ridge"))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    let mut failures: Vec<String> = Vec::new();
    let mut count = 0usize;

    for entry in entries {
        let path = entry.path();
        let stem = path
            .file_stem()
            .expect("fixture stem")
            .to_string_lossy()
            .into_owned();
        let file_name = path
            .file_name()
            .expect("fixture filename")
            .to_string_lossy()
            .into_owned();

        let src = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));

        let Some(expect) = parse_expectation(&src) else {
            failures.push(format!(
                "{file_name}: missing `-- expect: T###` or `-- expect-clean` directive"
            ));
            continue;
        };
        count += 1;

        let errors = run_typecheck_on_source(&stem, &src);
        let actual_codes: Vec<&str> = errors.iter().map(TypeError::code).collect();

        match expect {
            Expectation::Clean => {
                if !errors.is_empty() {
                    failures.push(format!(
                        "{file_name}: expected no T-errors but got {actual_codes:?}"
                    ));
                }
            }
            Expectation::Codes(expected) => {
                for code in &expected {
                    if !errors.iter().any(|e| e.code() == code) {
                        failures.push(format!(
                            "{file_name}: expected {code} but got {actual_codes:?}"
                        ));
                    }
                }
            }
        }
    }

    assert!(
        count >= REQUIRED_FIXTURES,
        "DoD requires {REQUIRED_FIXTURES} capability fixtures; got {count}"
    );
    assert!(
        failures.is_empty(),
        "capability fixture failures:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn lambda_aliases_preserve_independent_callback_permissions() {
    for permitted in [false, true] {
        let source = format!(
            "import std.io as Io\npub fn io effect (n: Int) -> Int =\n    Io.println \"effect\"\n    n\npub fn {}caller () -> Int =\n    let apply = fn f x -> f x\n    let both = fn f g x -> f x + g x\n    let pure = fn n -> n + 1\n    let first = apply pure 1\n    let second = both pure effect first\n    apply pure second\n",
            if permitted { "io " } else { "" },
        );
        let errors = run_typecheck_on_source("Main", &source);
        if permitted {
            assert!(errors.is_empty(), "{source}\n{errors:?}");
        } else {
            assert!(
                errors.iter().any(|e| e.code() == "T014"),
                "{source}\n{errors:?}"
            );
        }
    }
}

#[test]
fn creating_effectful_function_values_does_not_execute_them() {
    let source = "import std.io as Io\npub const printer: fn Text -> Unit = Io.println\npub fn value () -> fn io Int -> Int =\n    fn (n: Int) -> Int =\n        Io.println \"effect\"\n        n\n";
    let errors = run_typecheck_on_source("Main", source);
    assert!(errors.iter().any(|e| e.code() == "T001"), "{errors:?}");
    let valid = source.replace("printer: fn Text", "printer: fn io Text");
    let errors = run_typecheck_on_source("Main", &valid);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn constant_imports_keep_capabilities_and_pure_initialization() {
    for caller in ["", "io "] {
        let source = format!("import demo.Defs as D\npub fn {caller}call () -> Int =\n    let f = D.effect\n    f 7\n");
        let td = build_single_module_workspace("Main", &source);
        write_file(td.path(), "apps/demo/src/Defs.ridge", "import std.io as Io\npub const effect: fn io Int -> Int = fn (n: Int) -> Int =\n    Io.println \"effect\"\n    n\n");
        let graph = discover_workspace(td.path()).graph.expect("workspace");
        let resolved = resolve_workspace(graph);
        assert!(
            resolved.parse_errors.is_empty(),
            "{:?}",
            resolved.parse_errors
        );
        let result = typecheck_workspace(&resolved);
        if caller.is_empty() {
            assert!(
                result.errors.iter().any(|(_, e)| e.code() == "T014"),
                "{:?}",
                result.errors
            );
        } else {
            assert!(result.errors.is_empty(), "{:?}", result.errors);
        }
    }
    let source = "import std.io as Io\npub fn io effect () -> Int =\n    Io.println \"effect\"\n    7\npub const eager: Int = effect ()\n";
    let errors = run_typecheck_on_source("Main", source);
    assert!(errors.iter().any(|e| e.code() == "T014"), "{errors:?}");
}

#[test]
fn private_helpers_and_forwarders_keep_their_inferred_effects() {
    for prefix in ["", "io "] {
        let source = format!("import std.io as Io\nfn _effect (n: Int) -> Int =\n    Io.println \"effect\"\n    n\nfn _apply f x = f x\npub const effect: fn io Int -> Int = _effect\npub fn {prefix}caller () -> Int = _apply effect 7\n");
        let errors = run_typecheck_on_source("Main", &source);
        if prefix.is_empty() {
            assert!(
                errors.iter().any(|e| e.code() == "T014"),
                "{source}\n{errors:?}"
            );
        } else {
            assert!(errors.is_empty(), "{source}\n{errors:?}");
        }
    }
    let source = "import std.io as Io\nfn _even (n: Int) -> Bool =\n    if n == 0 then True else _odd (n - 1)\nfn _odd (n: Int) -> Bool =\n    Io.println \"effect\"\n    if n == 0 then False else _even (n - 1)\npub fn caller () -> Bool = _even 2\n";
    let errors = run_typecheck_on_source("Main", source);
    assert!(errors.iter().any(|e| e.code() == "T014"), "{errors:?}");
    let errors =
        run_typecheck_on_source("Main", &source.replace("pub fn caller", "pub fn io caller"));
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn captured_callback_rows_cannot_be_generalized_away() {
    for prefix in ["", "io "] {
        let source = format!("import std.io as Io\npub fn io effect (n: Int) -> Int =\n    Io.println \"effect\"\n    n\npub fn {prefix}caller () -> Int =\n    let combine = fn f g ->\n        let run = fn x -> f x + g x\n        run\n    let pure = fn n -> n + 1\n    let saved = combine pure effect\n    saved 7\n");
        let errors = run_typecheck_on_source("Main", &source);
        if prefix.is_empty() {
            assert!(
                errors.iter().any(|e| e.code() == "T014"),
                "{source}\n{errors:?}"
            );
        } else {
            assert!(errors.is_empty(), "{source}\n{errors:?}");
        }
    }
}

#[test]
fn constant_capability_contracts_allow_pure_implementations_and_retain_fs() {
    let errors = run_typecheck_on_source(
        "Main",
        "pub const f: fn io Int -> Int = fn (n: Int) -> Int = n\npub fn io value () -> Int = f 7\n",
    );
    assert!(errors.is_empty(), "{errors:?}");
    for prefix in ["", "fs "] {
        let source = format!("pub fn fs perform (n: Int) -> Int = n\npub const f: fn fs Int -> Int = perform\npub fn {prefix}value () -> Int =\n    let saved = f\n    saved 7\n");
        let errors = run_typecheck_on_source("Main", &source);
        if prefix.is_empty() {
            assert!(errors.iter().any(|e| e.code() == "T014"), "{errors:?}");
        } else {
            assert!(errors.is_empty(), "{errors:?}");
        }
    }
}

#[test]
fn callback_allowances_accept_pure_values_without_erasing_required_permissions() {
    let source = "pub fn io apply f n = f n\npub fn io value () -> Int =\n    let pure = fn (n: Int) -> Int = n + 1\n    apply pure 6\n";
    let errors = run_typecheck_on_source("Main", source);
    assert!(errors.is_empty(), "{errors:?}");
    let source = "import std.io as Io\npub fn apply (f: fn Int -> Int) -> Int = f 7\npub fn io value () -> Int =\n    apply (fn (n: Int) -> Int =\n        Io.println \"effect\"\n        n)\n";
    let errors = run_typecheck_on_source("Main", source);
    assert!(errors.iter().any(|e| e.code() == "T002"), "{errors:?}");
}

#[test]
fn local_annotations_and_returned_values_keep_the_declared_contract() {
    for prefix in ["", "io "] {
        let source = format!("pub fn make () -> fn io Int -> Int = fn (n: Int) -> Int = n\npub fn {prefix}caller () -> Int =\n    let f: fn io Int -> Int = fn (n: Int) -> Int = n\n    var g: fn io Int -> Int = make ()\n    g <- f\n    g 7\n");
        let errors = run_typecheck_on_source("Main", &source);
        if prefix.is_empty() {
            assert!(
                errors.iter().any(|e| e.code() == "T014"),
                "{source}\n{errors:?}"
            );
        } else {
            assert!(errors.is_empty(), "{source}\n{errors:?}");
        }
    }
}

#[test]
fn inner_helpers_retain_invoked_and_captured_callback_rows() {
    for form in ["inner", "captured", "record"] {
        for allowed in [false, true] {
            let mut source = String::from("import std.io as Io\n");
            if form == "record" {
                source.push_str("fn _invoke record -> Unit = record.run ()\n");
            }
            source.push_str(if allowed {
                "pub fn io caller () -> Unit =\n"
            } else {
                "pub fn caller () -> Unit =\n"
            });
            source.push_str(match form {
                "inner" => "    fn invoke f -> Unit = f \"effect\"\n    invoke Io.println\n",
                "captured" => "    let wrap = fn f ->\n        fn invoke (_u: Unit) -> Unit = f \"effect\"\n        invoke\n    let saved = wrap Io.println\n    saved ()\n",
                _ => "    _invoke ({ run = fn (_u: Unit) -> Unit = Io.println \"effect\" })\n",
            });
            let errors = run_typecheck_on_source("Main", &source);
            if allowed {
                assert!(errors.is_empty(), "{source}\n{errors:?}");
            } else {
                assert!(
                    errors.iter().any(|e| e.code() == "T014"),
                    "{source}\n{errors:?}"
                );
            }
        }
    }
}

#[test]
fn http_listener_requires_its_base_and_callback_permissions() {
    for listener in ["listen", "listenWithHeaders"] {
        let response = if listener == "listen" {
            "Http.respond 200 \"ok\""
        } else {
            "Http.withoutHeaders (Http.respond 200 \"ok\")"
        };
        for (caps, valid) in [("net io ", true), ("net ", false), ("io ", false)] {
            let source = format!("import std.net.http as Http\nimport std.io as Io\npub fn {caps}serve () -> Unit =\n    Http.{listener} 18086 (fn _ ->\n        Io.println \"effect\"\n        {response})\n");
            let errors = run_typecheck_on_source("Main", &source);
            if valid {
                assert!(errors.is_empty(), "{source}\n{errors:?}");
            } else {
                assert!(
                    errors.iter().any(|e| e.code() == "T014"),
                    "{source}\n{errors:?}"
                );
            }
        }
        let source = format!("import std.net.http as Http\npub fn net serve () -> Unit = Http.{listener} 18086 (fn _ -> 7)\n");
        let errors = run_typecheck_on_source("Main", &source);
        assert!(
            errors.iter().any(|e| e.code() == "T002"),
            "{source}\n{errors:?}"
        );
    }
}

#[test]
fn retry_keeps_callback_effects_alongside_time_and_random() {
    for prefix in ["time random ", "time random io "] {
        let source = format!("import std.repo as Repo (RetryPolicy)\nimport std.io as Io\npub fn {prefix}run (policy: RetryPolicy) -> Result Int Error =\n    Repo.retryWhen policy (fn _ -> True) (fn _ ->\n        Io.println \"effect\"\n        Ok 7)\n");
        let errors = run_typecheck_on_source("Main", &source);
        if prefix.contains("io") {
            assert!(errors.is_empty(), "{source}\n{errors:?}");
        } else {
            assert!(
                errors.iter().any(|e| e.code() == "T014"),
                "{source}\n{errors:?}"
            );
        }
    }
}
