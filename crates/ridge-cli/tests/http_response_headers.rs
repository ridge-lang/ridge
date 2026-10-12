//! Response metadata remains typed, opaque, and capability checked.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use assert_cmd::Command;
use common::make_workspace;

fn check(source: &str) -> std::process::Output {
    let workspace = make_workspace("Headers", source);
    Command::cargo_bin("ridge")
        .unwrap()
        .arg("check")
        .current_dir(&workspace.path)
        .output()
        .unwrap()
}

#[test]
fn header_values_are_text_and_metadata_cannot_be_forged() {
    let valid = check("import std.net.http as Http (Response, ResponseWithHeaders)\n\npub fn wrap (r: Response) -> Result ResponseWithHeaders Error = Http.withHeaders [(\"Location\", \"/target\")] r\n");
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    let wrong = check("import std.net.http as Http (Response, ResponseWithHeaders)\n\npub fn wrap (r: Response) -> Result ResponseWithHeaders Error = Http.withHeaders [(\"Location\", 42)] r\n");
    assert!(!wrong.status.success());
    assert!(
        String::from_utf8_lossy(&wrong.stderr).contains("expected Text, got Int"),
        "{}",
        String::from_utf8_lossy(&wrong.stderr)
    );
    let forged = check("import std.net.http as Http (Response, ResponseWithHeaders)\n\npub fn forged () -> ResponseWithHeaders = ResponseWithHeaders { status = 302, body = \"bad\", headers = [] }\n");
    assert!(!forged.status.success());
    assert!(String::from_utf8_lossy(&forged.stderr).contains("opaque"));
}

#[test]
fn listener_charges_callback_capabilities_to_the_caller() {
    let source = "import std.net.http as Http (Response, ResponseWithHeaders)\nimport std.io as Io\n\npub fn net io serve () -> Unit =\n    Http.listenWithHeaders 18086 (fn _ ->\n        Io.println \"hit\"\n        Http.withoutHeaders (Http.respond 200 \"ok\"))\n";
    let valid = check(source);
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    let missing = check(&source.replace("fn net io", "fn net"));
    assert!(!missing.status.success(), "callback IO must be charged");
    assert!(String::from_utf8_lossy(&missing.stderr).contains("io"));
}
