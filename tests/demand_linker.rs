#![cfg(feature = "air")]

use slinker::analysis::{DiscoveryPolicy, EdgeKind, LinkPolicy, Linker, Need, NodeKind, RejectCode};
use slinker::package::{
    BindingImage, BindingOrigin, ClosureSource, Digest, ExportMap, ImportBinding, ImportSpec,
    InstalledPackage, LifecycleMetadata, NativeComponent, NativeFacts, NativeSafety, ObjectKind, PackageId, PackageImage,
    PackageIndex, PackageProvider, S3Registration, SyntaxValidation,
};
use slinker::{Description, Error, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct FakeProvider {
    packages: HashMap<String, Arc<PackageImage>>,
    target: HashSet<String>,
    image_counts: Arc<Mutex<HashMap<String, usize>>>,
    optional_locate_counts: Arc<Mutex<HashMap<String, usize>>>,
    validation: SyntaxValidation,
    base: HashSet<String>,
}

impl FakeProvider {
    fn new(images: Vec<PackageImage>) -> Self {
        Self {
            packages: images.into_iter().map(|image| (image.index.package.id.name.clone(), Arc::new(image))).collect(),
            target: HashSet::new(),
            image_counts: Arc::new(Mutex::new(HashMap::new())),
            optional_locate_counts: Arc::new(Mutex::new(HashMap::new())),
            validation: SyntaxValidation::Accepted,
            base: [
                "library", "require", "requireNamespace", "loadNamespace", "getNamespace", "asNamespace",
                "packageVersion", "find.package", "system.file", ".Call", ".C", ".Fortran", ".External",
                "deparse", "substitute", "match.call", "print", "identity", "c", "list", "paste",
                "isNamespaceLoaded", "getNamespaceExports", "setHook", "packageEvent",
                "+", "-", "*", "/", "[", "[[", "$", "<-", "{", "if", "for", "return", "%in%", "&&",
            ].into_iter().map(str::to_owned).collect(),
        }
    }

    fn target(mut self, name: &str) -> Self {
        self.target.insert(name.to_owned());
        self
    }

    fn count_handle(&self) -> Arc<Mutex<HashMap<String, usize>>> {
        Arc::clone(&self.image_counts)
    }

    fn optional_locate_count_handle(&self) -> Arc<Mutex<HashMap<String, usize>>> {
        Arc::clone(&self.optional_locate_counts)
    }

    fn validation(mut self, validation: SyntaxValidation) -> Self {
        self.validation = validation;
        self
    }
}

impl PackageProvider for FakeProvider {
    fn locate(&mut self, name: &str) -> Result<InstalledPackage> {
        self.packages
            .get(name)
            .map(|image| image.index.package.clone())
            .ok_or_else(|| Error::Analysis(format!("missing fake package {name}")))
    }

    fn locate_optional(&mut self, name: &str) -> Result<Option<InstalledPackage>> {
        *self.optional_locate_counts.lock().unwrap().entry(name.to_owned()).or_default() += 1;
        Ok(self.packages.get(name).map(|image| image.index.package.clone()))
    }

    fn index(&mut self, package: &InstalledPackage) -> Result<Arc<PackageIndex>> {
        self.packages
            .get(&package.id.name)
            .map(|image| Arc::new(image.index.clone()))
            .ok_or_else(|| Error::Analysis(format!("missing fake index {}", package.id.name)))
    }

    fn image(&mut self, package: &InstalledPackage) -> Result<Arc<PackageImage>> {
        *self.image_counts.lock().unwrap().entry(package.id.name.clone()).or_default() += 1;
        self.packages
            .get(&package.id.name)
            .cloned()
            .ok_or_else(|| Error::Analysis(format!("missing fake image {}", package.id.name)))
    }

    fn is_target_provided(&self, package: &InstalledPackage) -> bool {
        self.target.contains(&package.id.name)
            || package.description.get("Priority").is_some_and(|value| value == "base")
    }

    fn is_base_binding(&self, name: &str) -> bool {
        self.base.contains(name)
    }

    fn validate_syntax(&self, _id: &PackageId, _binding: &str, _source: &str) -> Result<SyntaxValidation> {
        Ok(self.validation.clone())
    }
}

fn package(name: &str, bindings: &[(&str, Option<&str>)]) -> PackageImage {
    let exports = bindings
        .iter()
        .map(|(binding, _)| ((*binding).to_owned(), (*binding).to_owned()))
        .collect::<ExportMap>();
    package_with(name, bindings, Vec::new(), exports, Vec::new(), Vec::new(), Vec::new(), "")
}

fn package_with(
    name: &str,
    bindings: &[(&str, Option<&str>)],
    imports: Vec<ImportSpec>,
    exports: ExportMap,
    s3: Vec<S3Registration>,
    dynlibs: Vec<NativeComponent>,
    files: Vec<String>,
    extra_description: &str,
) -> PackageImage {
    let description = Description::parse(&format!("Package: {name}\nVersion: 1.0.0\n{extra_description}")).unwrap();
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
            source: Arc::from(source),
            environment: format!("namespace:{name}"),
        });
        images.insert((*binding).into(), BindingImage {
            name: (*binding).into(),
            origin: BindingOrigin::Code,
            object_kind: if closure.is_some() { ObjectKind::Closure } else { ObjectKind::Integer },
            closure,
            embedded_closures: Vec::new(),
            issues: Vec::new(),
        });
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
            files,
            has_sysdata: false,
        },
        bindings: images,
    }
}

