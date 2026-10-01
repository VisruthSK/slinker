use crate::syntax::facts::StaticArg;
use crate::syntax::source::{SourceId, Span, TextRange};
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub(super) struct RawArgument {
    pub(super) name: Option<String>,
    pub(super) value: TextRange,
    pub(super) static_arg: Option<StaticArg>,
}

#[derive(Debug, Clone)]
pub(super) struct RawCall {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) args: Vec<RawArgument>,
}

#[derive(Debug, Clone)]
pub(super) struct FunctionRegion {
    pub(super) function_start: usize,
    pub(super) formals: TextRange,
    pub(super) body: TextRange,
    pub(super) parameters: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ForRegion {
    pub(super) variable: String,
    pub(super) variable_start: usize,
    pub(super) body: TextRange,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct IfRegion {
    pub(super) if_start: usize,
    pub(super) condition: TextRange,
    pub(super) then_branch: TextRange,
    pub(super) else_branch: Option<TextRange>,
}

pub(super) fn strip_outer_parentheses(mut value: &str) -> &str {
    loop {
        if !value.starts_with('(') || !value.ends_with(')') {
            return value;
        }
        let Some(close) = matching_delimiter(value, 0) else {
            return value;
        };
        if close + 1 != value.len() {
            return value;
        }
        value = &value[1..value.len() - 1];
    }
}

pub(super) fn split_top_level_operator<'a>(
    value: &'a str,
    operator: &str,
) -> Option<(&'a str, &'a str)> {
    let bytes = value.as_bytes();
    let operator_bytes = operator.as_bytes();
    let mut cursor = 0usize;
    let mut depth = 0usize;
    let mut quote = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'(' => {
                depth += 1;
                cursor += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                cursor += 1;
            }
            _ if depth == 0
                && cursor + operator_bytes.len() <= bytes.len()
                && &bytes[cursor..cursor + operator_bytes.len()] == operator_bytes =>
            {
                return Some((&value[..cursor], &value[cursor + operator_bytes.len()..]));
            }
            _ => cursor += 1,
        }
    }
    None
}

pub(super) fn contains_call_named(text: &str, name: &str) -> bool {
    let mut offset = 0;
    while let Some(relative) = text[offset..].find(name) {
        let start = offset + relative;
        let end = start + name.len();
        if word_boundary_before(text, start)
            && word_boundary_after(text, end)
            && text.as_bytes().get(skip_trivia(text, end)).copied() == Some(b'(')
        {
            return true;
        }
        offset = end;
        if offset >= text.len() {
            break;
        }
    }
    false
}

pub(super) fn last_top_level_expression(
    text: &str,
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    let start = skip_trivia_bounded(text, start, end);
    let (mut cursor, limit) = if text.as_bytes().get(start).copied() == Some(b'{') {
        let close = matching_delimiter(text, start)?;
        (start + 1, close.min(end))
    } else {
        (start, end)
    };

    let mut last = None;
    while cursor < limit {
        cursor = skip_trivia_bounded(text, cursor, limit);
        while cursor < limit && text.as_bytes()[cursor] == b';' {
            cursor += 1;
            cursor = skip_trivia_bounded(text, cursor, limit);
        }
        if cursor >= limit {
            break;
        }
        let expression_start = cursor;
        let expression_end = expression_end(text, expression_start).min(limit);
        if expression_end <= expression_start {
            break;
        }
        last = Some((expression_start, expression_end));
        cursor = expression_end;
        while cursor < limit && matches!(text.as_bytes()[cursor], b';' | b'\n' | b'\r') {
            cursor += 1;
        }
    }
    last
}

pub(super) fn identifier_occurs_before(text: &str, name: &str, end: usize) -> bool {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;
    while cursor < end.min(bytes.len()) {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(end);
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, end),
            _ if text
                .get(cursor..end)
                .is_some_and(|rest| rest.starts_with(name))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + name.len()) =>
            {
                return true;
            }
            _ => cursor += text[cursor..].chars().next().map_or(1, char::len_utf8),
        }
    }
    false
}

