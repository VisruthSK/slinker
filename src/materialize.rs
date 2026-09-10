use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    ExportedBinding, ImportDirective, InspectError, RToolchain, SemanticSnapshot, Target,
    InstalledPackage, ToolchainError,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageMaterialization {
    pub name: String,
    pub recipes_rds: PathBuf,
    pub retained_bindings: Vec<String>,
    pub retained_datasets: Vec<String>,
    pub exports: Vec<ExportedBinding>,
    pub imports: Vec<ImportDirective>,
    pub modeled_on_load: bool,
}

impl PackageMaterialization {
    pub fn from_snapshot<I, S>(snapshot: &SemanticSnapshot, retained: I) -> Result<Self, MaterializeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::from_snapshot_inner(snapshot, retained, false)
    }

    /// Accept an `.onLoad` only after the analyzer has modeled it as a runtime
    /// activation effect. The hook is retained but never run during baseline
    /// materialization.
    pub fn from_snapshot_with_modeled_on_load<I, S>(
        snapshot: &SemanticSnapshot,
        retained: I,
    ) -> Result<Self, MaterializeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::from_snapshot_inner(snapshot, retained, true)
    }

    fn from_snapshot_inner<I, S>(
        snapshot: &SemanticSnapshot,
        retained: I,
        modeled_on_load: bool,
    ) -> Result<Self, MaterializeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut retained_bindings = retained
            .into_iter()
            .map(|name| name.as_ref().to_owned())
            .collect::<Vec<_>>();
        retained_bindings.sort();
        retained_bindings.dedup();

        if !snapshot.state.package_issues.is_empty() {
            return Err(MaterializeError::UnsupportedPackage {
                package: snapshot.state.package.clone(),
                issues: format_issues(&snapshot.state.package_issues),
            });
        }
        if !snapshot.state.dynlibs.is_empty() {
            return Err(MaterializeError::NativePackageUnsupported {
                package: snapshot.state.package.clone(),
                dynlibs: snapshot.state.dynlibs.clone(),
            });
        }
        if snapshot.state.has_on_load && !modeled_on_load {
            return Err(MaterializeError::UnmodeledOnLoad(snapshot.state.package.clone()));
        }
        if snapshot.state.has_on_load && !retained_bindings.iter().any(|name| name == ".onLoad") {
            retained_bindings.push(".onLoad".into());
            retained_bindings.sort();
        }

        for name in &retained_bindings {
            let object = snapshot
                .state
                .bindings
                .iter()
                .find(|binding| &binding.name == name)
                .ok_or_else(|| MaterializeError::UnknownBinding {
                    package: snapshot.state.package.clone(),
                    binding: name.clone(),
                })?;
            if !object.supported {
                return Err(MaterializeError::UnsupportedBinding {
                    package: snapshot.state.package.clone(),
                    binding: name.clone(),
                    issues: format_issues(&object.issues),
                });
            }
        }

        Ok(Self {
            name: snapshot.state.package.clone(),
            recipes_rds: snapshot.recipes_rds.clone(),
            retained_bindings,
            retained_datasets: Vec::new(),
            exports: snapshot.state.exports.clone(),
            imports: snapshot.state.imports.clone(),
            modeled_on_load: snapshot.state.has_on_load && modeled_on_load,
        })
    }

    pub fn retain_datasets<I, S>(mut self, snapshot: &SemanticSnapshot, datasets: I) -> Result<Self, MaterializeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut names = datasets
            .into_iter()
            .map(|name| name.as_ref().to_owned())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        for name in &names {
            let object = snapshot
                .state
                .datasets
                .iter()
                .find(|dataset| &dataset.name == name)
                .ok_or_else(|| MaterializeError::UnknownDataset {
                    package: snapshot.state.package.clone(),
                    dataset: name.clone(),
                })?;
            if !object.supported {
                return Err(MaterializeError::UnsupportedDataset {
                    package: snapshot.state.package.clone(),
                    dataset: name.clone(),
                    issues: format_issues(&object.issues),
                });
            }
        }
        self.retained_datasets = names;
        Ok(self)
    }
}

