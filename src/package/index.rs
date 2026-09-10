use crate::Description;
use crate::package::{BindingName, InstalledPackage};
use std::collections::BTreeMap;

pub type ExportMap = BTreeMap<String, BindingName>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportBinding {
    pub local: String,
    pub remote: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportSpec {
    All { package: String, except: Vec<String> },
    From { package: String, bindings: Vec<ImportBinding> },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct S3Registration {
    pub generic: String,
    pub class: String,
    pub method: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NativeFacts {
    pub callbacks: Vec<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum NativeSafety {
    Unanalyzed,
    Safe(NativeFacts),
    Unsupported(Vec<String>),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NativeComponent {
    pub name: String,
    pub safety: NativeSafety,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LifecycleMetadata {
    pub on_load: bool,
}

#[derive(Clone, Debug)]
pub struct PackageIndex {
    pub package: InstalledPackage,
    pub description: Description,
    pub exports: ExportMap,
    pub imports: Vec<ImportSpec>,
    pub s3: Vec<S3Registration>,
    pub dynlibs: Vec<NativeComponent>,
    pub lifecycle: LifecycleMetadata,
    pub binding_names: Vec<BindingName>,
    pub datasets: Vec<String>,
    /// Installed package-relative files/directories. These are metadata for
    /// resolving resource operations; they are not retained semantic resources.
    pub files: Vec<String>,
    pub has_sysdata: bool,
}

impl PackageIndex {
    pub fn import_from(&self, local: &str) -> Option<(&str, &str)> {
        self.imports.iter().find_map(|import| match import {
            ImportSpec::From { package, bindings } => bindings
                .iter()
                .find(|binding| binding.local == local)
                .map(|binding| (package.as_str(), binding.remote.as_str())),
            ImportSpec::All { .. } => None,
        })
    }
}

pub(crate) fn parse_package_index(text: &str, package: InstalledPackage) -> crate::Result<PackageIndex> {
    use crate::Error;

    let mut header: Option<(bool, bool)> = None;
    let mut exports = ExportMap::new();
    let mut imports = Vec::<ImportSpec>::new();
    let mut import_all = std::collections::HashMap::<String, usize>::new();
    let mut binding_names = Vec::new();
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
            .map_err(|message| Error::Analysis(format!("index protocol line {}: {message}", line_no + 1)))?;
        match kind {
            "HEADER" => {
                require(kind, &values, 4, line_no)?;
                if values[0] != package.id.name || values[1] != package.id.version {
                    return Err(Error::Analysis(format!(
                        "installed index identity changed while inspecting {}: expected {} {}, got {} {}",
                        package.id.name, package.id.name, package.id.version, values[0], values[1]
                    )));
                }
                header = Some((parse_bool(&values[2])?, parse_bool(&values[3])?));
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
            "BINDING_NAME" => {
                require(kind, &values, 1, line_no)?;
                binding_names.push(values[0].clone());
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
            "PACKAGE_ISSUE" => {}
            other => return Err(Error::Analysis(format!("unknown installed-index record {other:?}"))),
        }
    }

    let (on_load, has_sysdata) = header.ok_or_else(|| Error::Analysis("installed index has no HEADER".into()))?;
    binding_names.sort();
    binding_names.dedup();
    datasets.sort();
    datasets.dedup();
    files.sort();
    files.dedup();

    Ok(PackageIndex {
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
    })
}

fn require(kind: &str, values: &[String], expected: usize, line_no: usize) -> crate::Result<()> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(crate::Error::Analysis(format!(
            "index protocol line {}: {kind} expected {expected} fields, got {}",
            line_no + 1,
            values.len()
        )))
    }
}

fn parse_bool(value: &str) -> crate::Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(crate::Error::Analysis(format!("invalid protocol boolean {other:?}"))),
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