pub(super) fn function_body_range(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, bytes.len()),
            b'f' if text
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with("function"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + "function".len()) =>
            {
                let open = skip_trivia(text, cursor + "function".len());
                if bytes.get(open).copied() != Some(b'(') {
                    cursor += "function".len();
                    continue;
                }
                let close = matching_delimiter(text, open)?;
                let body_start = skip_trivia(text, close + 1);
                if bytes.get(body_start).copied() == Some(b'{') {
                    let body_close = matching_delimiter(text, body_start)?;
                    return Some((body_start, body_close + 1));
                }
                let body_end = expression_end(text, body_start);
                return Some((body_start, body_end));
            }
            _ => cursor += 1,
        }
    }
    None
}

pub(super) fn static_symbol_range(
    text: &str,
    start: usize,
    end: usize,
) -> Option<(String, usize, usize)> {
    let start = skip_trivia(text, start);
    let end = trim_end_offset(text, end);
    let value = text.get(start..end)?;
    match static_arg(value) {
        Some(StaticArg::Symbol(name)) => Some((name, start, end)),
        _ => None,
    }
}

pub(super) fn static_args(raw: &RawCall) -> Vec<Option<StaticArg>> {
    raw.args
        .iter()
        .map(|argument| argument.static_arg.clone())
        .collect()
}

pub(super) fn call_after_name(text: &str, name_start: usize, name_end: usize) -> Option<RawCall> {
    let open = skip_trivia(text, name_end);
    if text.as_bytes().get(open).copied()? != b'(' {
        return None;
    }
    let close = matching_delimiter(text, open)?;
    Some(RawCall {
        start: name_start,
        end: close + 1,
        args: split_arguments(text, open + 1, close),
    })
}

pub(super) fn split_arguments(text: &str, start: usize, end: usize) -> Vec<RawArgument> {
    let mut arguments = Vec::new();
    let mut segment_start = start;
    let mut cursor = start;
    let mut stack = Vec::new();
    let bytes = text.as_bytes();
    let mut quote = None;

    while cursor < end {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(end);
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => {
                cursor = skip_comment(text, cursor, end);
            }
            b'(' | b'[' | b'{' => {
                stack.push(byte);
                cursor += 1;
            }
            b')' | b']' | b'}' => {
                let _ = stack.pop();
                cursor += 1;
            }
            b',' if stack.is_empty() => {
                if let Some(argument) = raw_argument(text, segment_start, cursor) {
                    arguments.push(argument);
                } else {
                    arguments.push(RawArgument {
                        name: None,
                        value: TextRange::new(cursor, cursor),
                        static_arg: None,
                    });
                }
                segment_start = cursor + 1;
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }

    if segment_start < end || !arguments.is_empty() {
        if let Some(argument) = raw_argument(text, segment_start, end) {
            arguments.push(argument);
        } else if segment_start < end {
            arguments.push(RawArgument {
                name: None,
                value: TextRange::new(end, end),
                static_arg: None,
            });
        }
    }
    arguments
}

pub(super) fn raw_argument(text: &str, start: usize, end: usize) -> Option<RawArgument> {
    let start = skip_trivia_bounded(text, start, end);
    let end = trim_end_offset_bounded(text, start, end);
    if start >= end {
        return None;
    }
    let (name, value_start) = named_argument_split(text, start, end)
        .map(|(name, value_start)| (Some(name), value_start))
        .unwrap_or((None, start));
    let value_start = skip_trivia_bounded(text, value_start, end);
    let value_end = trim_end_offset_bounded(text, value_start, end);
    let static_arg = text.get(value_start..value_end).and_then(static_arg);
    Some(RawArgument {
        name,
        value: TextRange::new(value_start, value_end),
        static_arg,
    })
}

pub(super) fn argument_spans(source: &SourceId, arguments: &[RawArgument]) -> Vec<Option<Span>> {
    arguments
        .iter()
        .map(|argument| {
            (argument.value.start < argument.value.end)
                .then(|| Span::new(*source, argument.value.start, argument.value.end))
        })
        .collect()
}

pub(super) fn named_argument_split(
    text: &str,
    start: usize,
    end: usize,
) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut cursor = start;
    let mut stack = Vec::new();
    let mut quote = None;
    while cursor < end {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(end);
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'(' | b'[' | b'{' => {
                stack.push(byte);
                cursor += 1;
            }
            b')' | b']' | b'}' => {
                let _ = stack.pop();
                cursor += 1;
            }
            b'=' if stack.is_empty() => {
                let previous = cursor
                    .checked_sub(1)
                    .and_then(|index| bytes.get(index))
                    .copied();
                let next = bytes.get(cursor + 1).copied();
                if matches!(previous, Some(b'=' | b'!' | b'<' | b'>')) || next == Some(b'=') {
                    cursor += 1;
                    continue;
                }
                let lhs_start = skip_trivia_bounded(text, start, cursor);
                let lhs_end = trim_end_offset_bounded(text, lhs_start, cursor);
                let lhs = text.get(lhs_start..lhs_end)?;
                let name = static_symbol(lhs)?;
                return Some((name, cursor + 1));
            }
            _ => cursor += 1,
        }
    }
    None
}

