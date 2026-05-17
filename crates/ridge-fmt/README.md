# [TRANSITIONAL] ridge-fmt — Ridge Source Formatter

`ridge-fmt` provides the `format_source` function used by `ridge fmt` to
normalise Ridge source files.

## Algorithm (0.1.0 — Transitional)

The 0.1.0 formatter implements a **trivia-preserving round-trip**:

1. Pre-normalise tabs (each `\t` → two spaces) so tab-indented files are
   accepted without error.
2. Parse the source with trivia preserved via
   `ridge_parser::parse_module_with_trivia`.  Unparseable input is returned
   as `Err(FormatError::FmtSourceUnparseable)` — the formatter never silently
   corrupts a broken file.
3. Walk every source line and apply normalisation rules:
   - Tabs in leading whitespace → 2 spaces each.
   - Trailing whitespace stripped.
   - One space around binary operators (`+`, `-`, `*`, `/`, `==`, `!=`, `<`,
     `<=`, `>`, `>=`, `&&`, `||`, `|>`, `?>`, `++`).
   - CRLF input read transparently; output always LF.
4. Normalise blank lines between top-level declarations:
   - Zero blank lines between consecutive `import` statements.
   - Exactly one blank line between all other consecutive top-level
     declarations.
5. Re-attach line comments: same-line if combined length ≤ 80
   characters, otherwise on the preceding line.
6. Doc-bracket comments (`---…---`) are preserved verbatim.

## Known Limitations (Transitional)

This algorithm does **not**:

- Normalise indentation *within* nested expressions (e.g., a multi-line
  lambda body is emitted as-is beyond the first-level indentation fix).
- Break long lines or reflow trailing arguments.
- Reorder or group `import` statements.

These limitations are intentional for 0.1.0.  The 0.2.0 roadmap upgrades
`ridge fmt` to a **Wadler-Leijen printer-style** formatter, gated behind
`--style printer` with the trivia round-trip remaining the default until
0.3.0.  Early adopters should not develop reliance on the trivia algorithm's
whitespace fidelity — the "one canonical form" guarantee only becomes
unconditional in 0.3.0.

## Error Codes

| Code | Name | Meaning |
|------|------|---------|
| `C101` | `FmtSourceUnparseable` | The input source failed to parse; formatter refuses to emit output to avoid silent corruption. |

## Public API

```rust
pub fn format_source(src: &str) -> Result<String, FormatError>;
```

The CLI layer (`ridge fmt`, implemented in `ridge-cli`) drives this function
and is responsible for reading/writing files, handling `--check` exit codes,
and reporting errors to the user.
