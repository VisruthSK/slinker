use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use crate::{RToolchain, Target, ToolchainError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetProvidedPackage {
    pub name: String,
    pub version: String,
    pub library: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetEnvironment {
    pub target: Target,
    pub libraries: Vec<PathBuf>,
    pub packages: Vec<TargetProvidedPackage>,
}

impl TargetEnvironment {
    pub fn package(&self, name: &str) -> Option<&TargetProvidedPackage> {
        self.packages.iter().find(|package| package.name == name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetEnvironmentRequest {
    pub work_dir: PathBuf,
    /// Ordered exactly like `.libPaths()`. An empty vector asks the target R
    /// process to capture its own sanitized `.libPaths()`.
    pub libraries: Vec<PathBuf>,
}

impl TargetEnvironmentRequest {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        Self { work_dir: work_dir.into(), libraries: Vec::new() }
    }
}

#[derive(Debug)]
pub enum TargetEnvironmentError {
    Io(std::io::Error),
    Toolchain(ToolchainError),
    Protocol(String),
}

impl RToolchain {
    pub fn capture_target_environment(
        &self,
        request: &TargetEnvironmentRequest,
    ) -> Result<TargetEnvironment, TargetEnvironmentError> {
        fs::create_dir_all(&request.work_dir).map_err(TargetEnvironmentError::Io)?;
        let helper = request.work_dir.join("capture-target.R");
        let output = request.work_dir.join("target-environment.hrm");
        fs::write(&helper, include_str!("r/target_env.R")).map_err(TargetEnvironmentError::Io)?;

        let mut args = Vec::with_capacity(2 + request.libraries.len());
        args.push(helper.as_os_str());
        args.push(output.as_os_str());
        for library in &request.libraries {
            args.push(library.as_os_str());
        }
        self.run_rscript(args).map_err(TargetEnvironmentError::Toolchain)?;

        let text = fs::read_to_string(&output).map_err(TargetEnvironmentError::Io)?;
        parse_target_environment(&text)
    }
}

fn parse_target_environment(text: &str) -> Result<TargetEnvironment, TargetEnvironmentError> {
    let mut target: Option<Target> = None;
    let mut libraries = BTreeMap::<usize, PathBuf>::new();
    let mut packages = Vec::<TargetProvidedPackage>::new();

    for (line_no, line) in text.lines().enumerate() {
        if line.is_empty() { continue; }
        let mut fields = line.split('\t');
        let kind = fields.next().unwrap_or_default();
        let values = fields
            .map(decode_hex)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| TargetEnvironmentError::Protocol(format!("line {}: {error}", line_no + 1)))?;

        match kind {
            "HEADER" => {
                require_len(kind, &values, 3, line_no)?;
                target = Some(Target {
                    r_version: values[0].clone(),
                    os: values[1].clone(),
                    arch: values[2].clone(),
                });
            }
            "LIB" => {
                require_len(kind, &values, 2, line_no)?;
                let index = values[0].parse::<usize>().map_err(|_| {
                    TargetEnvironmentError::Protocol(format!("line {}: invalid library index {:?}", line_no + 1, values[0]))
                })?;
                libraries.insert(index, PathBuf::from(&values[1]));
            }
            "PACKAGE" => {
                require_len(kind, &values, 3, line_no)?;
                packages.push(TargetProvidedPackage {
                    name: values[0].clone(),
                    version: values[1].clone(),
                    library: PathBuf::from(&values[2]),
                });
            }
            other => return Err(TargetEnvironmentError::Protocol(format!("line {}: unknown record {other:?}", line_no + 1))),
        }
    }

    let target = target.ok_or_else(|| TargetEnvironmentError::Protocol("missing HEADER".into()))?;
    let libraries = libraries.into_values().collect::<Vec<_>>();

    // The helper emits packages in library search order and suppresses shadowed
    // duplicates. Preserve that order; it is part of the target resolution.
    Ok(TargetEnvironment { target, libraries, packages })
}

fn require_len(kind: &str, values: &[String], expected: usize, line_no: usize) -> Result<(), TargetEnvironmentError> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(TargetEnvironmentError::Protocol(format!("line {}: {kind} expected {expected} fields, got {}", line_no + 1, values.len())))
    }
}

fn decode_hex(value: &str) -> Result<String, String> {
    if value.len() % 2 != 0 { return Err(format!("odd-length hex field {value:?}")); }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let high = nibble(bytes[i]).ok_or_else(|| format!("invalid hex field {value:?}"))?;
        let low = nibble(bytes[i + 1]).ok_or_else(|| format!("invalid hex field {value:?}"))?;
        out.push((high << 4) | low);
        i += 2;
    }
    String::from_utf8(out).map_err(|error| format!("invalid utf-8 field: {error}"))
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for TargetEnvironmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "target-environment I/O error: {error}"),
            Self::Toolchain(error) => write!(f, "target-environment capture failed: {error}"),
            Self::Protocol(error) => write!(f, "invalid target-environment output: {error}"),
        }
    }
}

impl std::error::Error for TargetEnvironmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Toolchain(error) => Some(error),
            Self::Protocol(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_target_environment;

    fn h(value: &str) -> String {
        value.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn parses_target_resolution_order() {
        let text = format!(
            "HEADER\t{}\t{}\t{}\nLIB\t{}\t{}\nPACKAGE\t{}\t{}\t{}\n",
            h("4.6.1"), h("linux-gnu"), h("x86_64"), h("0"), h("/target/lib"),
            h("stats"), h("4.6.1"), h("/target/lib")
        );
        let target = parse_target_environment(&text).unwrap();
        assert_eq!(target.target.r_version, "4.6.1");
        assert_eq!(target.libraries, vec![PathBuf::from("/target/lib")]);
        assert_eq!(target.package("stats").unwrap().version, "4.6.1");
    }

    use std::path::PathBuf;
}