pub(super) fn static_arg(value: &str) -> Option<StaticArg> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(string) = static_string(value) {
        return Some(StaticArg::String(string));
    }
    static_symbol(value).map(StaticArg::Symbol)
}

pub(super) fn static_string(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let quote = *bytes.first()?;
    if !matches!(quote, b'\'' | b'"') || bytes.last().copied()? != quote || bytes.len() < 2 {
        return None;
    }
    let mut output = String::new();
    let mut cursor = 1;
    while cursor + 1 < bytes.len() {
        let byte = bytes[cursor];
        if byte != b'\\' {
            let character = value.get(cursor..)?.chars().next()?;
            output.push(character);
            cursor += character.len_utf8();
            continue;
        }
        cursor += 1;
        let escaped = *bytes.get(cursor)?;
        match escaped {
            b'\\' => output.push('\\'),
            b'\'' => output.push('\''),
            b'"' => output.push('"'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'b' => output.push('\u{0008}'),
            b'f' => output.push('\u{000c}'),
            b'a' => output.push('\u{0007}'),
            b'v' => output.push('\u{000b}'),
            _ => return None,
        }
        cursor += 1;
    }
    Some(output)
}

pub(super) fn static_symbol(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('`') && value.ends_with('`') && value.len() >= 2 {
        let inner = &value[1..value.len() - 1];
        if inner.contains('`') || inner.contains('\\') {
            return None;
        }
        return Some(inner.to_owned());
    }
    let mut chars = value.chars();
    let first = chars.next()?;
    if !(first.is_alphabetic() || first == '.' || first == '_') {
        return None;
    }
    if !chars.all(|character| character.is_alphanumeric() || character == '.' || character == '_') {
        return None;
    }
    Some(value.to_owned())
}

pub(super) fn namespace_extent(text: &str, start: usize) -> Option<(usize, bool)> {
    let mut cursor = name_token_end(text, start)?;
    cursor = skip_trivia(text, cursor);
    let rest = text.get(cursor..)?;
    let (operator_len, internal) = if rest.starts_with(":::") {
        (3, true)
    } else if rest.starts_with("::") {
        (2, false)
    } else {
        return None;
    };
    cursor += operator_len;
    cursor = skip_trivia(text, cursor);
    let end = name_token_end(text, cursor)?;
    Some((end, internal))
}

pub(super) fn name_token_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let first = *bytes.get(start)?;
    if first == b'`' {
        let mut cursor = start + 1;
        while cursor < bytes.len() {
            if bytes[cursor] == b'\\' {
                cursor += 2;
                continue;
            }
            if bytes[cursor] == b'`' {
                return Some(cursor + 1);
            }
            cursor += 1;
        }
        return None;
    }
    if first == b'\'' || first == b'"' {
        return quoted_end(text, start);
    }
    let mut cursor = start;
    while cursor < bytes.len() {
        let character = text.get(cursor..)?.chars().next()?;
        if !(character.is_alphanumeric() || character == '.' || character == '_') {
            break;
        }
        cursor += character.len_utf8();
    }
    (cursor > start).then_some(cursor)
}

pub(super) fn quoted_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let quote = *bytes.get(start)?;
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            cursor += 2;
            continue;
        }
        if bytes[cursor] == quote {
            return Some(cursor + 1);
        }
        cursor += 1;
    }
    None
}

pub(super) fn matching_delimiter(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let opener = *bytes.get(open)?;
    let expected = match opener {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        _ => return None,
    };
    let mut stack = vec![expected];
    let mut quote = None;
    let mut cursor = open + 1;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, bytes.len()),
            b'(' => {
                stack.push(b')');
                cursor += 1;
            }
            b'[' => {
                stack.push(b']');
                cursor += 1;
            }
            b'{' => {
                stack.push(b'}');
                cursor += 1;
            }
            b')' | b']' | b'}' => {
                if stack.pop()? != byte {
                    return None;
                }
                if stack.is_empty() {
                    return Some(cursor);
                }
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    None
}

