//! Formatting must not remove or rewrite an executable script's interpreter.
#![allow(clippy::unwrap_used)]
use ridge_fmt::format_source;

#[test]
fn interpreter_directive_preserves_tabs() {
    let source = "#!/usr/bin/env\tridge\npub fn main -> Int = 0\n";
    assert_eq!(format_source(source).unwrap(), source);
}

#[test]
fn preserves_shebang_and_formats_the_program_idempotently() {
    let source = "#!/usr/bin/env -S ridge run\n\npub fn answer -> Int = 42    \n";
    let expected = "#!/usr/bin/env -S ridge run\n\npub fn answer -> Int = 42\n";
    let formatted = format_source(source).unwrap();
    assert_eq!(formatted, expected);
    assert_eq!(format_source(&formatted).unwrap(), expected);
}

#[test]
fn preserves_shebang_without_program_and_normalizes_line_endings() {
    assert_eq!(
        format_source("#!/usr/bin/env ridge\r\n").unwrap(),
        "#!/usr/bin/env ridge\n"
    );
}
