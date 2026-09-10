use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use hrm::{Description, InstalledPackage, MetadataError, TargetEnvironment};

#[derive(Clone, Debug)]
pub struct ResolvedPackage {
    pub installed: InstalledPackage,
    pub root: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct Resolution {
    /// Runtime packages heRmetic will internalize, in dependency-first order.
    pub internalized: Vec<ResolvedPackage>,
    /// Packages supplied by R itself (`Priority: base`).
    pub platform: BTreeMap<String, String>,
    /// Third-party packages explicitly left external by the user.
    pub target_provided: BTreeMap<String, String>,
}


#[derive(Debug)]
pub enum ResolveError {
    Io { path: PathBuf, source: std::io::Error },
    Metadata { path: PathBuf, source: MetadataError },
    MissingPackage(String),
    MissingExplicitTarget(String),
    ExplicitTargetClosure { package: String, dependency: String },
    Cycle(Vec<String>),
}

pub fn resolve_installed_closure(
    target: &TargetEnvironment,
    root_description: &Description,
    additional: &BTreeSet<String>,
    explicit_target: &BTreeSet<String>,
) -> Result<Resolution, ResolveError> {
    let mut resolver = Resolver {
        target,
        explicit_target,
        resolution: Resolution::default(),
        state: BTreeMap::new(),
        stack: Vec::new(),
    };

    for package in explicit_target {
        let installed = target
            .package(package)
            .ok_or_else(|| ResolveError::MissingExplicitTarget(package.clone()))?;
        resolver
            .resolution
            .target_provided
            .insert(package.clone(), installed.version.clone());
    }

    for dependency in runtime_dependencies(root_description)? {
        resolver.visit(&dependency)?;
    }
    for package in additional {
        resolver.visit(package)?;
    }

    Ok(resolver.resolution)
}

struct Resolver<'a> {
    target: &'a TargetEnvironment,
    explicit_target: &'a BTreeSet<String>,
    resolution: Resolution,
    state: BTreeMap<String, VisitState>,
    stack: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Done,
}

impl Resolver<'_> {
    fn visit(&mut self, name: &str) -> Result<(), ResolveError> {
        if name == "R" {
            return Ok(());
        }
        if self.state.get(name) == Some(&VisitState::Done) {
            return Ok(());
        }
        if self.state.get(name) == Some(&VisitState::Visiting) {
            let start = self.stack.iter().position(|item| item == name).unwrap_or(0);
            let mut cycle = self.stack[start..].to_vec();
            cycle.push(name.to_owned());
            return Err(ResolveError::Cycle(cycle));
        }

        let installed = self
            .target
            .package(name)
            .cloned()
            .ok_or_else(|| ResolveError::MissingPackage(name.to_owned()))?;
        let root = installed.library.join(name);
        let description = read_description(&root)?;

        if is_r_platform(&description) {
            self.resolution
                .platform
                .insert(name.to_owned(), installed.version.clone());
            self.state.insert(name.to_owned(), VisitState::Done);
            return Ok(());
        }

        self.state.insert(name.to_owned(), VisitState::Visiting);
        self.stack.push(name.to_owned());

        let dependencies = runtime_dependencies(&description)?;
        if self.explicit_target.contains(name) {
            // Externalization is strictly explicit. An external package may only
            // depend on R platform packages or other explicitly external packages.
            for dependency in dependencies {
                let dep_installed = self
                    .target
                    .package(&dependency)
                    .cloned()
                    .ok_or_else(|| ResolveError::MissingPackage(dependency.clone()))?;
                let dep_description = read_description(&dep_installed.library.join(&dependency))?;
                if is_r_platform(&dep_description) {
                    self.resolution
                        .platform
                        .insert(dependency.clone(), dep_installed.version.clone());
                } else if self.explicit_target.contains(&dependency) {
                    self.visit(&dependency)?;
                } else {
                    return Err(ResolveError::ExplicitTargetClosure {
                        package: name.to_owned(),
                        dependency,
                    });
                }
            }
        } else {
            for dependency in dependencies {
                self.visit(&dependency)?;
            }
            self.resolution.internalized.push(ResolvedPackage {
                installed,
                root,
            });
        }

        self.stack.pop();
        self.state.insert(name.to_owned(), VisitState::Done);
        Ok(())
    }
}

fn read_description(root: &Path) -> Result<Description, ResolveError> {
    let path = root.join("DESCRIPTION");
    let text = fs::read_to_string(&path).map_err(|source| ResolveError::Io {
        path: path.clone(),
        source,
    })?;
    Description::parse(&text).map_err(|source| ResolveError::Metadata { path, source })
}

fn runtime_dependencies(description: &Description) -> Result<Vec<String>, ResolveError> {
    let mut names = BTreeSet::new();
    for dependency in description
        .imports()
        .map_err(|source| ResolveError::Metadata {
            path: PathBuf::from("DESCRIPTION"),
            source,
        })?
        .into_iter()
        .chain(
            description
                .depends()
                .map_err(|source| ResolveError::Metadata {
                    path: PathBuf::from("DESCRIPTION"),
                    source,
                })?
                .into_iter(),
        )
    {
        if dependency.name != "R" {
            names.insert(dependency.name);
        }
    }
    Ok(names.into_iter().collect())
}

fn is_r_platform(description: &Description) -> bool {
    description
        .get("Priority")
        .is_some_and(|priority| priority.eq_ignore_ascii_case("base"))
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "failed to read installed package at {}: {source}", path.display()),
            Self::Metadata { path, source } => write!(f, "invalid installed DESCRIPTION at {}: {source}", path.display()),
            Self::MissingPackage(name) => write!(
                f,
                "runtime dependency `{name}` is not installed in the selected R library universe"
            ),
            Self::MissingExplicitTarget(name) => write!(
                f,
                "explicit target-provided package `{name}` is not installed in the selected R library universe"
            ),
            Self::ExplicitTargetClosure { package, dependency } => write!(
                f,
                "target-provided `{package}` depends on `{dependency}`; list `{dependency}` explicitly too or internalize `{package}`"
            ),
            Self::Cycle(cycle) => write!(f, "installed dependency cycle: {}", cycle.join(" -> ")),
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Metadata { source, .. } => Some(source),
            _ => None,
        }
    }
}