fn export(name: &str) -> ExportMap {
    ExportMap::from([(name.to_owned(), name.to_owned())])
}

fn retained_binding(plan: &slinker::analysis::LinkPlan, package: &str, binding: &str) -> bool {
    plan.retained.iter().any(|need| matches!(need,
        Need::Binding { package: owner, binding: name } if owner.name == package && name == binding
    ))
}

#[test]
fn root_starts_from_exports_and_recurses_only_into_referenced_internal_bindings() {
    let root = package_with(
        "root",
        &[
            ("public", Some("public <- function() helper()")),
            ("helper", Some("helper <- function() 1")),
            ("unused_optional", Some("unused_optional <- function() foo::bar()")),
        ],
        Vec::new(),
        export("public"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with(
        "foo",
        &[("bar", Some("bar <- function() hidden()")), ("hidden", Some("hidden <- function() 1"))],
        Vec::new(),
        export("bar"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 2).analyze("root").unwrap();

    assert!(retained_binding(&plan, "root", "public"));
    assert!(retained_binding(&plan, "root", "helper"));
    assert!(!retained_binding(&plan, "root", "unused_optional"));
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn root_lifecycle_remains_an_entrypoint_even_when_not_exported() {
    let mut root = package_with(
        "root",
        &[
            ("public", Some("public <- function() 1")),
            (".onLoad", Some(".onLoad <- function(...) initialize_state()")),
            ("initialize_state", Some("initialize_state <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
        Vec::new(),
        export("public"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    root.index.lifecycle.on_load = true;
    let plan = Linker::new(FakeProvider::new(vec![root]), 1).analyze("root").unwrap();

    assert!(retained_binding(&plan, "root", ".onLoad"));
    assert!(retained_binding(&plan, "root", "initialize_state"));
    assert!(!retained_binding(&plan, "root", "unused"));
}

#[test]
fn root_reexported_import_is_demanded_without_local_binding() {
    let root = package_with(
        "root",
        &[],
        vec![ImportSpec::From {
            package: "utils".into(),
            bindings: vec![ImportBinding { local: "head".into(), remote: "head".into() }],
        }],
        export("head"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Imports: utils\n",
    );
    let utils = package_with(
        "utils",
        &[("head", Some("head <- function(x) x")), ("unused", Some("unused <- function() 1"))],
        Vec::new(),
        export("head"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, utils]), 1).analyze("root").unwrap();

    assert!(retained_binding(&plan, "root", "head"));
    assert!(retained_binding(&plan, "utils", "head"));
    assert!(!retained_binding(&plan, "utils", "unused"));
    assert!(!plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::UnresolvedBinding));
}

#[test]
fn qualified_access_loads_only_demanded_foreign_binding() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with(
        "foo",
        &[("bar", Some("bar <- function() helper()")), ("helper", Some("helper <- function() 1")), ("unused", Some("unused <- function() 2"))],
        Vec::new(), export("bar"), Vec::new(), Vec::new(), Vec::new(), "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 4).analyze("root").unwrap();
    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(retained_binding(&plan, "foo", "helper"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn foreign_binding_can_pull_another_package_without_rounds() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with("foo", &[("bar", Some("bar <- function() baz::qux()"))], Vec::new(), export("bar"), Vec::new(), Vec::new(), Vec::new(), "");
    let baz = package_with("baz", &[("qux", Some("qux <- function() 1")), ("unused", Some("unused <- function() 2"))], Vec::new(), export("qux"), Vec::new(), Vec::new(), Vec::new(), "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo, baz]), 2).analyze("root").unwrap();
    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(retained_binding(&plan, "baz", "qux"));
    assert!(!retained_binding(&plan, "baz", "unused"));
}

#[test]
fn unused_root_import_metadata_does_not_create_reachability() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() 1"))],
        vec![ImportSpec::All { package: "foo".into(), except: Vec::new() }],
        export("f"), Vec::new(), Vec::new(), Vec::new(), "",
    );
    let foo = package_with("foo", &[("a", Some("a <- function() 1")), ("b", Some("b <- function() 2"))], Vec::new(), ExportMap::from([("a".into(), "a".into()), ("b".into(), "b".into())]), Vec::new(), Vec::new(), Vec::new(), "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert!(!retained_binding(&plan, "foo", "a"));
    assert!(!retained_binding(&plan, "foo", "b"));
}

#[test]
fn renamed_import_from_resolves_remote_binding() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() local_x()"))],
        vec![ImportSpec::From { package: "foo".into(), bindings: vec![ImportBinding { local: "local_x".into(), remote: "x".into() }] }],
        export("f"), Vec::new(), Vec::new(), Vec::new(), "",
    );
    let foo = package_with("foo", &[("x", Some("x <- function() 1")), ("unused", Some("unused <- function() 2"))], Vec::new(), export("x"), Vec::new(), Vec::new(), Vec::new(), "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(retained_binding(&plan, "foo", "x"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn import_all_resolves_reachable_export_only() {
    let root = package_with("root", &[("f", Some("f <- function() x()"))], vec![ImportSpec::All { package: "foo".into(), except: Vec::new() }], export("f"), Vec::new(), Vec::new(), Vec::new(), "");
    let foo = package_with("foo", &[("x", Some("x <- function() 1")), ("y", Some("y <- function() 2"))], Vec::new(), ExportMap::from([("x".into(), "x".into()), ("y".into(), "y".into())]), Vec::new(), Vec::new(), Vec::new(), "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(retained_binding(&plan, "foo", "x"));
    assert!(!retained_binding(&plan, "foo", "y"));
}

#[test]
fn depends_metadata_alone_does_not_create_reachability() {
    let root = package_with("root", &[("f", Some("f <- function() 1"))], Vec::new(), export("f"), Vec::new(), Vec::new(), Vec::new(), "Depends: foo\n");
    let foo = package("foo", &[("x", Some("x <- function() 1"))]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert!(!retained_binding(&plan, "foo", "x"));
}

#[test]
fn exact_target_provided_package_terminates_internal_traversal() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with("foo", &[("bar", Some("bar <- function() hidden()")), ("hidden", Some("hidden <- function() 1"))], Vec::new(), export("bar"), Vec::new(), Vec::new(), Vec::new(), "");
    let provider = FakeProvider::new(vec![root, foo]).target("foo");
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();
    assert!(plan.graph.nodes.iter().any(|node| node.package == "foo" && matches!(&node.kind, NodeKind::ExternalBinding { name } if name == "bar")));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn require_namespace_default_policy_does_not_ingest_optional_package() {
    let root = package("root", &[("f", Some("f <- function() requireNamespace(\"foo\", quietly = TRUE)"))]);
    let foo = package("foo", &[("x", Some("x <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();
    assert!(plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn explicit_discovery_policy_can_internalize() {
    let root = package("root", &[("f", Some("f <- function() requireNamespace(\"foo\")"))]);
    let foo = package("foo", &[("x", Some("x <- function() 1"))]);
    let policy = LinkPolicy { namespace_discovery: DiscoveryPolicy::Internalize };
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).with_policy(policy).analyze("root").unwrap();
    assert!(plan.retained.iter().any(|need| matches!(need, Need::Activation { package } if package.name == "foo")));
}

#[test]
fn library_and_attaching_require_reject() {
    for source in ["f <- function() library(foo)", "f <- function() require(foo)"] {
        let root = package("root", &[("f", Some(source))]);
        let plan = Linker::new(FakeProvider::new(vec![root]), 1).analyze("root").unwrap();
        assert!(plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::PackageAttachmentUnsupported));
    }
}

#[test]
fn locally_shadowed_library_is_not_attachment_semantics() {
    let root = package("root", &[("library", Some("library <- function(x) 1")), ("f", Some("f <- function() library(foo)"))]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 2).analyze("root").unwrap();
    assert!(!plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::PackageAttachmentUnsupported));
    assert!(retained_binding(&plan, "root", "library"));
}


#[test]
fn function_parameter_shadowing_prevents_special_call_semantics() {
    let root = package("root", &[("f", Some("f <- function(library, deparse) { library(foo); deparse(x) }"))]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 1).analyze("root").unwrap();
    assert!(!plan.diagnostics.iter().any(|diagnostic| matches!(diagnostic.code, RejectCode::PackageAttachmentUnsupported | RejectCode::SyntaxObservation)));
}

#[test]
fn cyclic_local_references_terminate_with_one_node_each() {
    let root = package("root", &[("a", Some("a <- function() b()")), ("b", Some("b <- function() a()"))]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 2).analyze("root").unwrap();
    assert_eq!(plan.graph.nodes.iter().filter(|node| node.package == "root" && matches!(&node.kind, NodeKind::Binding { name } if name == "a")).count(), 1);
    assert_eq!(plan.graph.nodes.iter().filter(|node| node.package == "root" && matches!(&node.kind, NodeKind::Binding { name } if name == "b")).count(), 1);
}

#[test]
fn native_activation_keeps_component_without_widening_r_bindings() {
    let root = package("root", &[("f", Some("f <- function() foo::a()"))]);
    let foo = package_with("foo", &[("a", Some("a <- function(x) .Call(foo_a, x)")), ("b", Some("b <- function(x) .Call(foo_b, x)")), ("unused", Some("unused <- function(x) x"))], Vec::new(), export("a"), Vec::new(), vec![NativeComponent { name: "foo".into(), safety: NativeSafety::Safe(NativeFacts { callbacks: Vec::new() }) }], Vec::new(), "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(retained_binding(&plan, "foo", "a"));
    assert!(!retained_binding(&plan, "foo", "b"));
    assert!(!retained_binding(&plan, "foo", "unused"));
    assert!(plan.retained.iter().any(|need| matches!(need, Need::Native { package, component } if package.name == "foo" && component == "foo")));
}

#[test]
fn known_native_callback_adds_binding_edge() {
    let root = package("root", &[("f", Some("f <- function() foo::a()"))]);
    let foo = package_with(
        "foo",
        &[("a", Some("a <- function() 1")), ("callback", Some("callback <- function() 2"))],
        Vec::new(),
        export("a"),
        Vec::new(),
        vec![NativeComponent {
            name: "foo".into(),
            safety: NativeSafety::Safe(NativeFacts { callbacks: vec!["callback".into()] }),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(retained_binding(&plan, "foo", "callback"));
    assert!(plan.graph.edges.iter().any(|edge| edge.kind == EdgeKind::Callback));
    assert!(!plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup));
}

#[test]
fn unsupported_native_lookup_rejects_without_widening_r_namespace() {
    let root = package("root", &[("f", Some("f <- function() foo::a()"))]);
    let foo = package_with(
        "foo",
        &[("a", Some("a <- function() 1")), ("callback", Some("callback <- function() 2"))],
        Vec::new(),
        export("a"),
        Vec::new(),
        vec![NativeComponent {
            name: "foo".into(),
            safety: NativeSafety::Unsupported(vec!["dynamic R lookup".into()]),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(!retained_binding(&plan, "foo", "callback"));
    assert!(plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup));
}

#[test]
fn dependency_activation_does_not_root_unreachable_s3_methods() {
    let root = package("root", &[("f", Some("f <- function() foo::x()"))]);
    let foo = package_with("foo", &[("x", Some("x <- function() 1")), ("print.foo", Some("print.foo <- function(x, ...) x"))], Vec::new(), export("x"), vec![S3Registration { generic: "print".into(), class: "foo".into(), method: "print.foo".into() }], Vec::new(), Vec::new(), "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(retained_binding(&plan, "foo", "x"));
    assert!(!retained_binding(&plan, "foo", "print.foo"));
}

#[test]
fn resource_reference_retains_only_required_path() {
    let root = package("root", &[("f", Some("f <- function() system.file(\"data\", \"x.json\", package = \"foo\")"))]);
    let foo = package_with("foo", &[], Vec::new(), ExportMap::new(), Vec::new(), Vec::new(), vec!["data/x.json".into(), "data/y.json".into()], "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(plan.retained.iter().any(|need| matches!(need, Need::Resource { package, resource } if package.name == "foo" && resource == "data/x.json")));
    assert!(!plan.retained.iter().any(|need| matches!(need, Need::Resource { resource, .. } if resource == "data/y.json")));
}

#[test]
fn non_closure_binding_never_invokes_air() {
    let root = package("root", &[("constant", None)]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 4).analyze("root").unwrap();
    assert_eq!(plan.parsed_bindings, 0);
    assert!(retained_binding(&plan, "root", "constant"));
}

#[test]
fn package_image_is_requested_once_and_binding_is_parsed_once() {
    let root = package("root", &[("a", Some("a <- function() b()")), ("b", Some("b <- function() 1"))]);
    let provider = FakeProvider::new(vec![root]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 2).analyze("root").unwrap();
    assert_eq!(counts.lock().unwrap().get("root").copied().unwrap_or(0), 1);
    assert_eq!(plan.parsed_bindings, 2);
}

#[test]
fn air_frontend_failure_is_localized_not_package_fatal() {
    // Deliberately make Air reject this binding while the fake target-R
    // validator reports acceptance. This isolates disagreement handling from
    // any particular real-R grammar edge case.
    let root = package("root", &[
        ("good", Some("good <- function() 1")),
        ("awkward", Some("awkward <- function() {")),
    ]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 2).analyze("root").unwrap();

    assert!(retained_binding(&plan, "root", "good"));
    assert!(plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("awkward")
            && diagnostic.code == RejectCode::AirUnsupportedSyntax
    }));
}

#[test]
fn air_accepted_unknown_name_is_a_semantic_error_not_a_frontend_error() {
    let root = package("root", &[("f", Some("f <- function() missing_symbol()"))]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 1).analyze("root").unwrap();

    assert!(plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::UnresolvedBinding
    }));
    assert!(!plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::AirUnsupportedSyntax
    }));
}

#[test]
fn air_and_target_rejection_is_invalid_installed_representation() {
    let root = package("root", &[("awkward", Some("awkward <- function() {"))]);
    let provider = FakeProvider::new(vec![root])
        .validation(SyntaxValidation::Rejected("unexpected end of input".into()));
    let plan = Linker::new(provider, 1).analyze("root").unwrap();

    assert!(plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("awkward")
            && diagnostic.code == RejectCode::InvalidInstalledRepresentation
    }));
    assert!(!plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("awkward")
            && diagnostic.code == RejectCode::AirUnsupportedSyntax
    }));
}

#[test]
fn every_non_root_node_has_an_incoming_reason_and_why_path() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with("foo", &[("bar", Some("bar <- function() 1"))], Vec::new(), export("bar"), Vec::new(), Vec::new(), Vec::new(), "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    let bar = plan.graph.binding("foo", "bar").unwrap();
    assert!(plan.graph.incoming(bar).next().is_some());
    assert!(plan.graph.shortest_path(&plan.roots, bar).is_some());
}

#[test]
fn missing_packages_are_collated_instead_of_failing_fast() {
    let root = package("root", &[("a", Some("a <- function() foo::x()")), ("b", Some("b <- function() bar::y()"))]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 2).analyze("root").unwrap();
    let mut missing = plan
        .graph
        .missing_packages()
        .map(|id| plan.graph.nodes[id.0].package.clone())
        .collect::<Vec<_>>();
    missing.sort();
    assert_eq!(missing, vec!["bar", "foo"]);
    assert_eq!(plan.diagnostics.iter().filter(|d| d.code == RejectCode::MissingDependency).count(), 2);
}

#[test]
fn unused_dependency_import_does_not_pull_or_report_missing_package() {
    let root = package("root", &[("f", Some("f <- function() foo::x()"))]);
    let foo = package_with(
        "foo",
        &[("x", Some("x <- function() 1"))],
        vec![ImportSpec::From {
            package: "otelsdk".into(),
            bindings: vec![ImportBinding { local: "span".into(), remote: "span".into() }],
        }],
        export("x"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(retained_binding(&plan, "foo", "x"));
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "otelsdk"));
}

#[test]
fn reachable_dependency_import_reports_missing_package_with_provenance() {
    let root = package("root", &[("f", Some("f <- function() foo::x()"))]);
    let foo = package_with(
        "foo",
        &[("x", Some("x <- function() span()"))],
        vec![ImportSpec::From {
            package: "otelsdk".into(),
            bindings: vec![ImportBinding { local: "span".into(), remote: "span".into() }],
        }],
        export("x"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    let missing = plan.graph.nodes.iter().find(|node| node.package == "otelsdk" && matches!(&node.kind, NodeKind::MissingPackage)).unwrap();
    let path = plan.graph.shortest_path(&plan.roots, missing.id).unwrap();
    assert!(path.iter().any(|edge| edge.kind == EdgeKind::Import));
    assert!(path.iter().any(|edge| edge.reason.contains("span")));
}

#[test]
fn missing_unused_root_import_is_not_reported() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() 1"))],
        vec![ImportSpec::All { package: "required_at_root_load".into(), except: Vec::new() }],
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1).analyze("root").unwrap();
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "required_at_root_load"));
}

#[test]
fn absent_optional_resource_is_not_a_blocker() {
    let root = package("root", &[("f", Some("f <- function() system.file(\"missing\", package = \"foo\")"))]);
    let foo = package_with("foo", &[], Vec::new(), ExportMap::new(), Vec::new(), Vec::new(), Vec::new(), "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(!plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::MissingResource));
}

#[test]
fn absent_must_work_resource_is_a_precise_blocker() {
    let root = package("root", &[("f", Some("f <- function() system.file(\"missing\", package = \"foo\", mustWork = TRUE)"))]);
    let foo = package_with("foo", &[], Vec::new(), ExportMap::new(), Vec::new(), Vec::new(), Vec::new(), "");
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();
    assert!(plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::MissingResource));
}

#[test]
fn suggests_alone_never_enters_the_graph() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() 1"))],
        Vec::new(), export("f"), Vec::new(), Vec::new(), Vec::new(), "Suggests: foo\n",
    );
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn selecting_extra_does_not_root_an_unused_optional_package() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() 1"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1)
        .with_extra_packages(["foo".to_owned()])
        .analyze("root")
        .unwrap();

    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn effective_namespace_import_is_required_even_if_description_also_suggests_it() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() imported_bar()"))],
        vec![ImportSpec::From {
            package: "foo".into(),
            bindings: vec![ImportBinding { local: "imported_bar".into(), remote: "bar".into() }],
        }],
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with(
        "foo",
        &[("bar", Some("bar <- function() 1")), ("unused", Some("unused <- function() 2"))],
        Vec::new(),
        export("bar"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();

    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn config_needs_does_not_enable_a_suggested_runtime_package() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() foo::bar()"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\nConfig/Needs/website: foo, bar\n",
    );
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let locate_counts = provider.optional_locate_count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();

    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
    assert_eq!(locate_counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn required_description_relationship_wins_over_duplicate_suggests_when_source_uses_package() {
    for required_field in ["Imports", "Depends"] {
        let root = package_with(
            "root",
            &[("f", Some("f <- function() foo::bar()"))],
            Vec::new(),
            export("f"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            &format!("{required_field}: foo\nSuggests: foo\n"),
        );
        let foo = package_with(
            "foo",
            &[("bar", Some("bar <- function() 1")), ("unused", Some("unused <- function() 2"))],
            Vec::new(),
            export("bar"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "",
        );
        let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();

        assert!(retained_binding(&plan, "foo", "bar"), "{required_field} should make foo required when source uses it");
        assert!(!retained_binding(&plan, "foo", "unused"));
    }
}

#[test]
fn direct_suggested_namespace_access_is_ignored_without_extra_pkgs() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() foo::bar()"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with(
        "foo",
        &[("bar", Some("bar <- function() helper()")), ("helper", Some("helper <- function() 1"))],
        Vec::new(),
        export("bar"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let locate_counts = provider.optional_locate_count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();

    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert!(!plan.rewrites.iter().any(|rewrite| matches!(rewrite,
        slinker::build::Rewrite::NamespaceAccess { package, .. } if package.name == "foo"
    )));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
    assert_eq!(locate_counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn direct_suggested_namespace_access_is_internalized_when_selected() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() foo::bar()"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
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
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_extra_packages(["foo".to_owned()])
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(retained_binding(&plan, "foo", "helper"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn selecting_one_extra_does_not_enable_its_suggests() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() foo::bar()"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with(
        "foo",
        &[("bar", Some("bar <- function() baz::qux()"))],
        Vec::new(),
        export("bar"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: baz\n",
    );
    let baz = package("baz", &[("qux", Some("qux <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo, baz]);
    let counts = provider.count_handle();
    let locate_counts = provider.optional_locate_count_handle();
    let plan = Linker::new(provider, 2)
        .with_extra_packages(["foo".to_owned()])
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "baz"));
    assert_eq!(counts.lock().unwrap().get("baz").copied().unwrap_or(0), 0);
    assert_eq!(locate_counts.lock().unwrap().get("baz").copied().unwrap_or(0), 0);
}

#[test]
fn cli_like_unreachable_optional_helpers_do_not_expand_suggests() {
    let root = package_with(
        "cli",
        &[
            ("cli_alert", Some("cli_alert <- function() format_alert()")),
            ("format_alert", Some("format_alert <- function() paste('ok')")),
            ("knitr_helper", Some("knitr_helper <- function() knitr::knit()")),
            ("testthat_helper", Some("testthat_helper <- function() testthat::test_that('x', function() 1)")),
            ("rmarkdown_helper", Some("rmarkdown_helper <- function() rmarkdown::render('x.Rmd')")),
        ],
        Vec::new(),
        export("cli_alert"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests:\n    knitr,\n    testthat,\n    rmarkdown\n",
    );
    let knitr = package("knitr", &[("knit", Some("knit <- function() 1"))]);
    let testthat = package("testthat", &[("test_that", Some("test_that <- function(...) 1"))]);
    let rmarkdown = package("rmarkdown", &[("render", Some("render <- function(...) 1"))]);
    let provider = FakeProvider::new(vec![root, knitr, testthat, rmarkdown]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 4).analyze("cli").unwrap();

    assert!(retained_binding(&plan, "cli", "cli_alert"));
    assert!(retained_binding(&plan, "cli", "format_alert"));
    assert!(!retained_binding(&plan, "cli", "knitr_helper"));
    assert!(!retained_binding(&plan, "cli", "testthat_helper"));
    assert!(!retained_binding(&plan, "cli", "rmarkdown_helper"));
    for optional in ["knitr", "testthat", "rmarkdown"] {
        assert!(!plan.graph.nodes.iter().any(|node| node.package == optional));
        assert_eq!(counts.lock().unwrap().get(optional).copied().unwrap_or(0), 0);
    }
}

#[test]
fn cli_like_required_import_is_demanded_while_suggests_stay_out() {
    let root = package_with(
        "cli",
        &[
            ("cli_head", Some("cli_head <- function(x) head(x)")),
            ("knitr_helper", Some("knitr_helper <- function() knitr::knit()")),
            ("rlang_helper", Some("rlang_helper <- function() rlang::env()")),
            ("testthat_helper", Some("testthat_helper <- function() testthat::test_that('x', function() 1)")),
        ],
        vec![ImportSpec::From {
            package: "utils".into(),
            bindings: vec![ImportBinding { local: "head".into(), remote: "head".into() }],
        }],
        export("cli_head"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Imports:\n    utils\nSuggests:\n    knitr,\n    rlang,\n    testthat\n",
    );
    let utils = package_with(
        "utils",
        &[("head", Some("head <- function(x) x")), ("unused", Some("unused <- function() 1"))],
        Vec::new(),
        export("head"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let knitr = package("knitr", &[("knit", Some("knit <- function() 1"))]);
    let rlang = package("rlang", &[("env", Some("env <- function() 1"))]);
    let testthat = package("testthat", &[("test_that", Some("test_that <- function(...) 1"))]);
    let provider = FakeProvider::new(vec![root, utils, knitr, rlang, testthat]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 4).analyze("cli").unwrap();

    assert!(retained_binding(&plan, "utils", "head"));
    assert!(!retained_binding(&plan, "utils", "unused"));
    for optional in ["knitr", "rlang", "testthat"] {
        assert!(!plan.graph.nodes.iter().any(|node| node.package == optional));
        assert_eq!(counts.lock().unwrap().get(optional).copied().unwrap_or(0), 0);
    }
}

#[test]
fn unselected_suggested_resource_does_not_discover_package() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() system.file('data', 'x.json', package = 'foo')"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with(
        "foo",
        &[],
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        vec!["data/x.json".into()],
        "",
    );
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();

    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert!(!plan.rewrites.iter().any(|rewrite| matches!(rewrite,
        slinker::build::Rewrite::ResourceAccess { package, .. } if package.name == "foo"
    )));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn unselected_suggested_attachment_call_is_ignored() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() require(foo)"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1).analyze("root").unwrap();

    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert!(!plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::PackageAttachmentUnsupported));
}

#[test]
fn unselected_suggested_s3_generic_does_not_retain_optional_registration_method() {
    let root = package_with(
        "root",
        &[
            ("public", Some("public <- function() 1")),
            ("print.foo", Some("print.foo <- function(x, ...) x")),
        ],
        Vec::new(),
        export("public"),
        vec![S3Registration {
            generic: "foo::print".into(),
            class: "foo".into(),
            method: "print.foo".into(),
        }],
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package("foo", &[("print", Some("print <- function(x, ...) x"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();

    assert!(!retained_binding(&plan, "root", "print.foo"));
    assert!(!plan.retained.iter().any(|need| matches!(need,
        Need::S3Registration { registration, .. } if registration.generic == "foo::print"
    )));
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn retained_dependency_method_does_not_pull_unselected_suggested_generic() {
    let root = package("root", &[("f", Some("f <- function() dep::method()"))]);
    let dep = package_with(
        "dep",
        &[("method", Some("method <- function(x = NULL, ...) x"))],
        Vec::new(),
        export("method"),
        vec![S3Registration {
            generic: "foo::generic".into(),
            class: "dep_class".into(),
            method: "method".into(),
        }],
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package("foo", &[("generic", Some("generic <- function(x, ...) x"))]);
    let provider = FakeProvider::new(vec![root, dep, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 2).analyze("root").unwrap();

    assert!(retained_binding(&plan, "dep", "method"));
    assert!(!plan.retained.iter().any(|need| matches!(need,
        Need::S3Registration { package, registration }
            if package.name == "dep" && registration.generic == "foo::generic"
    )));
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn selected_extra_enables_retained_dependency_s3_generic() {
    let root = package("root", &[("f", Some("f <- function() dep::method()"))]);
    let dep = package_with(
        "dep",
        &[("method", Some("method <- function(x = NULL, ...) x"))],
        Vec::new(),
        export("method"),
        vec![S3Registration {
            generic: "foo::generic".into(),
            class: "dep_class".into(),
            method: "method".into(),
        }],
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package("foo", &[("generic", Some("generic <- function(x, ...) x"))]);
    let plan = Linker::new(FakeProvider::new(vec![root, dep, foo]), 2)
        .with_extra_packages(["foo".to_owned()])
        .analyze("root")
        .unwrap();

    assert!(plan.retained.iter().any(|need| matches!(need,
        Need::S3Registration { package, registration }
            if package.name == "dep" && registration.generic == "foo::generic"
    )));
    assert!(plan.retained.iter().any(|need| matches!(need,
        Need::Activation { package } if package.name == "foo"
    )));
}

#[test]
fn unselected_suggested_guard_prunes_optional_branch() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()"))],
        Vec::new(), export("f"), Vec::new(), Vec::new(), Vec::new(), "Suggests: foo\n",
    );
    let foo = package_with(
        "foo",
        &[("bar", Some("bar <- function() hidden()")), ("hidden", Some("hidden <- function() 1"))],
        Vec::new(), export("bar"), Vec::new(), Vec::new(), Vec::new(), "",
    );
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "foo"));
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
    assert!(plan.rewrites.iter().any(|rewrite| matches!(rewrite,
        slinker::build::Rewrite::PackageOperation {
            operation: slinker::build::PackageOperation::RequireNamespace { result: false }, ..
        }
    )));
}

#[test]
fn selected_extra_enables_guarded_optional_branch_without_rooting_whole_package() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()"))],
        Vec::new(), export("f"), Vec::new(), Vec::new(), Vec::new(), "Suggests: foo\n",
    );
    let foo = package_with(
        "foo",
        &[("bar", Some("bar <- function() helper()")), ("helper", Some("helper <- function() 1")), ("unused", Some("unused <- function() 2"))],
        Vec::new(), export("bar"), Vec::new(), Vec::new(), Vec::new(), "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_extra_packages(["foo".to_owned()])
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(retained_binding(&plan, "foo", "helper"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn selected_missing_extra_is_reported_as_missing_dependency() {
    let root = package_with(
        "root",
        &[("f", Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()"))],
        Vec::new(), export("f"), Vec::new(), Vec::new(), Vec::new(), "Suggests: foo\n",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .with_extra_packages(["foo".to_owned()])
        .analyze("root")
        .unwrap();
    assert!(plan.graph.nodes.iter().any(|node| node.package == "foo" && matches!(&node.kind, NodeKind::MissingPackage)));
}

#[test]
fn optional_onload_hook_does_not_activate_suggested_namespace() {
    let root = package("root", &[("f", Some("f <- function() glue::glue(\"x\")"))]);
    let mut glue = package_with(
        "glue",
        &[
            ("glue", Some("glue <- function(x) x")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(...) { if (isNamespaceLoaded(\"knitr\") && \"knit_engines\" %in% getNamespaceExports(\"knitr\")) { knitr::knit_engines$set(glue = glue) } else { setHook(packageEvent(\"knitr\", \"onLoad\"), function(...) knitr::knit_engines$set(glue = glue)) } }",
                ),
            ),
        ],
        Vec::new(),
        export("glue"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: knitr\n",
    );
    glue.index.lifecycle.on_load = true;
    let plan = Linker::new(FakeProvider::new(vec![root, glue]), 4).analyze("root").unwrap();
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "knitr"));
    assert!(!plan.diagnostics.iter().any(|diagnostic| diagnostic.code == RejectCode::MissingDependency));
}

#[test]
fn selected_extra_enables_optional_onload_hook_namespace() {
    let root = package("root", &[("f", Some("f <- function() glue::glue(\"x\")"))]);
    let mut glue = package_with(
        "glue",
        &[
            ("glue", Some("glue <- function(x) x")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(...) { if (isNamespaceLoaded(\"knitr\")) knitr::knit_engines$set(glue = glue) else setHook(packageEvent(\"knitr\", \"onLoad\"), function(...) knitr::knit_engines$set(glue = glue)) }",
                ),
            ),
        ],
        Vec::new(),
        export("glue"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: knitr\n",
    );
    glue.index.lifecycle.on_load = true;
    let knitr = package_with(
        "knitr",
        &[("knit_engines", None), ("unused", Some("unused <- function() 1"))],
        Vec::new(),
        export("knit_engines"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, glue, knitr]), 4)
        .with_extra_packages(["knitr".to_owned()])
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "knitr", "knit_engines"));
    assert!(!retained_binding(&plan, "knitr", "unused"));
}

#[test]
fn target_provided_namespace_is_not_assumed_loaded_for_onload_guard() {
    let root = package("root", &[("f", Some("f <- function() glue::glue(\"x\")"))]);
    let mut glue = package_with(
        "glue",
        &[
            ("glue", Some("glue <- function(x) x")),
            (
                ".onLoad",
                Some(".onLoad <- function(...) if (isNamespaceLoaded(\"knitr\")) knitr::knit_engines$set(glue = glue)"),
            ),
        ],
        Vec::new(),
        export("glue"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: knitr\n",
    );
    glue.index.lifecycle.on_load = true;
    let knitr = package_with(
        "knitr",
        &[("knit_engines", None)],
        Vec::new(),
        export("knit_engines"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Priority: base\n",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, glue, knitr]).target("knitr"), 4)
        .analyze("root")
        .unwrap();
    assert!(!plan.graph.nodes.iter().any(|node| node.package == "knitr"));
}
