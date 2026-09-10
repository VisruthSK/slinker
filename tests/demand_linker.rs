#![cfg(feature = "air")]

use hrm::analysis::{DiscoveryPolicy, EdgeKind, LinkPolicy, Linker, Need, NodeKind, RejectCode};
use hrm::package::{
    BindingImage, BindingOrigin, ClosureSource, Digest, ExportMap, ImportBinding, ImportSpec,
    InstalledPackage, LifecycleMetadata, NativeComponent, ObjectKind, PackageId, PackageImage,
    PackageIndex, PackageProvider, ResourceInfo, S3Registration, SyntaxValidation,
};
use hrm::{Description, Error, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct FakeProvider {
    packages: HashMap<String, Arc<PackageImage>>,
    target: HashSet<String>,
    image_counts: Arc<Mutex<HashMap<String, usize>>>,
    validation: SyntaxValidation,
}

impl FakeProvider {
    fn new(images: Vec<PackageImage>) -> Self {
        Self {
            packages: images
                .into_iter()
                .map(|image| (image.index.package.id.name.clone(), Arc::new(image)))
                .collect(),
            target: HashSet::new(),
            image_counts: Arc::new(Mutex::new(HashMap::new())),
            validation: SyntaxValidation::Accepted,
        }
    }

    fn target(mut self, name: &str) -> Self {
        self.target.insert(name.to_owned());
        self
    }

    fn count_handle(&self) -> Arc<Mutex<HashMap<String, usize>>> {
        Arc::clone(&self.image_counts)
    }
}

impl PackageProvider for FakeProvider {
    fn locate(&mut self, name: &str) -> Result<InstalledPackage> {
        self.packages
            .get(name)
            .map(|image| image.index.package.clone())
            .ok_or_else(|| Error::Analysis(format!("missing fake package {name}")))
    }

    fn image(&mut self, package: &InstalledPackage) -> Result<Arc<PackageImage>> {
        *self
            .image_counts
            .lock()
            .unwrap()
            .entry(package.id.name.clone())
            .or_default() += 1;
        self.packages
            .get(&package.id.name)
            .cloned()
            .ok_or_else(|| Error::Analysis(format!("missing fake image {}", package.id.name)))
    }

    fn is_target_provided(&self, package: &InstalledPackage) -> bool {
        self.target.contains(&package.id.name)
            || package
                .description
                .get("Priority")
                .is_some_and(|value| value == "base")
    }

    fn validate_syntax(
        &self,
        _id: &PackageId,
        _binding: &str,
        _source: &str,
    ) -> Result<SyntaxValidation> {
        Ok(self.validation.clone())
    }
}

fn package(name: &str, bindings: &[(&str, Option<&str>)]) -> PackageImage {
    package_with(
        name,
        bindings,
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    )
}

fn package_with(
    name: &str,
    bindings: &[(&str, Option<&str>)],
    imports: Vec<ImportSpec>,
    exports: ExportMap,
    s3: Vec<S3Registration>,
    dynlibs: Vec<NativeComponent>,
    resources: Vec<ResourceInfo>,
    extra_description: &str,
) -> PackageImage {
    let description = Description::parse(&format!(
        "Package: {name}\nVersion: 1.0.0\n{extra_description}"
    ))
    .unwrap();
    let installed = InstalledPackage {
        id: PackageId {
            name: name.into(),
            version: "1.0.0".into(),
            library: PathBuf::from(format!("/lib/{name}")),
            root: PathBuf::from(format!("/lib/{name}/{name}")),
            image_fingerprint: Digest(format!("fp-{name}")),
        },
        description: description.clone(),
    };
    let mut images = HashMap::new();
    for (binding, source) in bindings {
        let closure = source.map(|source| ClosureSource {
            formals: Arc::from("pairlist()"),
            body: Arc::from(source),
            source: Arc::from(source),
            environment: format!("namespace:{name}"),
        });
        images.insert(
            (*binding).into(),
            BindingImage {
                name: (*binding).into(),
                origin: BindingOrigin::Code,
                object_kind: if closure.is_some() {
                    ObjectKind::Closure
                } else {
                    ObjectKind::Integer
                },
                closure,
                issues: Vec::new(),
            },
        );
    }
    let mut names = images.keys().cloned().collect::<Vec<_>>();
    names.sort();
    PackageImage {
        index: PackageIndex {
            package: installed,
            description,
            exports,
            imports,
            s3,
            dynlibs,
            lifecycle: LifecycleMetadata::default(),
            binding_names: names,
            datasets: Vec::new(),
            resources,
            has_sysdata: false,
        },
        bindings: images,
    }
}

fn export(name: &str) -> ExportMap {
    ExportMap::from([(name.to_owned(), name.to_owned())])
}

fn retained_binding(plan: &hrm::analysis::LinkPlan, package: &str, binding: &str) -> bool {
    plan.retained.iter().any(|need| matches!(need,
        Need::Binding { package: owner, binding: name } if owner.name == package && name == binding
    ))
}

#[test]
fn qualified_access_loads_only_demanded_foreign_binding() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with(
        "foo",
        &[
            ("bar", Some("bar <- function() helper()")),
            ("helper", Some("helper <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
        Vec::new(),
        export("bar"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 4)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(retained_binding(&plan, "foo", "helper"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn foreign_binding_can_pull_another_package_without_rounds() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with(
        "foo",
        &[("bar", Some("bar <- function() baz::qux()"))],
        Vec::new(),
        export("bar"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let baz = package_with(
        "baz",
        &[
            ("qux", Some("qux <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
        Vec::new(),
        export("qux"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo, baz]), 2)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(retained_binding(&plan, "baz", "qux"));
    assert!(!retained_binding(&plan, "baz", "unused"));
}

#[test]
fn activation_follows_effective_imports_without_rooting_exports() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() 1"))],
        vec![ImportSpec::All {
            package: "foo".into(),
            except: Vec::new(),
        }],
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package_with(
        "foo",
        &[
            ("a", Some("a <- function() 1")),
            ("b", Some("b <- function() 2")),
        ],
        Vec::new(),
        ExportMap::from([("a".into(), "a".into()), ("b".into(), "b".into())]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(
        plan.retained
            .iter()
            .any(|need| matches!(need, Need::Activation { package } if package.name == "foo"))
    );
    assert!(!retained_binding(&plan, "foo", "a"));
    assert!(!retained_binding(&plan, "foo", "b"));
}

#[test]
fn renamed_import_from_resolves_remote_binding() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() local_x()"))],
        vec![ImportSpec::From {
            package: "foo".into(),
            bindings: vec![ImportBinding {
                local: "local_x".into(),
                remote: "x".into(),
            }],
        }],
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package_with(
        "foo",
        &[
            ("x", Some("x <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
        Vec::new(),
        export("x"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "x"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn import_all_resolves_reachable_export_only() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() x()"))],
        vec![ImportSpec::All {
            package: "foo".into(),
            except: Vec::new(),
        }],
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package_with(
        "foo",
        &[
            ("x", Some("x <- function() 1")),
            ("y", Some("y <- function() 2")),
        ],
        Vec::new(),
        ExportMap::from([("x".into(), "x".into()), ("y".into(), "y".into())]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "x"));
    assert!(!retained_binding(&plan, "foo", "y"));
}

#[test]
fn non_target_depends_rejects_without_internalizing_depends_package() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() 1"))],
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Depends: foo\n",
    );
    let foo = package("foo", &[("x", Some("x <- function() 1"))]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(
        plan.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DependsAttachmentUnsupported)
    );
    assert!(!retained_binding(&plan, "foo", "x"));
}

#[test]
fn exact_target_provided_package_terminates_internal_traversal() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with(
        "foo",
        &[
            ("bar", Some("bar <- function() hidden()")),
            ("hidden", Some("hidden <- function() 1")),
        ],
        Vec::new(),
        export("bar"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let provider = FakeProvider::new(vec![root, foo]).target("foo");
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();
    assert!(plan.graph.nodes.iter().any(|node| node.package == "foo"
        && matches!(&node.kind, NodeKind::ExternalBinding { name } if name == "bar")));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn require_namespace_default_policy_does_not_ingest_optional_package() {
    let root = package(
        "root",
        &[(
            "f",
            Some("f <- function() requireNamespace(\"foo\", quietly = TRUE)"),
        )],
    );
    let foo = package("foo", &[("x", Some("x <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();
    assert!(
        plan.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn explicit_discovery_policy_can_internalize() {
    let root = package(
        "root",
        &[("f", Some("f <- function() requireNamespace(\"foo\")"))],
    );
    let foo = package("foo", &[("x", Some("x <- function() 1"))]);
    let policy = LinkPolicy {
        namespace_discovery: DiscoveryPolicy::Internalize,
    };
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_policy(policy)
        .analyze("root")
        .unwrap();
    assert!(
        plan.retained
            .iter()
            .any(|need| matches!(need, Need::Activation { package } if package.name == "foo"))
    );
}

#[test]
fn library_and_attaching_require_reject() {
    for source in [
        "f <- function() library(foo)",
        "f <- function() require(foo)",
    ] {
        let root = package("root", &[("f", Some(source))]);
        let plan = Linker::new(FakeProvider::new(vec![root]), 1)
            .analyze("root")
            .unwrap();
        assert!(
            plan.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == RejectCode::PackageAttachmentUnsupported)
        );
    }
}

#[test]
fn locally_shadowed_library_is_not_attachment_semantics() {
    let root = package(
        "root",
        &[
            ("library", Some("library <- function(x) 1")),
            ("f", Some("f <- function() library(foo)")),
        ],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 2)
        .analyze("root")
        .unwrap();
    assert!(
        !plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::PackageAttachmentUnsupported)
    );
    assert!(retained_binding(&plan, "root", "library"));
}

#[test]
fn function_parameter_shadowing_prevents_special_call_semantics() {
    let root = package(
        "root",
        &[(
            "f",
            Some("f <- function(library, deparse) { library(foo); deparse(x) }"),
        )],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(!plan.diagnostics.iter().any(|diagnostic| matches!(
        diagnostic.code,
        RejectCode::PackageAttachmentUnsupported | RejectCode::SyntaxObservation
    )));
}

#[test]
fn cyclic_local_references_terminate_with_one_node_each() {
    let root = package(
        "root",
        &[
            ("a", Some("a <- function() b()")),
            ("b", Some("b <- function() a()")),
        ],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 2)
        .analyze("root")
        .unwrap();
    assert_eq!(
        plan.graph
            .nodes
            .iter()
            .filter(|node| node.package == "root"
                && matches!(&node.kind, NodeKind::Binding { name } if name == "a"))
            .count(),
        1
    );
    assert_eq!(
        plan.graph
            .nodes
            .iter()
            .filter(|node| node.package == "root"
                && matches!(&node.kind, NodeKind::Binding { name } if name == "b"))
            .count(),
        1
    );
}

#[test]
fn native_activation_keeps_component_without_widening_r_bindings() {
    let root = package("root", &[("f", Some("f <- function() foo::a()"))]);
    let foo = package_with(
        "foo",
        &[
            ("a", Some("a <- function(x) .Call(foo_a, x)")),
            ("b", Some("b <- function(x) .Call(foo_b, x)")),
            ("unused", Some("unused <- function(x) x")),
        ],
        Vec::new(),
        export("a"),
        Vec::new(),
        vec![NativeComponent {
            name: "foo".into(),
            callbacks: Vec::new(),
            opaque_r_lookup: false,
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "a"));
    assert!(!retained_binding(&plan, "foo", "b"));
    assert!(!retained_binding(&plan, "foo", "unused"));
    assert!(plan.retained.iter().any(|need| matches!(need, Need::Native { package, component } if package.name == "foo" && component == "foo")));
}

#[test]
fn known_native_callback_adds_binding_edge_and_unknown_lookup_rejects() {
    let root = package("root", &[("f", Some("f <- function() foo::a()"))]);
    let foo = package_with(
        "foo",
        &[
            ("a", Some("a <- function() 1")),
            ("callback", Some("callback <- function() 2")),
        ],
        Vec::new(),
        export("a"),
        Vec::new(),
        vec![NativeComponent {
            name: "foo".into(),
            callbacks: vec!["callback".into()],
            opaque_r_lookup: true,
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "callback"));
    assert!(
        plan.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
    assert!(
        plan.graph
            .edges
            .iter()
            .any(|edge| edge.kind == EdgeKind::Callback)
    );
}

#[test]
fn s3_registration_retains_method_binding() {
    let root = package("root", &[("f", Some("f <- function() foo::x()"))]);
    let foo = package_with(
        "foo",
        &[
            ("x", Some("x <- function() 1")),
            ("print.foo", Some("print.foo <- function(x, ...) x")),
        ],
        Vec::new(),
        export("x"),
        vec![S3Registration {
            generic: "print".into(),
            class: "foo".into(),
            method: "print.foo".into(),
        }],
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "print.foo"));
}

#[test]
fn resource_reference_retains_only_required_path() {
    let root = package(
        "root",
        &[(
            "f",
            Some("f <- function() system.file(\"data\", \"x.json\", package = \"foo\")"),
        )],
    );
    let foo = package_with(
        "foo",
        &[],
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        vec![
            ResourceInfo {
                path: "data/x.json".into(),
            },
            ResourceInfo {
                path: "data/y.json".into(),
            },
        ],
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(plan.retained.iter().any(|need| matches!(need, Need::Resource { package, resource } if package.name == "foo" && resource == "data/x.json")));
    assert!(
        !plan.retained.iter().any(
            |need| matches!(need, Need::Resource { resource, .. } if resource == "data/y.json")
        )
    );
}

#[test]
fn non_closure_binding_never_invokes_air() {
    let root = package("root", &[("constant", None)]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 4)
        .analyze("root")
        .unwrap();
    assert_eq!(plan.parsed_bindings, 0);
    assert!(retained_binding(&plan, "root", "constant"));
}

#[test]
fn package_image_is_requested_once_and_binding_is_parsed_once() {
    let root = package(
        "root",
        &[
            ("a", Some("a <- function() b()")),
            ("b", Some("b <- function() 1")),
        ],
    );
    let provider = FakeProvider::new(vec![root]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 2).analyze("root").unwrap();
    assert_eq!(counts.lock().unwrap().get("root").copied().unwrap_or(0), 1);
    assert_eq!(plan.parsed_bindings, 2);
}

#[test]
fn air_frontend_failure_is_localized_not_package_fatal() {
    let root = package(
        "root",
        &[
            ("good", Some("good <- function() 1")),
            (
                "awkward",
                Some("awkward <- function() sub(\".*\\\\.\", \"\", x = _)"),
            ),
        ],
    );
    let result = Linker::new(FakeProvider::new(vec![root]), 2).analyze("root");
    assert!(result.is_ok());
    let plan = result.unwrap();
    assert!(retained_binding(&plan, "root", "good"));
    if plan
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.binding.as_deref() == Some("awkward"))
    {
        assert!(
            plan.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == RejectCode::AirUnsupportedSyntax)
        );
    }
}

#[test]
fn every_non_root_node_has_an_incoming_reason_and_why_path() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with(
        "foo",
        &[("bar", Some("bar <- function() 1"))],
        Vec::new(),
        export("bar"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    let bar = plan.graph.binding("foo", "bar").unwrap();
    assert!(plan.graph.incoming(bar).next().is_some());
    assert!(plan.graph.shortest_path(&plan.roots, bar).is_some());
}
