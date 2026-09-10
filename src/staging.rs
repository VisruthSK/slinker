use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    Description, MetadataError, NativeHazard, NativeScanError, RToolchain, Target,
    TargetEnvironment, ToolchainError, scan_native_tree,
};

#[derive(Clone, Debug)]
pub struct StageRequest {
    pub source: PathBuf,
    pub work_dir: PathBuf,
    pub configure_args: Vec<String>,
    pub configure_vars: BTreeMap<String, String>,
    /// Libraries containing already-staged candidate dependencies.
    pub dependency_libraries: Vec<PathBuf>,
    /// Libraries that make up the declared target-provided package set.
    pub target_libraries: Vec<PathBuf>,
    /// Semantic target captured from the R process that supplied target libraries.
    pub expected_target: Option<Target>,
    /// Byte compilation is off by default because retained closures are rebuilt
    /// from source-level bodies into canonical synthetic environments.
    pub byte_compile: bool,
    pub keep_source: bool,
}

impl StageRequest {
    pub fn new(source: impl Into<PathBuf>, work_dir: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            work_dir: work_dir.into(),
            configure_args: Vec::new(),
            configure_vars: BTreeMap::new(),
            dependency_libraries: Vec::new(),
            target_libraries: Vec::new(),
            expected_target: None,
            byte_compile: false,
            keep_source: false,
        }
    }

    pub fn use_target_environment(&mut self, target: &TargetEnvironment) {
        self.target_libraries = target.libraries.clone();
        self.expected_target = Some(target.target.clone());
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveMetadata {
    pub description: String,
    pub parsed_description: Description,
    pub namespace: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredSourceView {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
    pub package_bytes: u64,
    pub r_source_bytes: u64,
    pub effective: EffectiveMetadata,
    /// Native source markers that require compatibility review. This is a
    /// prefilter, not a proof that unmarked native code is safe.
    pub native_hazards: Vec<NativeHazard>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledSemanticView {
    pub root: PathBuf,
    pub package_rds: PathBuf,
    pub namespace_rds: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedPackage {
    pub name: String,
    pub version: String,
    pub target: Target,
    pub library: PathBuf,
    /// Exact non-base library search path used for installation and semantic inspection.
    pub semantic_libraries: Vec<PathBuf>,
    pub configured: ConfiguredSourceView,
    pub installed: InstalledSemanticView,
}

#[derive(Debug)]
pub enum StageError {
    Io(std::io::Error),
    Toolchain(ToolchainError),
    MissingSource(PathBuf),
    MissingField { path: PathBuf, field: &'static str },
    MissingInstalledPackage(PathBuf),
    MissingInstalledMetadata(PathBuf),
    TargetMismatch { expected: Target, actual: Target },
    JoinPaths(std::env::JoinPathsError),
    Metadata { path: PathBuf, source: MetadataError },
    NativeScan(NativeScanError),
}

impl RToolchain {
    /// Stage packages in caller-supplied dependency order. Requests that share
    /// a work directory automatically share its staging library.
    pub fn stage_in_order(&self, requests: &[StageRequest]) -> Result<Vec<StagedPackage>, StageError> {
        requests.iter().map(|request| self.stage(request)).collect()
    }

    pub fn stage(&self, request: &StageRequest) -> Result<StagedPackage, StageError> {
        if !request.source.is_dir() {
            return Err(StageError::MissingSource(request.source.clone()));
        }

        let target = self.probe().map_err(StageError::Toolchain)?;
        if let Some(expected) = &request.expected_target {
            if expected != &target {
                return Err(StageError::TargetMismatch { expected: expected.clone(), actual: target });
            }
        }
        let configured_parent = request.work_dir.join("configured");
        let library = request.work_dir.join("library");
        let empty_library = request.work_dir.join("empty-library");
        fs::create_dir_all(&configured_parent).map_err(StageError::Io)?;
        fs::create_dir_all(&library).map_err(StageError::Io)?;
        fs::create_dir_all(&empty_library).map_err(StageError::Io)?;

        let source_description = request.source.join("DESCRIPTION");
        let source_description_text = fs::read_to_string(&source_description).map_err(StageError::Io)?;
        let source_description_parsed = Description::parse(&source_description_text)
            .map_err(|source| StageError::Metadata { path: source_description.clone(), source })?;
        let source_package = source_description_parsed
            .package()
            .ok_or_else(|| StageError::MissingField { path: source_description.clone(), field: "Package" })?
            .to_owned();
        let configured_root = configured_parent.join(&source_package);
        if configured_root.exists() {
            fs::remove_dir_all(&configured_root).map_err(StageError::Io)?;
        }
        copy_tree(&request.source, &configured_root).map_err(StageError::Io)?;

        let mut visible_libraries = Vec::new();
        visible_libraries.push(library.clone());
        visible_libraries.extend(request.dependency_libraries.iter().cloned());
        visible_libraries.extend(request.target_libraries.iter().cloned());
        let r_libs = std::env::join_paths(&visible_libraries).map_err(StageError::JoinPaths)?;
        let empty_profile = request.work_dir.join("empty.Rprofile");
        let empty_environ = request.work_dir.join("empty.Renviron");
        fs::write(&empty_profile, "").map_err(StageError::Io)?;
        fs::write(&empty_environ, "").map_err(StageError::Io)?;

        let mut command = self.command();
        command
            .env("R_LIBS", r_libs)
            .env("R_LIBS_USER", &empty_library)
            .env("R_LIBS_SITE", &empty_library)
            .env("R_PROFILE", &empty_profile)
            .env("R_ENVIRON", &empty_environ)
            .arg("CMD")
            .arg("INSTALL")
            .arg("-l")
            .arg(&library)
            .arg("--no-test-load")
            .arg("--no-multiarch");
        if request.byte_compile {
            command.arg("--byte-compile");
        } else {
            command.arg("--no-byte-compile");
        }
        if request.keep_source {
            command.arg("--with-keep.source");
        } else {
            command.arg("--without-keep.source");
        }
        if !request.configure_args.is_empty() {
            let args = request.configure_args.iter().map(|arg| shell_quote(arg)).collect::<Vec<_>>().join(" ");
            command.arg(format!("--configure-args={args}"));
        }
        if !request.configure_vars.is_empty() {
            let vars = request
                .configure_vars
                .iter()
                .map(|(key, value)| format!("{key}={}", shell_quote(value)))
                .collect::<Vec<_>>()
                .join(" ");
            command.arg(format!("--configure-vars={vars}"));
        }
        command.arg(&configured_root);
        crate::toolchain::checked_output(&mut command).map_err(StageError::Toolchain)?;

        let configured_description = configured_root.join("DESCRIPTION");
        let configured_description_text = fs::read_to_string(&configured_description).map_err(StageError::Io)?;
        let configured_description_parsed = Description::parse(&configured_description_text)
            .map_err(|source| StageError::Metadata { path: configured_description.clone(), source })?;
        let name = configured_description_parsed
            .package()
            .ok_or_else(|| StageError::MissingField {
                path: configured_description.clone(),
                field: "Package",
            })?
            .to_owned();
        let installed_root = library.join(&name);
        if !installed_root.is_dir() {
            return Err(StageError::MissingInstalledPackage(installed_root));
        }

        let installed_description = installed_root.join("DESCRIPTION");
        let package_rds = installed_root.join("Meta/package.rds");
        let namespace_rds = installed_root.join("Meta/nsInfo.rds");
        for metadata in [&package_rds, &namespace_rds] {
            if !metadata.is_file() {
                return Err(StageError::MissingInstalledMetadata(metadata.clone()));
            }
        }
        let installed_description_text = fs::read_to_string(&installed_description).map_err(StageError::Io)?;
        let installed_description_parsed = Description::parse(&installed_description_text)
            .map_err(|source| StageError::Metadata { path: installed_description.clone(), source })?;
        let version = installed_description_parsed
            .version()
            .or_else(|| configured_description_parsed.version())
            .ok_or_else(|| StageError::MissingField {
                path: installed_description.clone(),
                field: "Version",
            })?
            .to_owned();

        let description = configured_description_text;
        let parsed_description = configured_description_parsed;
        let namespace = fs::read_to_string(configured_root.join("NAMESPACE"))
            .or_else(|_| fs::read_to_string(installed_root.join("NAMESPACE")))
            .unwrap_or_default();

        let files = inventory(&configured_root).map_err(StageError::Io)?;
        let (package_bytes, r_source_bytes) = inventory_bytes(&configured_root, &files).map_err(StageError::Io)?;
        let native_hazards = scan_native_tree(&configured_root).map_err(StageError::NativeScan)?;
        Ok(StagedPackage {
            name,
            version,
            target,
            library,
            semantic_libraries: visible_libraries,
            configured: ConfiguredSourceView {
                root: configured_root,
                files,
                package_bytes,
                r_source_bytes,
                effective: EffectiveMetadata { description, parsed_description, namespace },
                native_hazards,
            },
            installed: InstalledSemanticView {
                root: installed_root.clone(),
                package_rds,
                namespace_rds,
            },
        })
    }
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = destination.join(entry.file_name());
        if ty.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if ty.is_symlink() {
            let target = fs::read_link(entry.path())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, to)?;
            #[cfg(windows)]
            {
                let resolved = entry.path().canonicalize()?;
                let result = if resolved.is_dir() {
                    std::os::windows::fs::symlink_dir(&target, &to)
                } else {
                    std::os::windows::fs::symlink_file(&target, &to)
                };

                // Creating symlinks on Windows can require Developer Mode or
                // SeCreateSymbolicLinkPrivilege. For a staging copy, fall back
                // to dereferencing only when Windows refuses link creation.
                // Other errors remain visible instead of silently changing
                // source semantics.
                if let Err(error) = result {
                    if error.kind() != std::io::ErrorKind::PermissionDenied {
                        return Err(error);
                    }
                    if resolved.is_dir() {
                        copy_tree(&resolved, &to)?;
                    } else {
                        fs::copy(&resolved, &to)?;
                    }
                }
            }
        } else if ty.is_file() {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    // R passes --configure-args/--configure-vars to a Bourne-style shell on
    // both Unix and Windows (via Rtools). Single-quote each logical argument
    // so spaces, backslashes, and shell metacharacters survive intact.
    if value.is_empty() {
        return "''".to_owned();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\"'\"'");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn inventory(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    fn visit(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let ty = entry.file_type()?;
            if ty.is_dir() {
                visit(root, &entry.path(), files)?;
            } else {
                files.push(entry.path().strip_prefix(root).unwrap().to_path_buf());
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn inventory_bytes(root: &Path, files: &[PathBuf]) -> std::io::Result<(u64, u64)> {
    let mut package_bytes = 0u64;
    let mut r_source_bytes = 0u64;
    for relative in files {
        let size = fs::metadata(root.join(relative))?.len();
        package_bytes += size;
        let is_r_source = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            == Some("R")
            && relative
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "r" | "s" | "q"));
        if is_r_source {
            r_source_bytes += size;
        }
    }
    Ok((package_bytes, r_source_bytes))
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "staging I/O error: {error}"),
            Self::Toolchain(error) => write!(f, "staging failed: {error}"),
            Self::MissingSource(path) => write!(f, "package source does not exist: {}", path.display()),
            Self::MissingField { path, field } => write!(f, "missing {field} in {}", path.display()),
            Self::MissingInstalledPackage(path) => write!(f, "R did not produce installed package at {}", path.display()),
            Self::MissingInstalledMetadata(path) => write!(f, "R installation is missing semantic metadata {}", path.display()),
            Self::TargetMismatch { expected, actual } => write!(
                f,
                "staging target mismatch: expected R {}/{}/{}, got R {}/{}/{}",
                expected.r_version, expected.os, expected.arch, actual.r_version, actual.os, actual.arch
            ),
            Self::JoinPaths(error) => write!(f, "invalid R library search path: {error}"),
            Self::Metadata { path, source } => write!(f, "invalid DESCRIPTION {}: {source}", path.display()),
            Self::NativeScan(error) => write!(f, "native compatibility prefilter failed: {error}"),
        }
    }
}

impl std::error::Error for StageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Toolchain(error) => Some(error),
            Self::JoinPaths(error) => Some(error),
            Self::Metadata { source, .. } => Some(source),
            Self::NativeScan(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn quotes_configure_shell_arguments() {
        assert_eq!(shell_quote("C:\\Program Files\\R"), "'C:\\Program Files\\R'");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
        assert_eq!(shell_quote(""), "''");
    }
}