fn format_issues(issues: &[crate::ObjectIssue]) -> Vec<String> {
    issues
        .iter()
        .map(|issue| format!("{}: {} ({})", issue.path, issue.kind, issue.detail))
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationRequest {
    /// Eager validation/debug snapshot of the complete synthetic environment graph.
    pub output_rds: PathBuf,
    /// Base path for an R lazy-load database. The materializer writes `.rdb` and `.rdx`.
    pub lazy_db_base: Option<PathBuf>,
    pub packages: Vec<PackageMaterialization>,
    /// R/OS/architecture identity this artifact is allowed to target.
    pub target: Option<Target>,
    /// Exact external package identities permitted at runtime.
    pub target_provided: Vec<InstalledPackage>,
}

impl MaterializationRequest {
    pub fn new(output_rds: impl Into<PathBuf>) -> Self {
        let output_rds = output_rds.into();
        let lazy_db_base = Some(output_rds.with_extension("lazy"));
        Self {
            output_rds,
            lazy_db_base,
            packages: Vec::new(),
            target: None,
            target_provided: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializedArtifact {
    pub baseline_rds: PathBuf,
    pub lazy_db_base: Option<PathBuf>,
    pub lazy_rdb: Option<PathBuf>,
    pub lazy_rdx: Option<PathBuf>,
    pub package_names: Vec<String>,
}

#[derive(Debug)]
pub enum MaterializeError {
    Io(std::io::Error),
    Toolchain(ToolchainError),
    Inspect(InspectError),
    UnknownBinding { package: String, binding: String },
    UnsupportedBinding { package: String, binding: String, issues: Vec<String> },
    UnknownDataset { package: String, dataset: String },
    UnsupportedDataset { package: String, dataset: String, issues: Vec<String> },
    UnsupportedPackage { package: String, issues: Vec<String> },
    UnmodeledOnLoad(String),
    NativePackageUnsupported { package: String, dynlibs: Vec<String> },
    DuplicatePackage(String),
    DuplicateTargetPackage(String),
    TargetMismatch { expected: Target, actual: Target },
    MissingRecipe(PathBuf),
    MissingOutput(PathBuf),
    NonUtf8Path(PathBuf),
}

impl RToolchain {
    pub fn materialize(&self, request: &MaterializationRequest) -> Result<MaterializedArtifact, MaterializeError> {
        if let Some(expected) = &request.target {
            let actual = self.probe().map_err(MaterializeError::Toolchain)?;
            if expected != &actual {
                return Err(MaterializeError::TargetMismatch { expected: expected.clone(), actual });
            }
        }
        ensure_parent(&request.output_rds)?;
        if let Some(lazy_db_base) = &request.lazy_db_base {
            ensure_parent(lazy_db_base)?;
        }

        let mut names = BTreeSet::new();
        for package in &request.packages {
            if !names.insert(package.name.clone()) {
                return Err(MaterializeError::DuplicatePackage(package.name.clone()));
            }
            if !package.recipes_rds.is_file() {
                return Err(MaterializeError::MissingRecipe(package.recipes_rds.clone()));
            }
        }

        let mut provided_names = BTreeSet::new();
        for package in &request.target_provided {
            if !provided_names.insert(package.name.clone()) {
                return Err(MaterializeError::DuplicateTargetPackage(package.name.clone()));
            }
        }

        let spec_path = request.output_rds.with_extension("materialize-spec.R");
        let runtime_dir = request.output_rds.with_extension("runtime");
        fs::write(&spec_path, render_spec(request)?).map_err(MaterializeError::Io)?;
        fs::create_dir_all(&runtime_dir).map_err(MaterializeError::Io)?;
        let result = self.run_runtime(
            &runtime_dir,
            "materialize",
            [spec_path.as_os_str()],
            false,
        );
        let _ = fs::remove_dir_all(&runtime_dir);
        result.map_err(MaterializeError::Toolchain)?;

        if !request.output_rds.is_file() {
            return Err(MaterializeError::MissingOutput(request.output_rds.clone()));
        }
        let (lazy_rdb, lazy_rdx) = if let Some(base) = &request.lazy_db_base {
            let rdb = append_extension(base, "rdb");
            let rdx = append_extension(base, "rdx");
            for path in [&rdb, &rdx] {
                if !path.is_file() {
                    return Err(MaterializeError::MissingOutput(path.clone()));
                }
            }
            (Some(rdb), Some(rdx))
        } else {
            (None, None)
        };

        Ok(MaterializedArtifact {
            baseline_rds: request.output_rds.clone(),
            lazy_db_base: request.lazy_db_base.clone(),
            lazy_rdb,
            lazy_rdx,
            package_names: names.into_iter().collect(),
        })
    }
}

fn ensure_parent(path: &Path) -> Result<(), MaterializeError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(MaterializeError::Io)?;
    }
    Ok(())
}

fn append_extension(base: &Path, extension: &str) -> PathBuf {
    let mut value = base.as_os_str().to_os_string();
    value.push(".");
    value.push(extension);
    PathBuf::from(value)
}

fn render_spec(request: &MaterializationRequest) -> Result<String, MaterializeError> {
    let mut out = String::new();
    out.push_str("spec <- list(\n");
    out.push_str("  output = ");
    out.push_str(&r_path(&request.output_rds)?);
    out.push_str(",\n  lazy_db = ");
    match &request.lazy_db_base {
        Some(path) => out.push_str(&r_path(path)?),
        None => out.push_str("NULL"),
    }

    out.push_str(",\n  provided = list(");
    for (index, package) in request.target_provided.iter().enumerate() {
        if index != 0 { out.push_str(", "); }
        out.push_str("list(name = ");
        out.push_str(&r_string(&package.name));
        out.push_str(", version = ");
        out.push_str(&r_string(&package.version));
        out.push_str(", library = ");
        out.push_str(&r_path(&package.library)?);
        out.push(')');
    }
    out.push_str("),\n  packages = list(\n");

    for (index, package) in request.packages.iter().enumerate() {
        if index != 0 { out.push_str(",\n"); }
        out.push_str("    list(name = ");
        out.push_str(&r_string(&package.name));
        out.push_str(", recipes = ");
        out.push_str(&r_path(&package.recipes_rds)?);
        out.push_str(", modeled_on_load = ");
        out.push_str(if package.modeled_on_load { "TRUE" } else { "FALSE" });
        out.push_str(", retained = ");
        render_strings(&mut out, package.retained_bindings.iter().map(String::as_str));
        out.push_str(", datasets = ");
        render_strings(&mut out, package.retained_datasets.iter().map(String::as_str));
        out.push_str(", exports = list(");
        for (export_index, export) in package.exports.iter().enumerate() {
            if export_index != 0 { out.push_str(", "); }
            out.push_str("list(name = ");
            out.push_str(&r_string(&export.name));
            out.push_str(", binding = ");
            out.push_str(&r_string(&export.binding));
            out.push(')');
        }
        out.push(')');
        out.push_str(", imports = list(");
        for (import_index, import) in package.imports.iter().enumerate() {
            if import_index != 0 { out.push_str(", "); }
            match import {
                ImportDirective::All { package, except } => {
                    out.push_str("list(kind = \"all\", package = ");
                    out.push_str(&r_string(package));
                    out.push_str(", except = ");
                    render_strings(&mut out, except.iter().map(String::as_str));
                    out.push(')');
                }
                ImportDirective::From { package, bindings } => {
                    out.push_str("list(kind = \"from\", package = ");
                    out.push_str(&r_string(package));
                    out.push_str(", bindings = list(");
                    for (binding_index, binding) in bindings.iter().enumerate() {
                        if binding_index != 0 { out.push_str(", "); }
                        out.push_str("list(name = ");
                        out.push_str(&r_string(&binding.name));
                        out.push_str(", binding = ");
                        out.push_str(&r_string(&binding.binding));
                        out.push(')');
                    }
                    out.push_str("))");
                }
            }
        }
        out.push_str("))");
    }

    out.push_str("\n  )\n)\n");
    Ok(out)
}

fn render_strings<'a, I>(out: &mut String, values: I)
where
    I: IntoIterator<Item = &'a str>,
{
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        out.push_str("character()");
        return;
    }
    out.push_str("c(");
    for (index, value) in values.iter().enumerate() {
        if index != 0 { out.push_str(", "); }
        out.push_str(&r_string(value));
    }
    out.push(')');
}

fn r_path(path: &Path) -> Result<String, MaterializeError> {
    let value = path
        .to_str()
        .ok_or_else(|| MaterializeError::NonUtf8Path(path.to_path_buf()))?;
    Ok(r_string(value))
}

fn r_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

impl fmt::Display for MaterializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "materialization I/O error: {error}"),
            Self::Toolchain(error) => write!(f, "materialization failed: {error}"),
            Self::Inspect(error) => write!(f, "materialization inspection failed: {error}"),
            Self::UnknownBinding { package, binding } => write!(f, "{package}::{binding} is not present in inspected baseline state"),
            Self::UnsupportedBinding { package, binding, issues } => write!(f, "cannot materialize {package}::{binding}: {}", issues.join("; ")),
            Self::UnknownDataset { package, dataset } => write!(f, "dataset {package}::{dataset} is not present in inspected baseline state"),
            Self::UnsupportedDataset { package, dataset, issues } => write!(f, "cannot materialize dataset {package}::{dataset}: {}", issues.join("; ")),
            Self::UnsupportedPackage { package, issues } => write!(f, "cannot materialize package {package}: {}", issues.join("; ")),
            Self::UnmodeledOnLoad(package) => write!(f, "cannot materialize {package}: .onLoad has no modeled runtime activation semantics"),
            Self::NativePackageUnsupported { package, dynlibs } => write!(f, "cannot materialize native package {package} before native identity/build planning: {}", dynlibs.join(", ")),
            Self::DuplicatePackage(package) => write!(f, "duplicate materialization package {package}"),
            Self::DuplicateTargetPackage(package) => write!(f, "duplicate target-provided package {package}"),
            Self::TargetMismatch { expected, actual } => write!(
                f,
                "materialization target mismatch: expected R {}/{}/{}, got R {}/{}/{}",
                expected.r_version, expected.os, expected.arch, actual.r_version, actual.os, actual.arch
            ),
            Self::MissingRecipe(path) => write!(f, "materialization recipe does not exist: {}", path.display()),
            Self::MissingOutput(path) => write!(f, "materializer did not produce {}", path.display()),
            Self::NonUtf8Path(path) => write!(f, "R materialization requires a Unicode path: {}", path.display()),
        }
    }
}

