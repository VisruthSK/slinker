use crate::analysis::namespace::{NamespaceDirective, parse_namespace};
use crate::analysis::parser::{ParsedRFile, RParser};
use crate::analysis::source::{SourceId, Sources};
use crate::metadata::Description;
use crate::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AnalysisPackageId {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct RUnit {
    pub path: PathBuf,
    pub parsed: ParsedRFile,
}

#[derive(Debug, Clone)]
pub struct AnalysisPackage {
    pub id: AnalysisPackageId,
    pub description: Description,
    pub namespace: Vec<NamespaceDirective>,
    pub r_units: Vec<RUnit>,
    pub has_native: bool,
    pub has_sysdata: bool,
}

pub(crate) struct PreparedPackage {
    id: AnalysisPackageId,
    description: Description,
    namespace: Vec<NamespaceDirective>,
    r_units: Vec<PreparedRUnit>,
    has_native: bool,
    has_sysdata: bool,
}

struct PreparedRUnit {
    path: PathBuf,
    text: String,
}

pub(crate) struct PackageParsePlan {
    id: AnalysisPackageId,
    description: Description,
    namespace: Vec<NamespaceDirective>,
    r_units: Vec<ParseRUnit>,
    has_native: bool,
    has_sysdata: bool,
}

struct ParseRUnit {
    path: PathBuf,
    source: SourceId,
    text: String,
}

impl AnalysisPackage {
    pub fn load(root: &Path, parser: &impl RParser, sources: &mut Sources) -> Result<Self> {
        PreparedPackage::load(root)?.attach_sources(sources).parse(parser)
    }
}

impl PreparedPackage {
    pub(crate) fn load(root: &Path) -> Result<Self> {
        let description_path = root.join("DESCRIPTION");
        let description_text = fs::read_to_string(&description_path)
            .map_err(|source| Error::Io { path: description_path.clone(), source })?;
        let description = Description::parse(&description_text)
            .map_err(|source| Error::Metadata { path: description_path.clone(), source })?;
        let name = description
            .package()
            .ok_or_else(|| Error::Analysis(format!("{} has no Package field", description_path.display())))?
            .to_owned();
        let version = description.version().unwrap_or("unknown").to_owned();

        let namespace_path = root.join("NAMESPACE");
        let namespace_text = if namespace_path.is_file() {
            fs::read_to_string(&namespace_path)
                .map_err(|source| Error::Io { path: namespace_path.clone(), source })?
        } else {
            String::new()
        };
        let namespace = parse_namespace(&namespace_text)?;

        let r_units = ordered_r_files(root, &description)?
            .into_iter()
            .map(|path| {
                let text = fs::read_to_string(&path)
                    .map_err(|source| Error::Io { path: path.clone(), source })?;
                let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                Ok(PreparedRUnit { path: relative, text })
            })
            .collect::<Result<Vec<_>>>()?;

        let has_native = root.join("src").is_dir()
            || namespace
                .iter()
                .any(|directive| matches!(directive, NamespaceDirective::UseDynLib { .. }));

        Ok(Self {
            id: AnalysisPackageId { name, version },
            description,
            namespace,
            r_units,
            has_native,
            has_sysdata: root.join("R").join("sysdata.rda").is_file(),
        })
    }

    pub(crate) fn attach_sources(self, sources: &mut Sources) -> PackageParsePlan {
        let r_units = self
            .r_units
            .into_iter()
            .map(|unit| {
                let source = sources.add(unit.path.clone(), unit.text.clone());
                ParseRUnit { path: unit.path, source, text: unit.text }
            })
            .collect();

        PackageParsePlan {
            id: self.id,
            description: self.description,
            namespace: self.namespace,
            r_units,
            has_native: self.has_native,
            has_sysdata: self.has_sysdata,
        }
    }
}

impl PackageParsePlan {
    pub(crate) fn parse(self, parser: &impl RParser) -> Result<AnalysisPackage> {
        let r_units = self
            .r_units
            .into_iter()
            .map(|unit| {
                let parsed = parser.parse(unit.source, &unit.text)?;
                Ok(RUnit { path: unit.path, parsed })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(AnalysisPackage {
            id: self.id,
            description: self.description,
            namespace: self.namespace,
            r_units,
            has_native: self.has_native,
            has_sysdata: self.has_sysdata,
        })
    }
}

fn ordered_r_files(root: &Path, description: &Description) -> Result<Vec<PathBuf>> {
    let r = root.join("R");
    if !r.is_dir() {
        return Ok(Vec::new());
    }

    let mut all = Vec::new();
    for entry in fs::read_dir(&r).map_err(|source| Error::Io { path: r.clone(), source })? {
        let entry = entry.map_err(|source| Error::Io { path: r.clone(), source })?;
        let path = entry.path();
        if path.is_file()
            && matches!(
                path.extension()
                    .and_then(|x| x.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("r") | Some("s") | Some("q")
            )
        {
            all.push(path);
        }
    }
    all.sort();

    // Installed-image analysis writes canonical synthetic source units after
    // installation has already constructed the binding image. Source Collate
    // order does not apply to this representation.
    if root.join(".hrm-installed-image").is_file() {
        return Ok(all);
    }

    let Some(collate) = description.get("Collate") else {
        return Ok(all);
    };
    let ordered_names = parse_collate(collate);
    let mut ordered = Vec::new();
    for name in ordered_names {
        let path = r.join(&name);
        if !path.is_file() {
            return Err(Error::Analysis(format!(
                "Collate names missing R source {}",
                path.display()
            )));
        }
        ordered.push(path);
    }
    Ok(ordered)
}

fn parse_collate(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escape = false;
    for ch in value.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' && quote.is_some() {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                if !current.trim().is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                quote = Some(ch);
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}