pub(super) fn find_function_regions(text: &str) -> Vec<FunctionRegion> {
    let mut regions = Vec::new();
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }

        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, bytes.len()),
            b'f' if text
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with("function"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + "function".len()) =>
            {
                let open = skip_trivia(text, cursor + "function".len());
                if bytes.get(open).copied() != Some(b'(') {
                    cursor += "function".len();
                    continue;
                }
                if let Some(region) = function_region_after_open(text, cursor, open) {
                    regions.push(region);
                }
                cursor += "function".len();
            }
            b'\\' => {
                let open = skip_trivia(text, cursor + 1);
                if bytes.get(open).copied() == Some(b'(')
                    && let Some(region) = function_region_after_open(text, cursor, open)
                {
                    regions.push(region);
                }
                cursor += 1;
            }
            _ => cursor += text[cursor..].chars().next().map_or(1, char::len_utf8),
        }
    }

    regions
}

pub(super) fn function_region_after_open(
    text: &str,
    function_start: usize,
    open: usize,
) -> Option<FunctionRegion> {
    let close = matching_delimiter(text, open)?;
    let body_start = skip_trivia(text, close + 1);
    if body_start >= text.len() {
        return None;
    }
    let body_end = expression_end(text, body_start);
    let parameters = split_arguments(text, open + 1, close)
        .into_iter()
        .filter_map(|argument| {
            let RawArgument { name, value, .. } = argument;
            name.or_else(|| text.get(value.start..value.end).and_then(static_symbol))
        })
        .collect();

    Some(FunctionRegion {
        function_start,
        formals: TextRange::new(open + 1, close),
        body: TextRange::new(body_start, body_end),
        parameters,
    })
}

pub(super) fn find_for_regions(text: &str) -> Vec<ForRegion> {
    let mut regions = Vec::new();
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }

        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, bytes.len()),
            b'f' if text
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with("for"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + 3) =>
            {
                let open = skip_trivia(text, cursor + 3);
                if bytes.get(open).copied() != Some(b'(') {
                    cursor += 3;
                    continue;
                }
                let Some(close) = matching_delimiter(text, open) else {
                    cursor += 3;
                    continue;
                };
                let variable_start = skip_trivia_bounded(text, open + 1, close);
                let Some(variable_end) = name_token_end(text, variable_start) else {
                    cursor += 3;
                    continue;
                };
                let Some(variable) = text
                    .get(variable_start..variable_end)
                    .and_then(static_symbol)
                else {
                    cursor += 3;
                    continue;
                };
                let in_start = skip_trivia_bounded(text, variable_end, close);
                if !text
                    .get(in_start..close)
                    .is_some_and(|rest| rest.starts_with("in"))
                    || !word_boundary_after(text, in_start + 2)
                {
                    cursor += 3;
                    continue;
                }
                let body_start = skip_trivia(text, close + 1);
                let body_end = expression_end(text, body_start);
                regions.push(ForRegion {
                    variable,
                    variable_start,
                    body: TextRange::new(body_start, body_end),
                });
                cursor += 3;
            }
            _ => cursor += text[cursor..].chars().next().map_or(1, char::len_utf8),
        }
    }

    regions
}

pub(super) fn find_if_regions(text: &str) -> Vec<IfRegion> {
    scan_if_regions(text, usize::MAX)
}

pub(super) fn first_if_region(text: &str) -> Option<IfRegion> {
    scan_if_regions(text, 1).into_iter().next()
}

pub(super) fn scan_if_regions(text: &str, limit: usize) -> Vec<IfRegion> {
    let mut regions = Vec::new();
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut quote = None;
    while cursor < bytes.len() && regions.len() < limit {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' => cursor = skip_comment(text, cursor, bytes.len()),
            b'i' if text
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with("if"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + 2) =>
            {
                let open = skip_trivia(text, cursor + 2);
                if bytes.get(open).copied() != Some(b'(') {
                    cursor += 2;
                    continue;
                }
                let Some(close) = matching_delimiter(text, open) else {
                    cursor += 2;
                    continue;
                };
                let then_start = skip_trivia(text, close + 1);
                let then_end = expression_end(text, then_start);
                let after_then = skip_trivia(text, then_end);
                let else_branch = if text
                    .get(after_then..)
                    .is_some_and(|rest| rest.starts_with("else"))
                    && word_boundary_after(text, after_then + 4)
                {
                    let start = skip_trivia(text, after_then + 4);
                    let end = expression_end(text, start);
                    Some(TextRange::new(start, end))
                } else {
                    None
                };
                regions.push(IfRegion {
                    if_start: cursor,
                    condition: TextRange::new(open + 1, close),
                    then_branch: TextRange::new(then_start, then_end),
                    else_branch,
                });
                cursor += 2;
            }
            _ => cursor += 1,
        }
    }
    regions
}