impl std::error::Error for MaterializeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Toolchain(error) => Some(error),
            Self::Inspect(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::{
        MaterializationRequest, ObjectState, SemanticSnapshot, SemanticState, InstalledPackage,
    };

    use super::{append_extension, r_string, render_spec, MaterializeError, PackageMaterialization};

    #[test]
    fn quotes_r_strings() {
        assert_eq!(r_string("a\\b\"c\n"), "\"a\\\\b\\\"c\\n\"");
    }

    #[test]
    fn quotes_windows_paths_for_r() {
        assert_eq!(r_string(r"C:\Program Files\R\library"), r#""C:\\Program Files\\R\\library""#);
    }

    #[test]
    fn appends_lazy_db_extensions() {
        assert_eq!(append_extension(Path::new("x/hermetic"), "rdb"), PathBuf::from("x/hermetic.rdb"));
    }

    #[test]
    fn renders_exact_target_identity() {
        let mut request = MaterializationRequest::new("out/baseline.rds");
        request.target_provided.push(InstalledPackage {
            name: "stats".into(),
            version: "4.6.1".into(),
            library: PathBuf::from("/opt/R/library"),
        });
        let spec = render_spec(&request).unwrap();
        assert!(spec.contains("name = \"stats\", version = \"4.6.1\", library = \"/opt/R/library\""));
    }


    fn snapshot(has_on_load: bool, dynlibs: Vec<String>) -> SemanticSnapshot {
        SemanticSnapshot {
            recipes_rds: PathBuf::from("recipes.rds"),
            analysis_root: PathBuf::from("analysis"),
            state: SemanticState {
                package: "foo".into(),
                version: "1.0.0".into(),
                has_on_load,
                exports: Vec::new(),
                imports: Vec::new(),
                bindings: vec![ObjectState {
                    name: ".onLoad".into(),
                    type_name: "closure".into(),
                    supported: true,
                    origin: None,
                    issues: Vec::new(),
                    closure_environments: Vec::new(),
                }],
                datasets: Vec::new(),
                package_issues: Vec::new(),
                s3: Vec::new(),
                dynlibs,
                resources: Vec::new(),
            },
        }
    }

    #[test]
    fn rejects_unmodeled_on_load() {
        let error = PackageMaterialization::from_snapshot(&snapshot(true, Vec::new()), std::iter::empty::<&str>())
            .unwrap_err();
        assert!(matches!(error, MaterializeError::UnmodeledOnLoad(package) if package == "foo"));
    }

    #[test]
    fn modeled_on_load_is_retained_without_running_it() {
        let package = PackageMaterialization::from_snapshot_with_modeled_on_load(
            &snapshot(true, Vec::new()),
            std::iter::empty::<&str>(),
        )
        .unwrap();
        assert!(package.modeled_on_load);
        assert_eq!(package.retained_bindings, vec![".onLoad"]);
    }

    #[test]
    fn rejects_native_package_before_native_planning() {
        let error = PackageMaterialization::from_snapshot(
            &snapshot(false, vec!["foo".into()]),
            std::iter::empty::<&str>(),
        )
        .unwrap_err();
        assert!(matches!(error, MaterializeError::NativePackageUnsupported { package, .. } if package == "foo"));
    }
}
