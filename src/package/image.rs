use crate::package::{
    ExportMap, ImportBinding, ImportSpec, InstalledPackage, LifecycleMetadata, NativeComponent, NativeSafety,
    PackageIndex, S3Registration,
};
use crate::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingOrigin {
    Code,
    Sysdata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectKind {
    Closure,
    Null,
    Logical,
    Integer,
    Double,
    Complex,
    Character,
    Raw,
    Symbol,
    List,
    Pairlist,
    Language,
    Expression,
    Environment,
    Builtin,
    Special,
    Other(String),
    Unavailable,
}

impl ObjectKind {
    pub fn from_r_type(value: &str) -> Self {
        match value {
            "closure" => Self::Closure,
            "NULL" => Self::Null,
            "logical" => Self::Logical,
            "integer" => Self::Integer,
            "double" => Self::Double,
            "complex" => Self::Complex,
            "character" => Self::Character,
            "raw" => Self::Raw,
            "symbol" => Self::Symbol,
            "list" => Self::List,
            "pairlist" => Self::Pairlist,
            "language" => Self::Language,
            "expression" => Self::Expression,
            "environment" => Self::Environment,
            "builtin" => Self::Builtin,
            "special" => Self::Special,
            "unavailable" => Self::Unavailable,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn needs_air(&self) -> bool {
        matches!(self, Self::Closure)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectIssue {
    pub path: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureSource {
    pub formals: Arc<str>,
    pub body: Arc<str>,
    pub source: Arc<str>,
    pub environment: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddedClosureSource {
    pub path: String,
    pub source: Arc<str>,
    pub environment: String,
}

#[derive(Clone, Debug)]
pub struct BindingImage {
    pub name: String,
    pub origin: BindingOrigin,
    pub object_kind: ObjectKind,
    pub closure: Option<ClosureSource>,
    pub embedded_closures: Vec<EmbeddedClosureSource>,
    pub issues: Vec<ObjectIssue>,
}

#[derive(Clone, Debug)]
pub struct PackageImage {
    pub index: PackageIndex,
    pub bindings: HashMap<String, BindingImage>,
}

impl PackageImage {
    pub fn binding(&self, name: &str) -> Option<&BindingImage> {
        self.bindings.get(name)
    }
}

pub(crate) fn parse_package_image(text: &str, package: InstalledPackage) -> Result<PackageImage> {
    let mut header: Option<(String, bool, bool)> = None;
    let mut exports = ExportMap::new();
    let mut imports = Vec::<ImportSpec>::new();
    let mut import_all = HashMap::<String, usize>::new();
    let mut bindings = HashMap::<String, BindingImage>::new();
    let mut datasets = Vec::new();
    let mut files = Vec::new();
    let mut s3 = Vec::new();
    let mut dynlibs = Vec::new();

    for (line_no, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let kind = fields.next().unwrap_or_default();
        let values = fields
            .map(decode_hex)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|message| Error::Analysis(format!("image protocol line {}: {message}", line_no + 1)))?;
        match kind {
            "HEADER" => {
                require(kind, &values, 4, line_no)?;
                if values[0] != package.id.name || values[1] != package.id.version {
                    return Err(Error::Analysis(format!(
                        "installed image identity changed while inspecting {}: expected {} {}, got {} {}",
                        package.id.name, package.id.name, package.id.version, values[0], values[1]
                    )));
                }
                header = Some((values[1].clone(), parse_bool(&values[2])?, parse_bool(&values[3])?));
            }
            "EXPORT" => {
                require(kind, &values, 2, line_no)?;
                exports.insert(values[0].clone(), values[1].clone());
            }
            "IMPORT_ALL" => {
                require(kind, &values, 1, line_no)?;
                let index = imports.len();
                import_all.insert(values[0].clone(), index);
                imports.push(ImportSpec::All { package: values[0].clone(), except: Vec::new() });
            }
            "IMPORT_EXCEPT" => {
                require(kind, &values, 2, line_no)?;
                if let Some(index) = import_all.get(&values[0]).copied() {
                    if let ImportSpec::All { except, .. } = &mut imports[index] {
                        except.push(values[1].clone());
                    }
                }
            }
            "IMPORT_FROM" => {
                require(kind, &values, 3, line_no)?;
                let package_name = values[0].clone();
                let binding = ImportBinding { remote: values[1].clone(), local: values[2].clone() };
                if let Some(ImportSpec::From { bindings, .. }) = imports.iter_mut().find(|item| {
                    matches!(item, ImportSpec::From { package, .. } if package == &package_name)
                }) {
                    bindings.push(binding);
                } else {
                    imports.push(ImportSpec::From { package: package_name, bindings: vec![binding] });
                }
            }
            "BINDING" => {
                require(kind, &values, 3, line_no)?;
                let origin = match values[1].as_str() {
                    "code" => BindingOrigin::Code,
                    "sysdata" => BindingOrigin::Sysdata,
                    other => return Err(Error::Analysis(format!("unknown binding origin {other:?}"))),
                };
                bindings.insert(values[0].clone(), BindingImage {
                    name: values[0].clone(),
                    origin,
                    object_kind: ObjectKind::from_r_type(&values[2]),
                    closure: None,
                    embedded_closures: Vec::new(),
                    issues: Vec::new(),
                });
            }
            "CLOSURE" => {
                require(kind, &values, 5, line_no)?;
                let binding = bindings.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!("CLOSURE precedes BINDING for {}", values[0]))
                })?;
                binding.closure = Some(ClosureSource {
                    environment: values[1].clone(),
                    formals: Arc::from(values[2].clone()),
                    body: Arc::from(values[3].clone()),
                    source: Arc::from(values[4].clone()),
                });
            }
            "NESTED_CLOSURE" => {
                require(kind, &values, 4, line_no)?;
                let binding = bindings.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!("NESTED_CLOSURE precedes BINDING for {}", values[0]))
                })?;
                binding.embedded_closures.push(EmbeddedClosureSource {
                    path: values[1].clone(),
                    environment: values[2].clone(),
                    source: Arc::from(values[3].clone()),
                });
            }
            "BINDING_ISSUE" => {
                require(kind, &values, 4, line_no)?;
                let binding = bindings.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!("BINDING_ISSUE precedes BINDING for {}", values[0]))
                })?;
                binding.issues.push(ObjectIssue {
                    path: values[1].clone(),
                    kind: values[2].clone(),
                    detail: values[3].clone(),
                });
            }
            "DATASET" => {
                require(kind, &values, 1, line_no)?;
                datasets.push(values[0].clone());
            }
            "S3" => {
                require(kind, &values, 3, line_no)?;
                s3.push(S3Registration {
                    generic: values[0].clone(),
                    class: values[1].clone(),
                    method: values[2].clone(),
                });
            }
            "DYNLIB" => {
                require(kind, &values, 1, line_no)?;
                dynlibs.push(NativeComponent { name: values[0].clone(), safety: NativeSafety::Unanalyzed });
            }
            "FILE" => {
                require(kind, &values, 1, line_no)?;
                files.push(values[0].clone());
            }
            "PACKAGE_ISSUE" => {
                // Package-level unsupported object-system metadata is represented
                // later as an activation diagnostic. Keep the protocol forward-compatible.
            }
            other => return Err(Error::Analysis(format!("unknown installed-image record {other:?}"))),
        }
    }

    let (_, on_load, has_sysdata) = header.ok_or_else(|| Error::Analysis("installed image has no HEADER".into()))?;
    let mut binding_names = bindings.keys().cloned().collect::<Vec<_>>();
    binding_names.sort();
    datasets.sort();
    files.sort();
    files.dedup();

    let index = PackageIndex {
        description: package.description.clone(),
        package,
        exports,
        imports,
        s3,
        dynlibs,
        lifecycle: LifecycleMetadata { on_load },
        binding_names,
        datasets,
        files,
        has_sysdata,
    };
    Ok(PackageImage { index, bindings })
}

fn require(kind: &str, values: &[String], expected: usize, line_no: usize) -> Result<()> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(Error::Analysis(format!(
            "image protocol line {}: {kind} expected {expected} fields, got {}",
            line_no + 1,
            values.len()
        )))
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(Error::Analysis(format!("invalid protocol boolean {other:?}"))),
    }
}

fn decode_hex(value: &str) -> std::result::Result<String, String> {
    if value.len() % 2 != 0 {
        return Err(format!("odd-length hex field {value:?}"));
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = nibble(bytes[index]).ok_or_else(|| format!("invalid hex field {value:?}"))?;
        let low = nibble(bytes[index + 1]).ok_or_else(|| format!("invalid hex field {value:?}"))?;
        out.push((high << 4) | low);
        index += 2;
    }
    String::from_utf8(out).map_err(|error| format!("invalid UTF-8 field: {error}"))
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
