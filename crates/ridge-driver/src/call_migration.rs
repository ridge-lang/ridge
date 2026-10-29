//! Apply semantic call edits without changing comments or line endings.

use ridge_typecheck::TypedModule;

/// Add explicit invocation markers at resolved zero-parameter message sites.
///
/// The supplied source must parse to the checked module. Compiler byte spans
/// index normalized text; replacements are mapped back to the original bytes.
/// This operation performs no I/O and does not format the surrounding source.
///
/// # Errors
///
/// Rejects stale source, unresolved bare messages and invalid/overlapping edits.
pub fn migrate_message_calls(source: &str, module: &TypedModule) -> Result<String, &'static str> {
    let parsed = ridge_parser::parse_source(source);
    let matches_checked_ast = parsed.module == *module.ast;
    if !parsed.errors.is_empty() || !parsed.lex_errors.is_empty() || !matches_checked_ast {
        return Err("source differs from the checked module; check it again before migrating");
    }
    if module
        .message_call_sites
        .iter()
        .any(|site| site.argument_count == 0 && site.parameter_count.is_none())
    {
        return Err("a bare message's handler could not be resolved; no edits were applied");
    }
    let normalized = ridge_lexer::normalise_line_endings(source);
    let mut edits: Vec<_> = module
        .message_call_sites
        .iter()
        .filter_map(|site| {
            site.replacement()
                .map(|replacement| (site.message, site.handler.as_str(), replacement))
        })
        .collect();
    edits.sort_by_key(|(span, _, _)| span.start);
    let mut previous_end = 0;
    let mut raw = 0;
    let mut offset = 0;
    let mut mapped = Vec::new();
    for (span, handler, replacement) in edits {
        let start = span.start as usize;
        let end = span.end as usize;
        if start < previous_end || normalized.get(start..end) != Some(handler) {
            return Err("call edits overlap or no longer identify the checked handler");
        }
        let start = original_offset(source.as_bytes(), &mut raw, &mut offset, start)
            .ok_or("call edit is outside the source")?;
        let end = original_offset(source.as_bytes(), &mut raw, &mut offset, end)
            .ok_or("call edit is outside the source")?;
        if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err("call edit is not on a UTF-8 boundary");
        }
        previous_end = span.end as usize;
        mapped.push((start..end, replacement));
    }
    let mut output = source.to_owned();
    for (range, replacement) in mapped.into_iter().rev() {
        output.replace_range(range, &replacement);
    }
    Ok(output)
}

fn original_offset(
    source: &[u8],
    raw: &mut usize,
    offset: &mut usize,
    target: usize,
) -> Option<usize> {
    while *offset < target {
        let byte = *source.get(*raw)?;
        *raw += 1;
        if byte == b'\r' && source.get(*raw) == Some(&b'\n') {
            *raw += 1;
        }
        *offset += 1;
    }
    Some(*raw)
}
