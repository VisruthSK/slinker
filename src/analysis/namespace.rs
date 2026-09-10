use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceDirective {
    Export(String),
    ExportPattern(String),
    Import(String),
    ImportFrom { package: String, symbols: Vec<String> },
    S3Method { generic: String, class: String, method: Option<String> },
    UseDynLib { dll: String },
    Other(String),
}

pub fn parse_namespace(text: &str) -> Result<Vec<NamespaceDirective>> {
    let mut out = Vec::new();
    for raw in logical_lines(text) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, body)) = split_call(line) else {
            out.push(NamespaceDirective::Other(line.to_owned()));
            continue;
        };
        let args = split_args(body).into_iter().map(unquote).collect::<Vec<_>>();
        let directive = match name {
            "export" if !args.is_empty() => {
                for symbol in args { out.push(NamespaceDirective::Export(symbol)); }
                continue;
            }
            "exportPattern" if args.len() == 1 => NamespaceDirective::ExportPattern(args[0].clone()),
            "import" if !args.is_empty() => {
                for package in args { out.push(NamespaceDirective::Import(package)); }
                continue;
            }
            "importFrom" if args.len() >= 2 => NamespaceDirective::ImportFrom {
                package: args[0].clone(),
                symbols: args[1..].to_vec(),
            },
            "S3method" if args.len() >= 2 => NamespaceDirective::S3Method {
                generic: args[0].clone(),
                class: args[1].clone(),
                method: args.get(2).cloned(),
            },
            "useDynLib" if !args.is_empty() => NamespaceDirective::UseDynLib { dll: args[0].clone() },
            _ => NamespaceDirective::Other(line.to_owned()),
        };
        out.push(directive);
    }
    Ok(out)
}

fn logical_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escape = false;
    for ch in text.chars() {
        if escape { current.push(ch); escape = false; continue; }
        if ch == '\\' && quote.is_some() { current.push(ch); escape = true; continue; }
        if let Some(q) = quote {
            current.push(ch);
            if ch == q { quote = None; }
            continue;
        }
        match ch {
            '\'' | '"' => { quote = Some(ch); current.push(ch); }
            '(' => { depth += 1; current.push(ch); }
            ')' => { depth -= 1; current.push(ch); }
            '\n' if depth == 0 => { if !current.trim().is_empty() { out.push(std::mem::take(&mut current)); } }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() { out.push(current); }
    out
}

fn split_call(line: &str) -> Option<(&str, &str)> {
    let open = line.find('(')?;
    if !line.ends_with(')') { return None; }
    Some((line[..open].trim(), &line[open + 1..line.len() - 1]))
}

fn split_args(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut quote: Option<char> = None;
    let mut escape = false;
    for (i, ch) in body.char_indices() {
        if escape { escape = false; continue; }
        if ch == '\\' && quote.is_some() { escape = true; continue; }
        if let Some(q) = quote { if ch == q { quote = None; } continue; }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ',' => { out.push(body[start..i].trim()); start = i + 1; }
            _ => {}
        }
    }
    let tail = body[start..].trim();
    if !tail.is_empty() { out.push(tail); }
    out
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.as_bytes()[0] as char;
        let last = value.as_bytes()[value.len() - 1] as char;
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            return value[1..value.len() - 1].to_owned();
        }
    }
    value.to_owned()
}