pub(super) fn expression_end(text: &str, start: usize) -> usize {
    let start = skip_trivia(text, start);
    let bytes = text.as_bytes();
    if start >= bytes.len() {
        return start;
    }
    if matches!(bytes[start], b'(' | b'[' | b'{') {
        return matching_delimiter(text, start).map_or(bytes.len(), |close| close + 1);
    }
    if text.get(start..).is_some_and(|rest| rest.starts_with("if"))
        && word_boundary_after(text, start + 2)
        && let Some(region) = first_if_region(&text[start..])
    {
        return start
            + region
                .else_branch
                .map_or(region.then_branch.end, |branch| branch.end);
    }

    let mut cursor = start;
    let mut stack = Vec::new();
    let mut quote = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                cursor += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'#' if stack.is_empty() => return trim_end_offset(text, cursor),
            b'#' => cursor = skip_comment(text, cursor, bytes.len()),
            b'(' | b'[' | b'{' => {
                stack.push(byte);
                cursor += 1;
            }
            b')' | b']' | b'}' => {
                if stack.is_empty() {
                    return trim_end_offset(text, cursor);
                }
                let _ = stack.pop();
                cursor += 1;
            }
            b';' | b'\n' if stack.is_empty() => return trim_end_offset(text, cursor),
            b'e' if stack.is_empty()
                && text
                    .get(cursor..)
                    .is_some_and(|rest| rest.starts_with("else"))
                && word_boundary_before(text, cursor)
                && word_boundary_after(text, cursor + 4) =>
            {
                return trim_end_offset(text, cursor);
            }
            _ => cursor += 1,
        }
    }
    trim_end_offset(text, cursor)
}

pub(super) fn statement_start(text: &str, position: usize) -> usize {
    let bytes = text.as_bytes();
    let mut cursor = position;
    while cursor > 0 {
        let byte = bytes[cursor - 1];
        if matches!(byte, b';' | b'\n' | b'{' | b'}') {
            break;
        }
        cursor -= 1;
    }
    cursor
}

pub(super) fn skip_trivia(text: &str, mut cursor: usize) -> usize {
    let bytes = text.as_bytes();
    loop {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor).copied() != Some(b'#') {
            return cursor;
        }
        cursor = skip_comment(text, cursor, bytes.len());
    }
}

pub(super) fn skip_trivia_bounded(text: &str, mut cursor: usize, end: usize) -> usize {
    let bytes = text.as_bytes();
    loop {
        while cursor < end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= end || bytes[cursor] != b'#' {
            return cursor;
        }
        cursor = skip_comment(text, cursor, end);
    }
}

pub(super) fn skip_comment(text: &str, mut cursor: usize, end: usize) -> usize {
    let bytes = text.as_bytes();
    while cursor < end && bytes[cursor] != b'\n' {
        cursor += 1;
    }
    cursor
}

pub(super) fn trim_end_offset(text: &str, end: usize) -> usize {
    trim_end_offset_bounded(text, 0, end)
}

pub(super) fn trim_end_offset_bounded(text: &str, start: usize, mut end: usize) -> usize {
    let bytes = text.as_bytes();
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

pub(super) fn word_boundary_before(text: &str, position: usize) -> bool {
    position == 0
        || text
            .get(..position)
            .and_then(|prefix| prefix.chars().next_back())
            .is_none_or(|character| {
                !(character.is_alphanumeric() || character == '.' || character == '_')
            })
}

pub(super) fn word_boundary_after(text: &str, position: usize) -> bool {
    text.get(position..)
        .and_then(|suffix| suffix.chars().next())
        .is_none_or(|character| {
            !(character.is_alphanumeric() || character == '.' || character == '_')
        })
}
