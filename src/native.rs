use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeHazard {
    pub path: PathBuf,
    pub symbol: String,
    pub byte_offset: usize,
}

#[derive(Debug)]
pub enum NativeScanError {
    Io { path: PathBuf, source: std::io::Error },
}

const HAZARDOUS_IDENTIFIERS: &[&str] = &[
    "R_GetCCallable",
    "R_RegisterCCallable",
    "R_FindNamespace",
    "Rf_eval",
    "R_tryEval",
    "R_ParseVector",
    "R_GetDLLRegisteredRoutines",
    "dlopen",
    "LoadLibrary",
    "LoadLibraryA",
    "LoadLibraryW",
    "GetProcAddress",
    "FreeLibrary",
];

/// Cheap native compatibility prefilter. A hit is evidence that requires a
/// native compatibility decision; absence of hits is never proof of safety.
///
/// Unlike a raw substring scan, this ignores comments and string/character
/// literals so examples and diagnostic text do not become false hazards.
pub fn scan_native_tree(root: &Path) -> Result<Vec<NativeHazard>, NativeScanError> {
    let src = root.join("src");
    if !src.is_dir() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    collect_files(&src, &mut files).map_err(|source| NativeScanError::Io {
        path: src.clone(),
        source,
    })?;
    files.sort();

    let mut hazards = Vec::new();
    for path in files {
        if !is_native_source(&path) {
            continue;
        }
        let bytes = fs::read(&path).map_err(|source| NativeScanError::Io {
            path: path.clone(),
            source,
        })?;
        for (offset, identifier) in c_like_identifiers(&bytes) {
            if HAZARDOUS_IDENTIFIERS.contains(&identifier) {
                hazards.push(NativeHazard {
                    path: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
                    symbol: identifier.to_owned(),
                    byte_offset: offset,
                });
            }
        }
    }
    hazards.sort_by(|left, right| {
        (&left.path, left.byte_offset, &left.symbol)
            .cmp(&(&right.path, right.byte_offset, &right.symbol))
    });
    hazards.dedup();
    Ok(hazards)
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            collect_files(&entry.path(), files)?;
        } else if ty.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn is_native_source(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("c")
            | Some("cc")
            | Some("cpp")
            | Some("cxx")
            | Some("h")
            | Some("hh")
            | Some("hpp")
            | Some("hxx")
    )
}

fn c_like_identifiers(bytes: &[u8]) -> Vec<(usize, &str)> {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        String(u8),
    }

    let mut out = Vec::new();
    let mut state = State::Code;
    let mut index = 0usize;

    while index < bytes.len() {
        match state {
            State::Code if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') => {
                state = State::LineComment;
                index += 2;
            }
            State::Code if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') => {
                state = State::BlockComment;
                index += 2;
            }
            State::Code if bytes[index] == b'\'' || bytes[index] == b'"' => {
                state = State::String(bytes[index]);
                index += 1;
            }
            State::Code if is_ident_start(bytes[index]) => {
                let start = index;
                index += 1;
                while index < bytes.len() && is_ident_continue(bytes[index]) {
                    index += 1;
                }
                if let Ok(identifier) = std::str::from_utf8(&bytes[start..index]) {
                    out.push((start, identifier));
                }
            }
            State::Code => index += 1,
            State::LineComment if bytes[index] == b'\n' => {
                state = State::Code;
                index += 1;
            }
            State::LineComment => index += 1,
            State::BlockComment
                if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') =>
            {
                state = State::Code;
                index += 2;
            }
            State::BlockComment => index += 1,
            State::String(_) if bytes[index] == b'\\' => {
                index = (index + 2).min(bytes.len());
            }
            State::String(quote) if bytes[index] == quote => {
                state = State::Code;
                index += 1;
            }
            State::String(_) => index += 1,
        }
    }

    out
}

fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

impl fmt::Display for NativeScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "failed to scan native source {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for NativeScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::c_like_identifiers;

    #[test]
    fn ignores_comments_and_literals() {
        let source = br#"
            // Rf_eval(x)
            const char *x = "R_GetCCallable";
            /* dlopen("x") */
            Rf_eval(call, env);
        "#;
        let identifiers: Vec<_> = c_like_identifiers(source)
            .into_iter()
            .map(|(_, identifier)| identifier)
            .collect();
        assert_eq!(identifiers.iter().filter(|name| **name == "Rf_eval").count(), 1);
        assert!(!identifiers.contains(&"R_GetCCallable"));
        assert!(!identifiers.contains(&"dlopen"));
    }
}
