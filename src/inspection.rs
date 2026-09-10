use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::ffi::OsString;

use crate::{RToolchain, ToolchainError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingOrigin {
    Code,
    Sysdata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectIssue {
    pub path: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureEnvironment {
    pub path: String,
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectState {
    pub name: String,
    pub type_name: String,
    pub supported: bool,
    pub origin: Option<BindingOrigin>,
    pub issues: Vec<ObjectIssue>,
    pub closure_environments: Vec<ClosureEnvironment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportedBinding {
    pub name: String,
    pub binding: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedBinding {
    pub name: String,
    pub binding: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportDirective {
    All { package: String, except: Vec<String> },
    From { package: String, bindings: Vec<ImportedBinding> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3Registration {
    pub generic: String,
    pub class: String,
    pub method: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectionRequest {
    pub library: PathBuf,
    pub package: String,
    pub output_path: PathBuf,
}

impl InspectionRequest {
    pub fn new(
        library: impl Into<PathBuf>,
        package: impl Into<String>,
        output_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            library: library.into(),
            package: package.into(),
            output_path: output_path.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticSnapshot {
    pub state: SemanticState,
    pub recipes_rds: PathBuf,
    pub analysis_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticState {
    pub package: String,
    pub version: String,
    pub has_on_load: bool,
    pub exports: Vec<ExportedBinding>,
    pub imports: Vec<ImportDirective>,
    pub bindings: Vec<ObjectState>,
    pub datasets: Vec<ObjectState>,
    pub package_issues: Vec<ObjectIssue>,
    pub s3: Vec<S3Registration>,
    pub dynlibs: Vec<String>,
    pub resources: Vec<String>,
}

#[derive(Debug)]
pub enum InspectError {
    Io(std::io::Error),
    Toolchain(ToolchainError),
    Protocol(String),
    NonUtf8Path(PathBuf),
}

impl RToolchain {
    pub fn inspect_package(
        &self,
        library: &Path,
        package: &str,
        output_path: &Path,
    ) -> Result<SemanticState, InspectError> {
        Ok(self.inspect_package_snapshot(library, package, output_path)?.state)
    }

    pub fn inspect_package_snapshot(
        &self,
        library: &Path,
        package: &str,
        output_path: &Path,
    ) -> Result<SemanticSnapshot, InspectError> {
        self.inspect_package_snapshot_with_libraries(
            library,
            package,
            output_path,
            &[library.to_path_buf()],
        )
    }

    pub fn inspect_package_snapshot_with_libraries(
        &self,
        library: &Path,
        package: &str,
        output_path: &Path,
        libraries: &[PathBuf],
    ) -> Result<SemanticSnapshot, InspectError> {
        let request = InspectionRequest::new(library, package, output_path);
        let work_dir = output_path.with_extension("inspect-runtime");
        let result = self.inspect_package_snapshots_with_libraries(
            std::slice::from_ref(&request),
            libraries,
            1,
            &work_dir,
        );
        let _ = fs::remove_dir_all(&work_dir);
        result?.into_iter().next().ok_or_else(|| {
            InspectError::Protocol("single-package inspection returned no snapshot".into())
        })
    }

    pub fn inspect_package_snapshots_with_libraries(
        &self,
        requests: &[InspectionRequest],
        libraries: &[PathBuf],
        jobs: usize,
        work_dir: &Path,
    ) -> Result<Vec<SemanticSnapshot>, InspectError> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        fs::create_dir_all(work_dir).map_err(InspectError::Io)?;
        let manifest = work_dir.join("inspection-manifest.hrm");
        let mut text = String::new();
        for library in libraries {
            text.push_str("LIB\t");
            text.push_str(&encode_path(library)?);
            text.push('\n');
        }
        for request in requests {
            if let Some(parent) = request.output_path.parent() {
                fs::create_dir_all(parent).map_err(InspectError::Io)?;
            }
            let recipes = request.output_path.with_extension("recipes.rds");
            let analysis = request.output_path.with_extension("analysis");
            text.push_str("JOB\t");
            text.push_str(&encode_path(&request.library)?);
            text.push('\t');
            text.push_str(&encode_hex(request.package.as_bytes()));
            text.push('\t');
            text.push_str(&encode_path(&request.output_path)?);
            text.push('\t');
            text.push_str(&encode_path(&recipes)?);
            text.push('\t');
            text.push_str(&encode_path(&analysis)?);
            text.push('\n');
        }
        fs::write(&manifest, text).map_err(InspectError::Io)?;

        let args = vec![
            manifest.as_os_str().to_os_string(),
            OsString::from(jobs.max(1).to_string()),
        ];
        self.run_runtime(work_dir, "inspect-batch", args, false)
            .map_err(InspectError::Toolchain)?;

        requests
            .iter()
            .map(|request| {
                let text = fs::read_to_string(&request.output_path).map_err(InspectError::Io)?;
                Ok(SemanticSnapshot {
                    state: parse_semantic_state(&text)?,
                    recipes_rds: request.output_path.with_extension("recipes.rds"),
                    analysis_root: request.output_path.with_extension("analysis"),
                })
            })
            .collect()
    }
}

fn encode_path(path: &Path) -> Result<String, InspectError> {
    let value = path
        .to_str()
        .ok_or_else(|| InspectError::NonUtf8Path(path.to_path_buf()))?;
    Ok(encode_hex(value.as_bytes()))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn parse_semantic_state(text: &str) -> Result<SemanticState, InspectError> {
    let mut state: Option<SemanticState> = None;
    let mut current_binding: Option<usize> = None;
    let mut current_dataset: Option<usize> = None;

    for (line_no, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let kind = fields.next().unwrap_or_default();
        let values = fields
            .map(decode_hex)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| InspectError::Protocol(format!("line {}: {error}", line_no + 1)))?;

        match kind {
            "HEADER" => {
                require_len(kind, &values, 3, line_no)?;
                state = Some(SemanticState {
                    package: values[0].clone(),
                    version: values[1].clone(),
                    has_on_load: parse_bool(&values[2], line_no)?,
                    exports: Vec::new(),
                    imports: Vec::new(),
                    bindings: Vec::new(),
                    datasets: Vec::new(),
                    package_issues: Vec::new(),
                    s3: Vec::new(),
                    dynlibs: Vec::new(),
                    resources: Vec::new(),
                });
                current_binding = None;
                current_dataset = None;
            }
            "EXPORT" => {
                require_len(kind, &values, 2, line_no)?;
                state_mut(&mut state, line_no)?.exports.push(ExportedBinding {
                    name: values[0].clone(),
                    binding: values[1].clone(),
                });
            }
            "IMPORT_ALL" => {
                let package = one(kind, &values, line_no)?.to_owned();
                state_mut(&mut state, line_no)?.imports.push(ImportDirective::All { package, except: Vec::new() });
            }
            "IMPORT_EXCEPT" => {
                require_len(kind, &values, 2, line_no)?;
                let state = state_mut(&mut state, line_no)?;
                let package = &values[0];
                if let Some(ImportDirective::All { except, .. }) = state.imports.iter_mut().rev().find(|directive| matches!(directive, ImportDirective::All { package: p, .. } if p == package)) {
                    except.push(values[1].clone());
                } else {
                    return Err(InspectError::Protocol(format!("line {}: IMPORT_EXCEPT without IMPORT_ALL", line_no + 1)));
                }
            }
            "IMPORT_FROM" => {
                require_len(kind, &values, 3, line_no)?;
                let state = state_mut(&mut state, line_no)?;
                let package = &values[0];
                let binding = ImportedBinding { name: values[2].clone(), binding: values[1].clone() };
                if let Some(ImportDirective::From { bindings, .. }) = state.imports.iter_mut().rev().find(|directive| matches!(directive, ImportDirective::From { package: p, .. } if p == package)) {
                    bindings.push(binding);
                } else {
                    state.imports.push(ImportDirective::From { package: package.clone(), bindings: vec![binding] });
                }
            }
            "BINDING" => {
                require_len(kind, &values, 4, line_no)?;
                let origin = match values[1].as_str() {
                    "code" => BindingOrigin::Code,
                    "sysdata" => BindingOrigin::Sysdata,
                    other => return Err(InspectError::Protocol(format!("line {}: bad binding origin {other:?}", line_no + 1))),
                };
                let object = ObjectState {
                    name: values[0].clone(),
                    origin: Some(origin),
                    type_name: values[2].clone(),
                    supported: parse_bool(&values[3], line_no)?,
                    issues: Vec::new(),
                    closure_environments: Vec::new(),
                };
                let state = state_mut(&mut state, line_no)?;
                state.bindings.push(object);
                current_binding = Some(state.bindings.len() - 1);
                current_dataset = None;
            }
            "DATASET" => {
                require_len(kind, &values, 3, line_no)?;
                let object = ObjectState {
                    name: values[0].clone(),
                    origin: None,
                    type_name: values[1].clone(),
                    supported: parse_bool(&values[2], line_no)?,
                    issues: Vec::new(),
                    closure_environments: Vec::new(),
                };
                let state = state_mut(&mut state, line_no)?;
                state.datasets.push(object);
                current_dataset = Some(state.datasets.len() - 1);
                current_binding = None;
            }
            "ISSUE" => {
                require_len(kind, &values, 3, line_no)?;
                let issue = ObjectIssue { path: values[0].clone(), kind: values[1].clone(), detail: values[2].clone() };
                let state = state_mut(&mut state, line_no)?;
                if let Some(index) = current_binding {
                    state.bindings[index].issues.push(issue);
                } else if let Some(index) = current_dataset {
                    state.datasets[index].issues.push(issue);
                } else {
                    return Err(InspectError::Protocol(format!("line {}: ISSUE without object", line_no + 1)));
                }
            }
            "CLOSURE_ENV" => {
                require_len(kind, &values, 3, line_no)?;
                let env = ClosureEnvironment { path: values[0].clone(), kind: values[1].clone(), name: values[2].clone() };
                let state = state_mut(&mut state, line_no)?;
                if let Some(index) = current_binding {
                    state.bindings[index].closure_environments.push(env);
                } else if let Some(index) = current_dataset {
                    state.datasets[index].closure_environments.push(env);
                } else {
                    return Err(InspectError::Protocol(format!("line {}: CLOSURE_ENV without object", line_no + 1)));
                }
            }
            "PACKAGE_ISSUE" => {
                require_len(kind, &values, 3, line_no)?;
                state_mut(&mut state, line_no)?.package_issues.push(ObjectIssue {
                    path: values[0].clone(),
                    kind: values[1].clone(),
                    detail: values[2].clone(),
                });
                current_binding = None;
                current_dataset = None;
            }
            "S3" => {
                require_len(kind, &values, 3, line_no)?;
                state_mut(&mut state, line_no)?.s3.push(S3Registration { generic: values[0].clone(), class: values[1].clone(), method: values[2].clone() });
                current_binding = None;
                current_dataset = None;
            }
            "DYNLIB" => state_mut(&mut state, line_no)?.dynlibs.push(one(kind, &values, line_no)?.to_owned()),
            "RESOURCE" => state_mut(&mut state, line_no)?.resources.push(one(kind, &values, line_no)?.to_owned()),
            other => return Err(InspectError::Protocol(format!("line {}: unknown record {other:?}", line_no + 1))),
        }
    }

    let mut state = state.ok_or_else(|| InspectError::Protocol("missing HEADER".into()))?;
    state.exports.sort_by(|a, b| (&a.name, &a.binding).cmp(&(&b.name, &b.binding)));
    state.exports.dedup();
    state.bindings.sort_by(|a, b| a.name.cmp(&b.name));
    state.datasets.sort_by(|a, b| a.name.cmp(&b.name));
    state.resources.sort();
    state.resources.dedup();
    Ok(state)
}

fn state_mut(state: &mut Option<SemanticState>, line_no: usize) -> Result<&mut SemanticState, InspectError> {
    state.as_mut().ok_or_else(|| InspectError::Protocol(format!("line {}: record before HEADER", line_no + 1)))
}

fn require_len(kind: &str, values: &[String], expected: usize, line_no: usize) -> Result<(), InspectError> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(InspectError::Protocol(format!("line {}: {kind} expected {expected} fields, got {}", line_no + 1, values.len())))
    }
}

fn one<'a>(kind: &str, values: &'a [String], line_no: usize) -> Result<&'a str, InspectError> {
    require_len(kind, values, 1, line_no)?;
    Ok(&values[0])
}

fn parse_bool(value: &str, line_no: usize) -> Result<bool, InspectError> {
    match value {
        "1" => Ok(true),
        "0" => Ok(false),
        other => Err(InspectError::Protocol(format!("line {}: invalid bool {other:?}", line_no + 1))),
    }
}

fn decode_hex(value: &str) -> Result<String, String> {
    if value.len() % 2 != 0 {
        return Err(format!("odd-length hex field {value:?}"));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let chars = value.as_bytes();
    let mut index = 0;
    while index < chars.len() {
        let high = hex_nibble(chars[index]).ok_or_else(|| format!("invalid hex field {value:?}"))?;
        let low = hex_nibble(chars[index + 1]).ok_or_else(|| format!("invalid hex field {value:?}"))?;
        bytes.push((high << 4) | low);
        index += 2;
    }
    String::from_utf8(bytes).map_err(|error| format!("invalid utf-8 field: {error}"))
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "semantic inspection I/O error: {error}"),
            Self::Toolchain(error) => write!(f, "semantic inspection failed: {error}"),
            Self::Protocol(error) => write!(f, "invalid semantic inspection output: {error}"),
            Self::NonUtf8Path(path) => write!(f, "R inspection requires a Unicode path: {}", path.display()),
        }
    }
}

impl std::error::Error for InspectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Toolchain(error) => Some(error),
            Self::Protocol(_) | Self::NonUtf8Path(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BindingOrigin, ImportDirective, ImportedBinding, parse_semantic_state};

    fn h(value: &str) -> String {
        value.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn parses_protocol() {
        let text = format!(
            "HEADER\t{}\t{}\t{}\nEXPORT\t{}\t{}\nIMPORT_FROM\t{}\t{}\t{}\nBINDING\t{}\t{}\t{}\t{}\nISSUE\t{}\t{}\t{}\n",
            h("foo"), h("1.2.3"), h("0"), h("run"), h("run"), h("bar"), h("x"), h("local_x"),
            h("run"), h("code"), h("closure"), h("0"), h("$.attr"), h("environment"), h("embedded environment")
        );
        let state = parse_semantic_state(&text).unwrap();
        assert_eq!(state.package, "foo");
        assert_eq!(state.imports, vec![ImportDirective::From { package: "bar".into(), bindings: vec![ImportedBinding { name: "local_x".into(), binding: "x".into() }] }]);
        assert_eq!(state.bindings[0].origin, Some(BindingOrigin::Code));
        assert!(!state.bindings[0].supported);
    }
}
