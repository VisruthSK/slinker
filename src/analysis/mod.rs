pub mod diagnostic;
pub mod graph;
pub mod namespace;
pub mod package;
pub mod parser;
pub mod source;

pub use diagnostic::{Diagnostic, RejectCode};
pub use graph::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
pub use namespace::NamespaceDirective;
pub use package::AnalysisPackage;
use package::PreparedPackage;
pub use parser::{BindingCertainty, EvalPhase, ResourceRef};

use parser::{AirParser, CallSite, StaticArg, SyntaxEffectKind};
use source::Sources;
use crate::{Error, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetPackage {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerConfig {
    pub root: PathBuf,
    pub dependencies: Vec<PathBuf>,
    pub target_packages: Vec<TargetPackage>,
}

#[derive(Debug)]
pub struct Analysis {
    pub graph: Graph,
    pub roots: Vec<NodeId>,
    pub reachable: Vec<bool>,
    pub diagnostics: Vec<Diagnostic>,
    pub sources: Sources,
    pub packages: Vec<AnalysisPackage>,
}

pub struct Analyzer {
    jobs: usize,
    packages: BTreeMap<PathBuf, AnalysisPackage>,
    sources: Sources,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self {
            jobs: 1,
            packages: BTreeMap::new(),
            sources: Sources::default(),
        }
    }
}

impl Analyzer {
    pub fn with_jobs(jobs: usize) -> Self {
        Self {
            jobs: jobs.max(1),
            ..Self::default()
        }
    }

    pub fn analyze(&mut self, cfg: &AnalyzerConfig) -> Result<Analysis> {
        let roots = std::iter::once(cfg.root.clone())
            .chain(cfg.dependencies.iter().cloned())
            .collect::<Vec<_>>();

        let missing = roots
            .iter()
            .filter(|root| !self.packages.contains_key(*root))
            .cloned()
            .collect::<Vec<_>>();

        if !missing.is_empty() {
            let pool = if self.jobs > 1 && missing.len() > 1 {
                Some(
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(self.jobs.min(missing.len()))
                        .build()
                        .map_err(|error| {
                            Error::Analysis(format!("failed to create Rayon pool: {error}"))
                        })?,
                )
            } else {
                None
            };

            let prepared = if let Some(pool) = &pool {
                pool.install(|| {
                    missing
                        .par_iter()
                        .map(|root| PreparedPackage::load(root))
                        .collect::<Vec<_>>()
                })
            } else {
                missing
                    .iter()
                    .map(|root| PreparedPackage::load(root))
                    .collect::<Vec<_>>()
            };
            let prepared = prepared.into_iter().collect::<Result<Vec<_>>>()?;

            let plans = prepared
                .into_iter()
                .map(|package| package.attach_sources(&mut self.sources))
                .collect::<Vec<_>>();

            let parsed = if let Some(pool) = &pool {
                pool.install(|| {
                    plans
                        .into_par_iter()
                        .map(|plan| plan.parse(&AirParser))
                        .collect::<Vec<_>>()
                })
            } else {
                plans
                    .into_iter()
                    .map(|plan| plan.parse(&AirParser))
                    .collect::<Vec<_>>()
            };
            let parsed = parsed.into_iter().collect::<Result<Vec<_>>>()?;

            for (root, package) in missing.into_iter().zip(parsed) {
                self.packages.insert(root, package);
            }
        }

        let packages = roots
            .iter()
            .map(|root| {
                self.packages
                    .get(root)
                    .cloned()
                    .ok_or_else(|| Error::Analysis(format!("analysis cache missed {}", root.display())))
            })
            .collect::<Result<Vec<_>>>()?;
        reject_duplicate_packages(&packages)?;
        let root_name = packages
            .first()
            .expect("analyzer always has a root package")
            .id
            .name
            .clone();

        let target: HashSet<String> = cfg
            .target_packages
            .iter()
            .map(|package| package.name.clone())
            .collect();
        let package_names: HashSet<String> =
            packages.iter().map(|package| package.id.name.clone()).collect();

        let mut graph = Graph::default();
        let mut roots = Vec::new();
        let mut diagnostics = Vec::new();

        let mut activation_nodes = HashMap::<String, NodeId>::new();
        let mut native_nodes = HashMap::<String, NodeId>::new();
        let mut sysdata_nodes = HashMap::<String, NodeId>::new();
        let mut binding_nodes = HashMap::<(String, String), NodeId>::new();
        let mut init_nodes = HashMap::<(String, usize), NodeId>::new();

        // Pass 1: create stable identities for package activation, source
        // initialization units, package bindings, and package-wide components.
        for package in &packages {
            let name = &package.id.name;
            let activation = graph.add_node(
                name,
                NodeKind::Lifecycle {
                    hook: "activation".into(),
                },
                None,
            );
            activation_nodes.insert(name.clone(), activation);
            if name == &root_name {
                roots.push(activation);
            }

            let mut ordinal = 0usize;
            for source in &package.r_units {
                for expression in &source.parsed.expressions {
                    let init = graph.add_node(
                        name,
                        NodeKind::Initialization { ordinal },
                        Some(expression.span.clone()),
                    );
                    graph.set_bytes(
                        init,
                        expression.span.end.saturating_sub(expression.span.start) as u64,
                    );
                    init_nodes.insert((name.clone(), ordinal), init);
                    ordinal += 1;

                    // The root is not tree-shaken. Retain all of its source
                    // materialization units, including side-effect-only units.
                    if name == &root_name {
                        roots.push(init);
                    }

                    for definition in &expression.definitions {
                        let binding = graph.add_node(
                            name,
                            NodeKind::Binding {
                                name: definition.name.clone(),
                            },
                            Some(definition.span.clone()),
                        );
                        binding_nodes.insert(
                            (name.clone(), definition.name.clone()),
                            binding,
                        );

                        // An R top-level expression is the minimum execution unit
                        // in this analyzer. If it writes multiple bindings, keeping
                        // one keeps the unit and all writes produced by that unit.
                        graph.add_edge(
                            binding,
                            init,
                            EdgeKind::Initialization,
                            format!("`{}` is produced by this initialization unit", definition.name),
                        );
                        graph.add_edge(
                            init,
                            binding,
                            EdgeKind::Initialization,
                            format!("initialization writes `{}`", definition.name),
                        );

                        if name == &root_name {
                            roots.push(binding);
                        }
                    }
                }
            }

            if package.has_native {
                let native = graph.add_node(
                    name,
                    NodeKind::NativeComponent { name: name.clone() },
                    None,
                );
                native_nodes.insert(name.clone(), native);
                if name == &root_name {
                    roots.push(native);
                }
            }

            if package.has_sysdata {
                let sysdata = graph.add_node(
                    name,
                    NodeKind::SerializedObject {
                        name: "sysdata.rda".into(),
                    },
                    None,
                );
                sysdata_nodes.insert(name.clone(), sysdata);
                if name == &root_name {
                    roots.push(sysdata);
                }
            }

            for directive in &package.namespace {
                graph.add_node(
                    name,
                    NodeKind::NamespaceDirective {
                        text: format!("{directive:?}"),
                    },
                    None,
                );
            }
        }

        // Bind lifecycle hooks after all package bindings exist.
        for package in &packages {
            let name = &package.id.name;
            if let Some(&binding) = binding_nodes.get(&(name.clone(), ".onLoad".into())) {
                let lifecycle = graph.add_node(
                    name,
                    NodeKind::Lifecycle {
                        hook: ".onLoad".into(),
                    },
                    graph.nodes[binding.0].span.clone(),
                );
                graph.add_edge(
                    activation_nodes[name],
                    lifecycle,
                    EdgeKind::Lifecycle,
                    "package activation runs .onLoad",
                );
                graph.add_edge(
                    lifecycle,
                    binding,
                    EdgeKind::Lifecycle,
                    ".onLoad implementation",
                );

                if name != &root_name {
                    let mut diagnostic = Diagnostic::reject(
                        name,
                        RejectCode::LifecycleHook,
                        "internalized dependency `.onLoad` requires an explicit activation effect model",
                    );
                    diagnostic.span = graph.nodes[binding.0].span.clone();
                    let reject = rejection_node(
                        &mut graph,
                        name,
                        &diagnostic,
                        "onLoad".into(),
                    );
                    graph.add_edge(
                        lifecycle,
                        reject,
                        EdgeKind::Effect,
                        diagnostic.message.clone(),
                    );
                    diagnostic.node = Some(reject);
                    diagnostics.push(diagnostic);
                }
            }
        }

        // Package-wide dependency policy. Every hard R dependency must be
        // either supplied for internalization or declared target-provided.
        for package in &packages {
            for dep in dependency_names(package, "Imports", package.description.imports())? {
                if target.contains(&dep) || package_names.contains(&dep) {
                    continue;
                }
                let mut diagnostic = Diagnostic::reject(
                    &package.id.name,
                    RejectCode::MissingDependency,
                    format!("Imports dependency `{dep}` was not supplied or declared target-provided"),
                );
                let reject = rejection_node(
                    &mut graph,
                    &package.id.name,
                    &diagnostic,
                    format!("imports:{dep}"),
                );
                graph.add_edge(
                    activation_nodes[&package.id.name],
                    reject,
                    EdgeKind::Effect,
                    diagnostic.message.clone(),
                );
                diagnostic.node = Some(reject);
                diagnostics.push(diagnostic);
            }

            for dep in dependency_names(package, "Depends", package.description.depends())? {
                if target.contains(&dep) || package_names.contains(&dep) {
                    continue;
                }
                let mut diagnostic = Diagnostic::reject(
                    &package.id.name,
                    RejectCode::NonTargetDepends,
                    format!(
                        "non-target-provided Depends dependency `{dep}` has attachment semantics"
                    ),
                );
                let reject = rejection_node(
                    &mut graph,
                    &package.id.name,
                    &diagnostic,
                    format!("depends:{dep}"),
                );
                graph.add_edge(
                    activation_nodes[&package.id.name],
                    reject,
                    EdgeKind::Effect,
                    diagnostic.message.clone(),
                );
                diagnostic.node = Some(reject);
                diagnostics.push(diagnostic);
            }
        }

        // Pass 2: effective NAMESPACE load semantics and root public re-exports.
        for package in &packages {
            let name = &package.id.name;
            let activation = activation_nodes[name];
            let imported = imported_symbols(package, &packages);

            for directive in &package.namespace {
                let directive_node = graph.add_node(
                    name,
                    NodeKind::NamespaceDirective {
                        text: format!("{directive:?}"),
                    },
                    None,
                );

                match directive {
                    // export() is resolution metadata, not an activation edge.
                    // Making every export activation-reachable defeats tree-shaking.
                    NamespaceDirective::Export(symbol) => {
                        if name == &root_name
                            && !binding_nodes.contains_key(&(name.clone(), symbol.clone()))
                        {
                            if let Some(provider) = imported.get(symbol) {
                                roots.push(directive_node);
                                add_activation_edge(
                                    &mut graph,
                                    directive_node,
                                    provider,
                                    &activation_nodes,
                                );
                                if let Some(&binding) =
                                    binding_nodes.get(&(provider.clone(), symbol.clone()))
                                {
                                    graph.add_edge(
                                        directive_node,
                                        binding,
                                        EdgeKind::ReExport,
                                        format!("root NAMESPACE re-exports {provider}::{symbol}"),
                                    );
                                }
                            }
                        }
                    }
                    NamespaceDirective::ExportPattern(_) => {
                        // Export patterns are API metadata. They become relevant
                        // when another package needs the resulting export set;
                        // those resolution sites reject conservatively below.
                    }
                    NamespaceDirective::Import(dep) => {
                        graph.add_edge(
                            activation,
                            directive_node,
                            EdgeKind::NamespaceLoad,
                            format!("{name} applies import({dep})"),
                        );
                        add_activation_edge(
                            &mut graph,
                            directive_node,
                            dep,
                            &activation_nodes,
                        );

                        if package_has_export_pattern(dep, &packages) && package_names.contains(dep)
                        {
                            let mut diagnostic = Diagnostic::reject(
                                name,
                                RejectCode::UnknownNamespaceDirective,
                                format!(
                                    "import({dep}) requires resolving {dep}'s exportPattern() surface"
                                ),
                            );
                            let reject = rejection_node(
                                &mut graph,
                                name,
                                &diagnostic,
                                format!("import-export-pattern:{dep}"),
                            );
                            graph.add_edge(
                                directive_node,
                                reject,
                                EdgeKind::Effect,
                                diagnostic.message.clone(),
                            );
                            diagnostic.node = Some(reject);
                            diagnostics.push(diagnostic);
                        }
                    }
                    NamespaceDirective::ImportFrom { package: dep, .. } => {
                        graph.add_edge(
                            activation,
                            directive_node,
                            EdgeKind::NamespaceLoad,
                            format!("{name} applies importFrom({dep}, ...)"),
                        );
                        add_activation_edge(
                            &mut graph,
                            directive_node,
                            dep,
                            &activation_nodes,
                        );
                    }
                    NamespaceDirective::S3Method {
                        generic,
                        class,
                        method,
                    } => {
                        graph.add_edge(
                            activation,
                            directive_node,
                            EdgeKind::NamespaceLoad,
                            "package activation applies S3method directive",
                        );
                        let registration = graph.add_node(
                            name,
                            NodeKind::S3Registration {
                                generic: generic.clone(),
                                class: class.clone(),
                            },
                            None,
                        );
                        graph.add_edge(
                            directive_node,
                            registration,
                            EdgeKind::S3Registration,
                            "S3method directive",
                        );
                        if name == &root_name {
                            roots.push(registration);
                        }

                        let method_name = method
                            .clone()
                            .unwrap_or_else(|| format!("{generic}.{class}"));
                        if let Some(&binding) =
                            binding_nodes.get(&(name.clone(), method_name.clone()))
                        {
                            graph.add_edge(
                                registration,
                                binding,
                                EdgeKind::S3Registration,
                                format!("registered S3 method `{method_name}`"),
                            );
                        } else if let Some(provider) = imported.get(&method_name) {
                            if !target.contains(provider) {
                                add_activation_edge(
                                    &mut graph,
                                    registration,
                                    provider,
                                    &activation_nodes,
                                );
                                if let Some(&binding) =
                                    binding_nodes.get(&(provider.clone(), method_name.clone()))
                                {
                                    graph.add_edge(
                                        registration,
                                        binding,
                                        EdgeKind::S3Registration,
                                        format!(
                                            "registered S3 method `{method_name}` imported from `{provider}`"
                                        ),
                                    );
                                }
                            }
                        } else {
                            let mut diagnostic = Diagnostic::reject(
                                name,
                                RejectCode::UnresolvedBinding,
                                format!(
                                    "S3 registration `{generic}.{class}` has no statically resolved method `{method_name}`"
                                ),
                            );
                            let reject = rejection_node(
                                &mut graph,
                                name,
                                &diagnostic,
                                format!("s3-method:{generic}:{class}:{method_name}"),
                            );
                            graph.add_edge(
                                registration,
                                reject,
                                EdgeKind::Effect,
                                diagnostic.message.clone(),
                            );
                            diagnostic.node = Some(reject);
                            diagnostics.push(diagnostic);
                        }
                    }
                    NamespaceDirective::UseDynLib { dll } => {
                        graph.add_edge(
                            activation,
                            directive_node,
                            EdgeKind::NamespaceLoad,
                            format!("{name} applies useDynLib({dll})"),
                        );
                        if let Some(&native) = native_nodes.get(name) {
                            graph.add_edge(
                                directive_node,
                                native,
                                EdgeKind::Native,
                                format!("useDynLib({dll}) loads native component"),
                            );
                        }
                    }
                    NamespaceDirective::Other(text) => {
                        graph.add_edge(
                            activation,
                            directive_node,
                            EdgeKind::NamespaceLoad,
                            "package activation reaches unsupported NAMESPACE directive",
                        );
                        let mut diagnostic = Diagnostic::reject(
                            name,
                            RejectCode::UnknownNamespaceDirective,
                            format!("unsupported NAMESPACE directive: {text}"),
                        );
                        let reject = rejection_node(
                            &mut graph,
                            name,
                            &diagnostic,
                            format!("namespace:{text}"),
                        );
                        graph.add_edge(
                            directive_node,
                            reject,
                            EdgeKind::Effect,
                            diagnostic.message.clone(),
                        );
                        diagnostic.node = Some(reject);
                        diagnostics.push(diagnostic);
                    }
                }
            }
        }

        // Pass 3: source-unit references and policy effects.
        for package in &packages {
            let name = &package.id.name;
            let local_bindings: HashSet<String> = package
                .r_units
                .iter()
                .flat_map(|unit| unit.parsed.bindings().map(|def| def.name.clone()))
                .collect();
            // Runtime closure lookup sees the fully materialized package frame.
            // Materialization-time lookup is intentionally more conservative:
            // without proving exact Collate and intra-expression execution order,
            // an import of the same name remains a possible fallback.
            let runtime_definite_bindings: HashSet<String> = package
                .r_units
                .iter()
                .flat_map(|unit| unit.parsed.bindings())
                .filter(|def| def.certainty == BindingCertainty::Definite)
                .map(|def| def.name.clone())
                .collect();
            let imported = imported_symbols(package, &packages);

            let mut ordinal = 0usize;
            for source in &package.r_units {
                for expression in &source.parsed.expressions {
                    let owner = init_nodes[&(name.clone(), ordinal)];
                    ordinal += 1;

                    let rewrites_internal_package = expression
                        .package_refs
                        .iter()
                        .any(|reference| package_names.contains(&reference.package))
                        || expression.resource_refs.iter().any(|reference| {
                            reference
                                .package
                                .as_ref()
                                .is_some_and(|package| package_names.contains(package))
                        })
                        || expression.calls.iter().any(|call| {
                            static_package_identity(call)
                                .is_some_and(|package| package_names.contains(package))
                        });

                    for reference in &expression.package_refs {
                        if target.contains(&reference.package) {
                            continue;
                        }

                        let Some(&package_activation) = activation_nodes.get(&reference.package)
                        else {
                            attach_rejection(
                                &mut graph,
                                &mut diagnostics,
                                owner,
                                name,
                                RejectCode::UnresolvedBinding,
                                format!(
                                    "package-qualified reference {}{}{} has no supplied or target-provided package",
                                    reference.package,
                                    if reference.internal { ":::" } else { "::" },
                                    reference.symbol
                                ),
                                Some(reference.span.clone()),
                                format!("package-ref:{}:{}", reference.package, reference.span.start),
                            );
                            continue;
                        };

                        graph.add_edge(
                            owner,
                            package_activation,
                            EdgeKind::NamespaceLoad,
                            format!(
                                "{} access activates {}",
                                if reference.internal { ":::" } else { "::" },
                                reference.package
                            ),
                        );

                        let provider = if reference.internal {
                            Some(reference.package.clone())
                        } else {
                            resolve_export_provider(
                                &reference.package,
                                &reference.symbol,
                                &packages,
                            )
                        };

                        let Some(provider) = provider else {
                            attach_rejection(
                                &mut graph,
                                &mut diagnostics,
                                owner,
                                name,
                                RejectCode::UnresolvedBinding,
                                format!(
                                    "cannot prove that {}::{} resolves to a retained exported binding",
                                    reference.package, reference.symbol
                                ),
                                Some(reference.span.clone()),
                                format!("unresolved-export:{}:{}", reference.package, reference.span.start),
                            );
                            continue;
                        };

                        let Some(&target_binding) =
                            binding_nodes.get(&(provider.clone(), reference.symbol.clone()))
                        else {
                            attach_rejection(
                                &mut graph,
                                &mut diagnostics,
                                owner,
                                name,
                                RejectCode::UnresolvedBinding,
                                format!(
                                    "{}{}{} resolves to `{provider}`, but no source binding was indexed",
                                    reference.package,
                                    if reference.internal { ":::" } else { "::" },
                                    reference.symbol
                                ),
                                Some(reference.span.clone()),
                                format!("missing-binding:{provider}:{}", reference.symbol),
                            );
                            continue;
                        };

                        graph.add_edge(
                            owner,
                            target_binding,
                            EdgeKind::PackageQualified,
                            format!(
                                "{}{}{} via {}",
                                reference.package,
                                if reference.internal { ":::" } else { "::" },
                                reference.symbol,
                                provider
                            ),
                        );
                    }

                    for resource in &expression.resource_refs {
                        let resource_package = resource.package.as_deref().unwrap_or("base");
                        if resource_package == "base" || target.contains(resource_package) {
                            continue;
                        }

                        if !package_names.contains(resource_package) {
                            attach_rejection(
                                &mut graph,
                                &mut diagnostics,
                                owner,
                                name,
                                RejectCode::MissingDependency,
                                format!(
                                    "system.file() refers to package `{resource_package}`, which was not supplied or declared target-provided"
                                ),
                                Some(resource.span.clone()),
                                format!("resource-package:{resource_package}:{}", resource.span.start),
                            );
                            continue;
                        }

                        let path = resource.path.clone().unwrap_or_else(|| "*".into());
                        let resource_node = graph.add_node(
                            resource_package,
                            NodeKind::Resource { path: path.clone() },
                            Some(resource.span.clone()),
                        );
                        graph.add_edge(
                            owner,
                            resource_node,
                            EdgeKind::Resource,
                            if path == "*" {
                                format!(
                                    "system.file() requires a dynamically selected resource from `{resource_package}`"
                                )
                            } else {
                                format!(
                                    "system.file() requires `{resource_package}/{path}`"
                                )
                            },
                        );
                    }

                    for reference in &expression.references {
                        // Package-frame writes inside top-level control flow are
                        // potential rather than guaranteed bindings. Retain the
                        // local producer when one exists, but do not let that hide
                        // an imported fallback of the same name.
                        if local_bindings.contains(&reference.name) {
                            if let Some(&binding) =
                                binding_nodes.get(&(name.clone(), reference.name.clone()))
                            {
                                graph.add_edge(
                                    owner,
                                    binding,
                                    EdgeKind::Lexical,
                                    format!("package lexical reference `{}`", reference.name),
                                );
                            }
                        }

                        let definitely_shadowed = match reference.phase {
                            EvalPhase::Runtime => {
                                runtime_definite_bindings.contains(&reference.name)
                            }
                            EvalPhase::Materialization => false,
                        };
                        if definitely_shadowed {
                            continue;
                        }

                        let Some(provider) = imported.get(&reference.name) else {
                            // `R/sysdata.rda` injects package-frame bindings that
                            // have no R-source definition. Until the R-side
                            // inspector enumerates them individually, retain the
                            // serialized object conservatively for unresolved
                            // package lexical references.
                            if let Some(&sysdata) = sysdata_nodes.get(name) {
                                graph.add_edge(
                                    owner,
                                    sysdata,
                                    EdgeKind::Lexical,
                                    format!(
                                        "unresolved package reference `{}` may be supplied by sysdata.rda",
                                        reference.name
                                    ),
                                );
                            }
                            continue;
                        };
                        if target.contains(provider) {
                            continue;
                        }
                        if let Some(&activation) = activation_nodes.get(provider) {
                            graph.add_edge(
                                owner,
                                activation,
                                EdgeKind::NamespaceLoad,
                                format!("imported fallback requires `{provider}` activation"),
                            );
                        }
                        if let Some(&binding) =
                            binding_nodes.get(&(provider.clone(), reference.name.clone()))
                        {
                            graph.add_edge(
                                owner,
                                binding,
                                EdgeKind::Import,
                                format!(
                                    "imported fallback `{}` from `{provider}`",
                                    reference.name
                                ),
                            );
                        }
                    }

                    for call in &expression.calls {
                        if is_native_call(&call.callee) {
                            if let Some(&native) = native_nodes.get(name) {
                                graph.add_edge(
                                    owner,
                                    native,
                                    EdgeKind::Native,
                                    format!("{} reaches package native component", call.callee),
                                );
                            }
                        }

                        if let Some(package) = static_namespace_load(call) {
                            if package_names.contains(package) && !target.contains(package) {
                                if let Some(&activation) = activation_nodes.get(package) {
                                    graph.add_edge(
                                        owner,
                                        activation,
                                        EdgeKind::NamespaceLoad,
                                        format!("{}({package:?}) activates internalized `{package}`", call.callee),
                                    );
                                }
                            }
                        }

                        let diagnostic = if name == &root_name {
                            None
                        } else {
                            match call.phase {
                                EvalPhase::Materialization => {
                                    classify_top_level_call(
                                        name,
                                        call,
                                        &target,
                                        &package_names,
                                        rewrites_internal_package,
                                    )
                                }
                                EvalPhase::Runtime => {
                                    classify_call(
                                        name,
                                        call,
                                        &target,
                                        &package_names,
                                        rewrites_internal_package,
                                    )
                                }
                            }
                        };
                        if let Some(diagnostic) = diagnostic {
                            let diagnostic_owner = owner;
                            attach_existing_diagnostic(
                                &mut graph,
                                &mut diagnostics,
                                diagnostic_owner,
                                diagnostic,
                                Some(call.span.clone()),
                                format!("call:{}:{}", call.callee, call.span.start),
                            );
                        }
                    }

                    for effect in &expression.effects {
                        if name == &root_name {
                            continue;
                        }
                        let (code, message) = match effect.kind {
                            SyntaxEffectKind::SuperAssignment => (
                                RejectCode::EnvironmentMutation,
                                "reachable superassignment (`<<-`/`->>`) can mutate an enclosing environment",
                            ),
                            SyntaxEffectKind::IndirectPackageWrite => (
                                RejectCode::UnsupportedTopLevelEffect,
                                "package-frame assignment occurs below an unknown lazy evaluation boundary",
                            ),
                            SyntaxEffectKind::UnsupportedAssignmentTarget => (
                                RejectCode::EnvironmentMutation,
                                "assignment target cannot be reduced to a statically known binding",
                            ),
                        };
                        let diagnostic_owner = owner;
                        attach_rejection(
                            &mut graph,
                            &mut diagnostics,
                            diagnostic_owner,
                            name,
                            code,
                            message.into(),
                            Some(effect.span.clone()),
                            format!("syntax-effect:{:?}:{}", effect.kind, effect.span.start),
                        );
                    }
                }
            }
        }

        // Native code is atomic at the shared-library component boundary.
        // Reachability into the native component does not widen the package's R
        // layer; R bindings remain governed by ordinary graph edges.

        dedup_roots(&mut roots);
        let reachable = graph.reachable(roots.iter().copied());
        for diagnostic in &mut diagnostics {
            diagnostic.reachable = diagnostic
                .node
                .map(|node| reachable[node.0])
                .unwrap_or(true);
        }

        Ok(Analysis {
            graph,
            roots,
            reachable,
            diagnostics,
            sources: self.sources.clone(),
            packages,
        })
    }
}

fn dependency_names(
    package: &AnalysisPackage,
    field: &str,
    dependencies: std::result::Result<Vec<crate::Dependency>, crate::MetadataError>,
) -> Result<Vec<String>> {
    dependencies
        .map(|dependencies| dependencies.into_iter().map(|dependency| dependency.name).collect())
        .map_err(|error| {
            Error::Analysis(format!(
                "invalid {field} field in package `{}`: {error}",
                package.id.name
            ))
        })
}

fn reject_duplicate_packages(packages: &[AnalysisPackage]) -> Result<()> {
    let mut seen = HashSet::new();
    for package in packages {
        if !seen.insert(package.id.name.clone()) {
            return Err(Error::Analysis(format!(
                "duplicate package source supplied for `{}`",
                package.id.name
            )));
        }
    }
    Ok(())
}

fn dedup_roots(roots: &mut Vec<NodeId>) {
    let mut seen = HashSet::new();
    roots.retain(|root| seen.insert(*root));
}

fn rejection_node(
    graph: &mut Graph,
    package: &str,
    diagnostic: &Diagnostic,
    key: String,
) -> NodeId {
    graph.add_node(
        package,
        NodeKind::Rejection {
            code: format!("{:?}:{key}", diagnostic.code),
        },
        diagnostic.span.clone(),
    )
}

fn attach_rejection(
    graph: &mut Graph,
    diagnostics: &mut Vec<Diagnostic>,
    owner: NodeId,
    package: &str,
    code: RejectCode,
    message: String,
    span: Option<crate::analysis::source::Span>,
    key: String,
) {
    let diagnostic = Diagnostic::reject(package, code, message);
    attach_existing_diagnostic(graph, diagnostics, owner, diagnostic, span, key);
}

fn attach_existing_diagnostic(
    graph: &mut Graph,
    diagnostics: &mut Vec<Diagnostic>,
    owner: NodeId,
    mut diagnostic: Diagnostic,
    span: Option<crate::analysis::source::Span>,
    key: String,
) {
    diagnostic.span = span;
    let reject = rejection_node(graph, &diagnostic.package, &diagnostic, key);
    graph.add_edge(owner, reject, EdgeKind::Effect, diagnostic.message.clone());
    diagnostic.node = Some(reject);
    diagnostics.push(diagnostic);
}

fn add_activation_edge(
    graph: &mut Graph,
    from: NodeId,
    dep: &str,
    activation_nodes: &HashMap<String, NodeId>,
) {
    let Some(&target) = activation_nodes.get(dep) else {
        return;
    };
    graph.add_edge(
        from,
        target,
        EdgeKind::NamespaceLoad,
        format!("namespace import activates `{dep}`"),
    );
}

fn imported_symbols(package: &AnalysisPackage, packages: &[AnalysisPackage]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for directive in &package.namespace {
        match directive {
            NamespaceDirective::ImportFrom {
                package: dep,
                symbols,
            } => {
                let supplied = packages
                    .iter()
                    .any(|candidate| candidate.id.name == *dep);
                for symbol in symbols {
                    if let Some(provider) = resolve_export_provider(dep, symbol, packages) {
                        out.insert(symbol.clone(), provider);
                    } else if !supplied {
                        // Target-provided packages are not parsed here. For an
                        // explicit importFrom(), the named package itself is the
                        // provider unless a supplied package proves a re-export.
                        out.insert(symbol.clone(), dep.clone());
                    }
                }
            }
            NamespaceDirective::Import(dep) => {
                for (symbol, provider) in exported_symbols(dep, packages, &mut HashSet::new()) {
                    out.insert(symbol, provider);
                }
            }
            _ => {}
        }
    }
    out
}

fn resolve_export_provider(package: &str, symbol: &str, packages: &[AnalysisPackage]) -> Option<String> {
    exported_symbols(package, packages, &mut HashSet::new()).remove(symbol)
}

fn exported_symbols(
    package: &str,
    packages: &[AnalysisPackage],
    visiting: &mut HashSet<String>,
) -> BTreeMap<String, String> {
    if !visiting.insert(package.to_string()) {
        return BTreeMap::new();
    }
    let Some(pkg) = packages
        .iter()
        .find(|candidate| candidate.id.name == package)
    else {
        visiting.remove(package);
        return BTreeMap::new();
    };

    let imports = imported_symbols_shallow(pkg, packages, visiting);
    let local: HashSet<String> = pkg
        .r_units
        .iter()
        .flat_map(|unit| unit.parsed.bindings().map(|binding| binding.name.clone()))
        .collect();
    let mut exports = BTreeMap::new();
    for directive in &pkg.namespace {
        let NamespaceDirective::Export(symbol) = directive else {
            continue;
        };
        if local.contains(symbol) {
            exports.insert(symbol.clone(), package.to_string());
        } else if let Some(provider) = imports.get(symbol) {
            exports.insert(symbol.clone(), provider.clone());
        }
    }
    visiting.remove(package);
    exports
}

fn imported_symbols_shallow(
    package: &AnalysisPackage,
    packages: &[AnalysisPackage],
    visiting: &mut HashSet<String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for directive in &package.namespace {
        match directive {
            NamespaceDirective::ImportFrom {
                package: dep,
                symbols,
            } => {
                let supplied = packages
                    .iter()
                    .any(|candidate| candidate.id.name == *dep);
                let exported = exported_symbols(dep, packages, visiting);
                for symbol in symbols {
                    if let Some(provider) = exported.get(symbol) {
                        out.insert(symbol.clone(), provider.clone());
                    } else if !supplied {
                        out.insert(symbol.clone(), dep.clone());
                    }
                }
            }
            NamespaceDirective::Import(dep) => {
                for (symbol, provider) in exported_symbols(dep, packages, visiting) {
                    out.insert(symbol, provider);
                }
            }
            _ => {}
        }
    }
    out
}

fn package_has_export_pattern(package: &str, packages: &[AnalysisPackage]) -> bool {
    packages
        .iter()
        .find(|candidate| candidate.id.name == package)
        .is_some_and(|package| {
            package
                .namespace
                .iter()
                .any(|directive| matches!(directive, NamespaceDirective::ExportPattern(_)))
        })
}

fn classify_top_level_call(
    package: &str,
    call: &CallSite,
    target: &HashSet<String>,
    internalized: &HashSet<String>,
    transformed_syntax: bool,
) -> Option<Diagnostic> {
    classify_call(package, call, target, internalized, transformed_syntax)
}

fn classify_call(
    package: &str,
    call: &CallSite,
    target: &HashSet<String>,
    internalized: &HashSet<String>,
    transformed_syntax: bool,
) -> Option<Diagnostic> {
    let callee = call.callee.as_str();
    let first_string = call.args.first().and_then(|value| match value {
        Some(StaticArg::String(value)) => Some(value.as_str()),
        _ => None,
    });
    let first_package_literal = call.args.first().and_then(|value| match value {
        Some(StaticArg::String(value)) | Some(StaticArg::Symbol(value)) => Some(value.as_str()),
        None => None,
    });
    let reject = |code, message: String| Some(Diagnostic::reject(package, code, message));

    match callee {
        "get" | "get0" | "mget" | "exists" => reject(
            RejectCode::DynamicLookup,
            format!("reachable dynamic lookup through `{callee}` is not modeled"),
        ),
        "assign" | "remove" | "rm" | "lockBinding" | "unlockBinding" => reject(
            RejectCode::EnvironmentMutation,
            format!("reachable environment mutation through `{callee}`"),
        ),
        "eval" | "evalq" | "parse" | "source" | "sys.source" | "str2lang" => reject(
            RejectCode::ArbitraryEvaluation,
            format!("reachable unsupported evaluation through `{callee}`"),
        ),
        "body" | "formals" | "substitute" | "deparse" if transformed_syntax => reject(
            RejectCode::SyntaxObservation,
            format!(
                "reachable syntax observation through `{callee}` can observe a required package-identity rewrite"
            ),
        ),
        "makeActiveBinding" | "delayedAssign" => reject(
            RejectCode::ActiveBinding,
            format!("`{callee}` creates unsupported binding semantics"),
        ),
        "setClass" | "setGeneric" | "setMethod" | "R6Class" => reject(
            RejectCode::ObjectSystem,
            format!("unsupported object-system operation `{callee}`"),
        ),
        "requireNamespace" | "loadNamespace" | "getNamespace" | "asNamespace"
        | "packageVersion" | "find.package" => match first_string {
            Some(name) if target.contains(name) || internalized.contains(name) => None,
            Some(name) => reject(
                RejectCode::DynamicPackageDiscovery,
                format!("runtime package discovery of unresolved package `{name}` through `{callee}`"),
            ),
            None => reject(
                RejectCode::DynamicPackageDiscovery,
                format!("dynamic runtime package discovery through `{callee}`"),
            ),
        },
        "require" | "library" => match first_package_literal {
            Some(name) if target.contains(name) => None,
            Some(name) => reject(
                RejectCode::DynamicPackageDiscovery,
                format!("`{callee}({name})` has attachment semantics that are not internalized"),
            ),
            None => reject(
                RejectCode::DynamicPackageDiscovery,
                format!("dynamic attachment through `{callee}` is not modeled"),
            ),
        },
        _ => None,
    }
}

fn static_package_identity(call: &CallSite) -> Option<&str> {
    let first = call.args.first()?.as_ref()?;
    match call.callee.as_str() {
        "require" | "library" => match first {
            StaticArg::String(name) | StaticArg::Symbol(name) => Some(name.as_str()),
        },
        "requireNamespace" | "loadNamespace" | "getNamespace" | "asNamespace"
        | "packageVersion" | "find.package" => match first {
            StaticArg::String(name) => Some(name.as_str()),
            StaticArg::Symbol(_) => None,
        },
        _ => None,
    }
}

fn static_namespace_load(call: &CallSite) -> Option<&str> {
    match call.callee.as_str() {
        "requireNamespace" | "loadNamespace" | "getNamespace" | "asNamespace" => {
            static_package_identity(call)
        }
        _ => None,
    }
}

fn is_native_call(callee: &str) -> bool {
    matches!(callee, ".Call" | ".C" | ".External" | ".Fortran")
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> std::io::Result<Self> {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "hrm-analysis-test-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn package(dir: &Path, name: &str, imports: &str, namespace: &str, source: &str) -> PathBuf {
        let root = dir.join(name);
        fs::create_dir_all(root.join("R")).unwrap();
        let imports_field = if imports.is_empty() {
            String::new()
        } else {
            format!("Imports: {imports}\n")
        };
        fs::write(
            root.join("DESCRIPTION"),
            format!("Package: {name}\nVersion: 1.0.0\n{imports_field}"),
        )
        .unwrap();
        fs::write(root.join("NAMESPACE"), namespace).unwrap();
        fs::write(root.join("R/code.R"), source).unwrap();
        root
    }

    #[test]
    fn possible_local_binding_does_not_hide_imported_fallback() {
        let temp = TempDir::new().unwrap();
        let foo = package(temp.path(), "foo", "", "export(x)\n", "x <- 2\n");
        let root = package(
            temp.path(),
            "root",
            "foo",
            "export(f)\nimportFrom(foo, x)\n",
            "flag <- FALSE\nif (flag) x <- 1\nf <- function() x\n",
        );

        let analysis = Analyzer::default()
            .analyze(&AnalyzerConfig {
                root,
                dependencies: vec![foo],
                target_packages: vec![],
            })
            .unwrap();

        let x = analysis.graph.binding("foo", "x").unwrap();
        assert!(analysis.reachable[x.0]);
    }

    #[test]
    fn imported_infix_operator_is_retained() {
        let temp = TempDir::new().unwrap();
        let foo = package(
            temp.path(),
            "foo",
            "",
            "export(\"%pipe%\")\n",
            "`%pipe%` <- function(x, y) x\n",
        );
        let root = package(
            temp.path(),
            "root",
            "foo",
            "export(f)\nimportFrom(foo, \"%pipe%\")\n",
            "f <- function(x) x %pipe% 1\n",
        );

        let analysis = Analyzer::default()
            .analyze(&AnalyzerConfig {
                root,
                dependencies: vec![foo],
                target_packages: vec![],
            })
            .unwrap();

        let operator = analysis.graph.binding("foo", "%pipe%").unwrap();
        assert!(analysis.reachable[operator.0]);
    }
    #[test]
    fn later_local_definition_does_not_hide_materialization_import() {
        let temp = TempDir::new().unwrap();
        let foo = package(temp.path(), "foo", "", "export(x)\n", "x <- 7\n");
        let bar = package(
            temp.path(),
            "bar",
            "foo",
            "export(y)\nimportFrom(foo, x)\n",
            "y <- x\nx <- 1\n",
        );
        let root = package(
            temp.path(),
            "root",
            "bar",
            "export(f)\nimportFrom(bar, y)\n",
            "f <- function() y\n",
        );

        let analysis = Analyzer::default()
            .analyze(&AnalyzerConfig {
                root,
                dependencies: vec![bar, foo],
                target_packages: vec![],
            })
            .unwrap();

        let imported_x = analysis.graph.binding("foo", "x").unwrap();
        assert!(analysis.reachable[imported_x.0]);
    }

    #[test]
    fn materialization_keeps_import_fallback_even_after_local_definition() {
        let temp = TempDir::new().unwrap();
        let foo = package(temp.path(), "foo", "", "export(x)\n", "x <- 7\n");
        let bar = package(
            temp.path(),
            "bar",
            "foo",
            "export(y)\nimportFrom(foo, x)\n",
            "x <- 1\ny <- x\n",
        );
        let root = package(
            temp.path(),
            "root",
            "bar",
            "export(f)\nimportFrom(bar, y)\n",
            "f <- function() y\n",
        );

        let analysis = Analyzer::default()
            .analyze(&AnalyzerConfig {
                root,
                dependencies: vec![bar, foo],
                target_packages: vec![],
            })
            .unwrap();

        let imported_x = analysis.graph.binding("foo", "x").unwrap();
        assert!(analysis.reachable[imported_x.0]);
    }

    #[test]
    fn imported_replacement_function_is_retained() {
        let temp = TempDir::new().unwrap();
        let foo = package(
            temp.path(),
            "foo",
            "",
            "export(\"decorate<-\")\n",
            "`decorate<-` <- function(x, value) x\n",
        );
        let root = package(
            temp.path(),
            "root",
            "foo",
            "export(f)\nimportFrom(foo, \"decorate<-\")\n",
            "f <- function(x) { decorate(x) <- 1; x }\n",
        );

        let analysis = Analyzer::default()
            .analyze(&AnalyzerConfig {
                root,
                dependencies: vec![foo],
                target_packages: vec![],
            })
            .unwrap();

        let setter = analysis.graph.binding("foo", "decorate<-").unwrap();
        assert!(analysis.reachable[setter.0]);
    }

    #[test]
    fn dead_unsupported_initialization_does_not_block_retained_code() {
        let temp = TempDir::new().unwrap();
        let dep = package(
            temp.path(),
            "dep",
            "",
            "export(f)\n",
            "delayedAssign(\"unused\", 1)\nf <- function() 1\n",
        );
        let root = package(
            temp.path(),
            "root",
            "dep",
            "export(f)\nimportFrom(dep, f)\n",
            "",
        );

        let analysis = Analyzer::default()
            .analyze(&AnalyzerConfig {
                root,
                dependencies: vec![dep],
                target_packages: vec![],
            })
            .unwrap();

        assert!(analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.package == "dep"
                && !diagnostic.reachable
                && diagnostic.code == RejectCode::ActiveBinding
        }));
    }

    #[test]
    fn unresolved_package_reference_retains_sysdata_conservatively() {
        let temp = TempDir::new().unwrap();
        let dep = package(
            temp.path(),
            "dep",
            "",
            "export(f)\n",
            "f <- function() table_from_sysdata\n",
        );
        fs::write(dep.join("R/sysdata.rda"), b"fixture").unwrap();
        let root = package(
            temp.path(),
            "root",
            "dep",
            "export(f)\nimportFrom(dep, f)\n",
            "",
        );

        let analysis = Analyzer::default()
            .analyze(&AnalyzerConfig {
                root,
                dependencies: vec![dep],
                target_packages: vec![],
            })
            .unwrap();

        let node = analysis
            .graph
            .nodes
            .iter()
            .find(|node| {
                node.package == "dep"
                    && matches!(
                        &node.kind,
                        NodeKind::SerializedObject { name } if name == "sysdata.rda"
                    )
            })
            .unwrap();
        assert!(analysis.reachable[node.id.0]);
    }

    #[test]
    fn unused_dependency_binding_is_eliminated() {
        let temp = TempDir::new().unwrap();
        let dep = package(
            temp.path(),
            "dep",
            "",
            "export(used)\nexport(unused)\n",
            "used <- function() 1\nunused <- function() 2\n",
        );
        let root = package(
            temp.path(),
            "root",
            "dep",
            "export(run)\nimportFrom(dep, used)\n",
            "run <- function() used()\n",
        );

        let analysis = Analyzer::default()
            .analyze(&AnalyzerConfig {
                root,
                dependencies: vec![dep],
                target_packages: vec![],
            })
            .unwrap();

        let used = analysis.graph.binding("dep", "used").unwrap();
        let unused = analysis.graph.binding("dep", "unused").unwrap();
        assert!(analysis.reachable[used.0]);
        assert!(!analysis.reachable[unused.0]);

        let retained_bytes: u64 = analysis
            .graph
            .nodes
            .iter()
            .filter(|node| {
                node.package == "dep"
                    && matches!(&node.kind, NodeKind::Initialization { .. })
                    && analysis.reachable[node.id.0]
            })
            .map(|node| node.bytes)
            .sum();
        let total_bytes: u64 = analysis
            .graph
            .nodes
            .iter()
            .filter(|node| {
                node.package == "dep"
                    && matches!(&node.kind, NodeKind::Initialization { .. })
            })
            .map(|node| node.bytes)
            .sum();
        assert!(retained_bytes > 0);
        assert!(retained_bytes < total_bytes);
    }


    #[test]
    fn reachable_system_file_retains_internalized_resource() {
        let temp = TempDir::new().unwrap();
        let dep = package(
            temp.path(),
            "dep",
            "",
            "export(asset_path)\n",
            r#"asset_path <- function() system.file("extdata", "model.json", package = "dep")
"#,
        );
        fs::create_dir_all(dep.join("inst/extdata")).unwrap();
        fs::write(dep.join("inst/extdata/model.json"), "{}").unwrap();
        let root = package(
            temp.path(),
            "root",
            "dep",
            "export(run)\nimportFrom(dep, asset_path)\n",
            "run <- function() asset_path()\n",
        );

        let analysis = Analyzer::default()
            .analyze(&AnalyzerConfig {
                root,
                dependencies: vec![dep],
                target_packages: vec![],
            })
            .unwrap();

        let resource = analysis
            .graph
            .nodes
            .iter()
            .find(|node| {
                node.package == "dep"
                    && matches!(
                        &node.kind,
                        NodeKind::Resource { path } if path == "extdata/model.json"
                    )
            })
            .expect("resource node");
        assert!(analysis.reachable[resource.id.0]);
    }

}
