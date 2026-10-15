use crate::package::Atom;
use crate::syntax::facts::StaticArg;
use crate::syntax::source::TextRange;
use std::collections::BTreeSet;
#[derive(Debug, Clone)]
pub(super) struct RawArgument {
    pub(super) name: Option<String>,
    pub(super) value: TextRange,
    pub(super) static_arg: Option<StaticArg>,
}

impl RawArgument {
    pub(super) fn is_hole(&self) -> bool {
        self.name.is_none() && self.value.start == self.value.end
    }
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

pub(super) fn static_arg(value: &str) -> Option<StaticArg> {
    let value = value.trim();
    match value {
        "TRUE" => return Some(StaticArg::Logical(true)),
        "FALSE" => return Some(StaticArg::Logical(false)),
        _ => {}
    }
    if value.is_empty() {
        return None;
    }
    if let Some(string) = static_string(value) {
        return Some(StaticArg::String(Atom::from(string)));
    }
    static_symbol(value).map(|symbol| StaticArg::Symbol(Atom::from(symbol)))
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

pub(super) fn skip_comment(text: &str, mut cursor: usize, end: usize) -> usize {
    let bytes = text.as_bytes();
    while cursor < end && bytes[cursor] != b'\n' {
        cursor += 1;
    }
    cursor
}
