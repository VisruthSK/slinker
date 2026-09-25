#![cfg(feature = "air")]

use slinker::analysis::{
    DiscoveryPolicy, EdgeKind, ExplanationDag, GraphEdgeReasonExport, GraphExport, LinkPolicy,
    Linker, NodeKind, RejectCode,
};
use slinker::package::{
    BindingImage, BindingOrigin, ClosureSource, Digest, EmbeddedClosureSource, ExportMap,
    ImportBinding, ImportSpec, InstalledPackage, LifecycleMetadata, NativeComponent, NativeFacts,
    NativeRegistration, NativeRoutineSummary, NativeSafety, NativeSymbolBinding, ObjectIssue,
    ObjectKind, PackageIdentity, PackageImage, PackageIndex, PackageLocation, PackageProvider,
    PrivateBindingImage, PrivateEnvironmentImage, S3Registration, SyntaxValidation,
};
use slinker::{Description, Error, Result, Target, TargetEnvironment};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct FakeProvider {
    packages: HashMap<String, Arc<PackageImage>>,
    target_environment: TargetEnvironment,
    image_counts: Arc<Mutex<HashMap<String, usize>>>,
    optional_locate_counts: Arc<Mutex<HashMap<String, usize>>>,
    validation: SyntaxValidation,
}

impl FakeProvider {
    fn new(images: Vec<PackageImage>) -> Self {
        Self {
            packages: images
                .into_iter()
                .map(|image| (image.index.identity.name.clone(), Arc::new(image)))
                .collect(),
            image_counts: Arc::new(Mutex::new(HashMap::new())),
            optional_locate_counts: Arc::new(Mutex::new(HashMap::new())),
            validation: SyntaxValidation::Accepted,
            target_environment: TargetEnvironment {
                r_home: PathBuf::from("/opt/R"),
                target: Target {
                    r_version: "4.6.1".into(),
                    os: "test".into(),
                    arch: "test".into(),
                },
                libraries: Vec::new(),
                base_bindings: [
                    "library",
                    "require",
                    "requireNamespace",
                    "loadNamespace",
                    "getNamespace",
                    "asNamespace",
                    "packageVersion",
                    "find.package",
                    "system.file",
                    ".Call",
                    ".C",
                    ".Fortran",
                    ".External",
                    "deparse",
                    "substitute",
                    "match.call",
                    "quote",
                    "bquote",
                    "print",
                    "identity",
                    "is.null",
                    "c",
                    "list",
                    "paste",
                    "paste0",
                    "strsplit",
                    "switch",
                    "names",
                    "isNamespaceLoaded",
                    "getNamespaceExports",
                    "setHook",
                    "packageEvent",
                    "makeActiveBinding",
                    "environment",
                    "UseMethod",
                    "NextMethod",
                    "new.env",
                    "list2env",
                    "lapply",
                    "is.function",
                    "length",
                    "==",
                    "stop",
                    "+",
                    "-",
                    "*",
                    "/",
                    "[",
                    "[[",
                    "$",
                    "<-",
                    "{",
                    "if",
                    "for",
                    "return",
                    "%in%",
                    "&&",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            },
        }
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
    fn target_environment(&self) -> &TargetEnvironment {
        &self.target_environment
    }

    fn locate(&mut self, name: &str) -> Result<Option<InstalledPackage>> {
        *self
            .optional_locate_counts
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_default() += 1;
        Ok(self.packages.get(name).map(|image| installed(&image.index)))
    }

    fn index(&mut self, package: &InstalledPackage) -> Result<Arc<PackageIndex>> {
        self.packages
            .get(&package.identity.name)
            .map(|image| Arc::clone(&image.index))
            .ok_or_else(|| Error::Analysis(format!("missing fake index {}", package.identity.name)))
    }

    fn binding_image(
        &mut self,
        package: &InstalledPackage,
        _name: &str,
    ) -> Result<Arc<PackageImage>> {
        *self
            .image_counts
            .lock()
            .unwrap()
            .entry(package.identity.name.clone())
            .or_default() += 1;
        self.packages
            .get(&package.identity.name)
            .cloned()
            .ok_or_else(|| Error::Analysis(format!("missing fake image {}", package.identity.name)))
    }

    fn validate_syntax(&mut self, _source: &str) -> Result<SyntaxValidation> {
        Ok(self.validation.clone())
    }

    fn normalize_syntax(&mut self, source: &str) -> Result<String> {
        Ok(source.to_owned())
    }
}

fn package(name: &str, bindings: &[(&str, Option<&str>)]) -> PackageImage {
    let exports = bindings
        .iter()
        .map(|(binding, _)| ((*binding).to_owned(), (*binding).to_owned()))
        .collect::<ExportMap>();
    package_from_fixture(
        name,
        bindings,
        FixtureMetadata {
            imports: Vec::new(),
            exports,
            s3: Vec::new(),
            dynlibs: Vec::new(),
            files: Vec::new(),
            extra_description: String::new(),
        },
    )
}

struct FixtureMetadata {
    imports: Vec<ImportSpec>,
    exports: ExportMap,
    s3: Vec<S3Registration>,
    dynlibs: Vec<NativeComponent>,
    files: Vec<String>,
    extra_description: String,
}

macro_rules! package_with {
    ($name:expr, $bindings:expr, $imports:expr, $exports:expr, $s3:expr, $dynlibs:expr, $files:expr, $description:expr $(,)?) => {
        package_from_fixture(
            $name,
            $bindings,
            FixtureMetadata {
                imports: $imports,
                exports: $exports,
                s3: $s3,
                dynlibs: $dynlibs,
                files: $files,
                extra_description: $description.into(),
            },
        )
    };
}

fn package_from_fixture(
    name: &str,
    bindings: &[(&str, Option<&str>)],
    metadata: FixtureMetadata,
) -> PackageImage {
    let FixtureMetadata {
        imports,
        exports,
        s3,
        dynlibs,
        files,
        extra_description,
    } = metadata;
    let description = Description::parse(&format!(
        "Package: {name}\nVersion: 1.0.0\n{extra_description}"
    ));
    let mut images = HashMap::new();
    for (binding, source) in bindings {
        let closure = source.map(|source| ClosureSource {
            source: Arc::from(source),
            environment: format!("namespace:{name}"),
        });
        images.insert(
            (*binding).into(),
            BindingImage {
                name: (*binding).into(),
                origin: BindingOrigin::Code,
                representation: slinker::package::BindingRepresentation::Value,
                classes: Vec::new(),
                object_kind: if closure.is_some() {
                    ObjectKind::Closure
                } else {
                    ObjectKind::Integer
                },
                closure,
                environment: None,
                embedded_closures: Vec::new(),
                embedded_environments: Vec::new(),
                issues: Vec::new(),
            },
        );
    }
    let mut names = images.keys().cloned().collect::<Vec<_>>();
    names.sort();
    PackageImage {
        index: Arc::new(PackageIndex {
            identity: PackageIdentity {
                name: name.into(),
                version: "1.0.0".parse().expect("valid test package version"),
                image_fingerprint: Digest(format!("fp-{name}")),
            },
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
        }),
        bindings: images,
        private_environments: HashMap::new(),
    }
}

fn installed(index: &PackageIndex) -> InstalledPackage {
    let name = &index.identity.name;
    InstalledPackage {
        identity: index.identity.clone(),
        location: PackageLocation {
            library: PathBuf::from(format!("/lib/{name}")),
            root: PathBuf::from(format!("/lib/{name}/{name}")),
        },
        description: index.description.clone(),
    }
}

fn export(name: &str) -> ExportMap {
    ExportMap::from([(name.to_owned(), name.to_owned())])
}

fn test_target() -> TargetEnvironment {
    TargetEnvironment {
        r_home: PathBuf::from("/opt/R"),
        target: Target {
            r_version: "4.6.1".into(),
            os: "mingw32".into(),
            arch: "x86_64".into(),
        },
        libraries: Vec::new(),
        base_bindings: Default::default(),
    }
}

fn retained_binding(plan: &slinker::analysis::LinkIr, package: &str, binding: &str) -> bool {
    plan.provenance().nodes().iter().any(|node| {
        node.package == package
            && matches!(&node.kind, NodeKind::Binding { name } if name == binding)
    })
}

fn retained_private_binding(
    plan: &slinker::analysis::LinkIr,
    package: &str,
    environment: &str,
    binding: &str,
) -> bool {
    plan.provenance().nodes().iter().any(|node| {
        node.package == package
            && matches!(&node.kind,
                NodeKind::PrivateBinding { environment: owner_environment, name }
                    if owner_environment == environment && name == binding)
    })
}

fn program_has_s3_registration(
    plan: &slinker::analysis::LinkIr,
    owner: &str,
    generic_package: Option<&str>,
    generic: &str,
    class: &str,
    method: &str,
) -> bool {
    plan.program()
        .s3_registrations()
        .iter()
        .any(|registration| {
            let namespace = plan.program().namespace(registration.owner_namespace);
            let owner_matches = plan.program().package(namespace.package).identity().name == owner;
            let generic_package_matches = registration
                .generic
                .package
                .map(|package| plan.program().package(package).identity().name.as_str())
                == generic_package;
            owner_matches
                && generic_package_matches
                && registration.generic.name == generic
                && registration.class == class
                && plan.program().binding(registration.method).name == method
        })
}

fn private_closure(name: &str, environment: &str, source: &str) -> PrivateBindingImage {
    PrivateBindingImage {
        name: name.into(),
        representation: slinker::package::BindingRepresentation::Value,
        classes: Vec::new(),
        object_kind: ObjectKind::Closure,
        closure: Some(ClosureSource {
            environment: environment.into(),
            source: Arc::from(source),
        }),
        environment: None,
        embedded_closures: Vec::new(),
        embedded_environments: Vec::new(),
        issues: Vec::new(),
    }
}

#[test]
fn retaining_structured_object_does_not_execute_nested_closure() {
    let mut root = package_with!(
        "root",
        &[
            ("generator_funs", None),
            ("dead_dependency", Some("dead_dependency <- function() 1")),
        ],
        Vec::new(),
        export("generator_funs"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let binding = root.bindings.get_mut("generator_funs").unwrap();
    binding.object_kind = ObjectKind::List;
    binding.embedded_closures.push(EmbeddedClosureSource {
        path: "$[[1]]".into(),
        source: Arc::from(".slinker_embedded <- function() dead_dependency()"),
        environment: "namespace:root".into(),
    });

    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(!retained_binding(&plan, "root", "dead_dependency"));
}

#[test]
fn runtime_construction_executes_reenclosed_closures_in_derived_environment() {
    let mut root = package_with!(
        "root",
        &[
            (
                "f",
                Some(
                    "f <- function() { generator <- new.env(parent = capsule); generator$self <- generator; methods <- assign_func_envs(templates, generator); list2env2(methods, generator); generator }",
                ),
            ),
            ("capsule", None),
            ("templates", None),
        ],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    root.bindings
        .get_mut("f")
        .unwrap()
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    let capsule = root.bindings.get_mut("capsule").unwrap();
    capsule.object_kind = ObjectKind::Environment;
    capsule.environment = Some("private:1".into());
    {
        let templates = root.bindings.get_mut("templates").unwrap();
        templates.object_kind = ObjectKind::List;
        for name in ["first", "second"] {
            templates.embedded_closures.push(EmbeddedClosureSource {
                path: format!("$${name}"),
                source: Arc::from(format!(
                    ".slinker_embedded <- function() {{ self; {name}_dependency() }}"
                )),
                environment: "namespace:root".into(),
            });
        }
    }
    for name in ["first", "second"] {
        root.bindings.insert(
            format!("{name}_dependency"),
            BindingImage {
                name: format!("{name}_dependency"),
                origin: BindingOrigin::Code,
                representation: slinker::package::BindingRepresentation::Value,
                classes: Vec::new(),
                object_kind: ObjectKind::Closure,
                closure: Some(ClosureSource {
                    source: Arc::from(format!("{name}_dependency <- function() 1")),
                    environment: "namespace:root".into(),
                }),
                environment: None,
                embedded_closures: Vec::new(),
                embedded_environments: Vec::new(),
                issues: Vec::new(),
            },
        );
    }
    root.private_environments.insert(
        "private:1".into(),
        PrivateEnvironmentImage {
            id: "private:1".into(),
            parent: "namespace:root".into(),
            bindings: HashMap::from([
                (
                    "assign_func_envs".into(),
                    private_closure(
                        "assign_func_envs",
                        "private:1",
                        "assign_func_envs <- function(objs, target_env) { if (is.null(target_env)) return(objs); lapply(objs, function(x) { if (is.function(x)) environment(x) <- target_env; x }) }",
                    ),
                ),
                (
                    "list2env2".into(),
                    private_closure(
                        "list2env2",
                        "private:1",
                        "list2env2 <- function(x, envir = NULL) { if (is.null(envir)) envir <- new.env(); if (length(x) == 0L) return(NULL); list2env(x, envir) }",
                    ),
                ),
            ]),
        },
    );
    Arc::make_mut(&mut root.index).binding_names = root.bindings.keys().cloned().collect();

    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "root", "first_dependency"));
    assert!(retained_binding(&plan, "root", "second_dependency"));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding && diagnostic.message.contains("self")
    }));
}

#[test]
fn unknown_closure_enclosure_reports_root_cause_without_lexical_cascade() {
    let mut root = package("root", &[("f", Some("f <- function() self + classname"))]);
    root.bindings
        .get_mut("f")
        .unwrap()
        .closure
        .as_mut()
        .unwrap()
        .environment = "unsupported:dynamic".into();
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnknownClosureEnclosure
            && diagnostic.binding.as_deref() == Some("f")
    }));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding
            && (diagnostic.message.contains("self") || diagnostic.message.contains("classname"))
    }));
}

#[test]
fn provenance_dump_is_deterministic() {
    let root = package(
        "root",
        &[
            ("b", Some("b <- function() 1")),
            ("a", Some("a <- function() b()")),
        ],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    let first = plan.provenance().dump();
    let second = plan.provenance().dump();
    assert_eq!(first, second);
    assert!(first.contains("LexicalReference"));
}

#[test]
fn root_starts_from_exports_and_recurses_only_into_referenced_internal_bindings() {
    let root = package_with!(
        "root",
        &[
            ("public", Some("public <- function() helper()")),
            ("helper", Some("helper <- function() 1")),
            (
                "unused_optional",
                Some("unused_optional <- function() foo::bar()"),
            ),
        ],
        Vec::new(),
        export("public"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with!(
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
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 2).analyze("root").unwrap();

    assert!(retained_binding(&plan, "root", "public"));
    assert!(retained_binding(&plan, "root", "helper"));
    assert!(!retained_binding(&plan, "root", "unused_optional"));
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn closure_private_environment_is_inventory_not_a_root_set() {
    let mut root = package_with!(
        "root",
        &[("public", Some("public <- function() 1"))],
        Vec::new(),
        export("public"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    root.bindings
        .get_mut("public")
        .unwrap()
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    root.private_environments.insert(
        "private:1".into(),
        PrivateEnvironmentImage {
            id: "private:1".into(),
            parent: "namespace:root".into(),
            bindings: HashMap::from([(
                "unused".into(),
                private_closure("unused", "private:1", "unused <- function() foo::bar()"),
            )]),
        },
    );
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 2)
        .analyze("root")
        .unwrap();
    assert!(!retained_private_binding(
        &plan,
        "root",
        "private:1",
        "unused"
    ));
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
}

#[test]
fn lexical_lookup_demands_only_the_referenced_private_binding() {
    let mut root = package_with!(
        "root",
        &[("public", Some("public <- function() helper()"))],
        Vec::new(),
        export("public"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    root.bindings
        .get_mut("public")
        .unwrap()
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    root.private_environments.insert(
        "private:1".into(),
        PrivateEnvironmentImage {
            id: "private:1".into(),
            parent: "namespace:root".into(),
            bindings: HashMap::from([
                (
                    "helper".into(),
                    private_closure("helper", "private:1", "helper <- function() 1"),
                ),
                (
                    "unused".into(),
                    private_closure("unused", "private:1", "unused <- function() foo::bar()"),
                ),
            ]),
        },
    );
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 2)
        .analyze("root")
        .unwrap();
    assert!(retained_private_binding(
        &plan,
        "root",
        "private:1",
        "helper"
    ));
    assert!(!retained_private_binding(
        &plan,
        "root",
        "private:1",
        "unused"
    ));
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
}

#[test]
fn unused_private_binding_issue_does_not_block_owner_closure() {
    let mut root = package_with!(
        "root",
        &[("public", Some("public <- function() 1"))],
        Vec::new(),
        export("public"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    root.bindings
        .get_mut("public")
        .unwrap()
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    root.private_environments.insert(
        "private:1".into(),
        PrivateEnvironmentImage {
            id: "private:1".into(),
            parent: "namespace:root".into(),
            bindings: HashMap::from([(
                "bad".into(),
                PrivateBindingImage {
                    name: "bad".into(),
                    representation: slinker::package::BindingRepresentation::Value,
                    classes: Vec::new(),
                    object_kind: ObjectKind::Other("externalptr".into()),
                    closure: None,
                    environment: None,
                    embedded_closures: Vec::new(),
                    embedded_environments: Vec::new(),
                    issues: vec![ObjectIssue {
                        path: "$".into(),
                        kind: "external_pointer".into(),
                        detail: "external pointer".into(),
                    }],
                },
            )]),
        },
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnsupportedObject)
    );
}

#[test]
fn onload_can_create_a_missing_exported_active_binding() {
    let mut root = package_with!(
        "root",
        &[
            ("dummy", Some("dummy <- function() NULL")),
            ("get_pb", Some("get_pb <- function() 1")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(lib, pkg) { pkgenv <- environment(dummy); makeActiveBinding(\"pb\", get_pb, pkgenv) }",
                ),
            ),
        ],
        Vec::new(),
        export("pb"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    Arc::make_mut(&mut root.index).lifecycle.on_load = true;
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding
            && diagnostic.binding.as_deref() == Some("pb")
    }));
    assert!(retained_binding(&plan, "root", "get_pb"));
}

#[test]
fn dependency_onload_can_create_a_missing_exported_active_binding() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() foo::pb"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Imports: foo\n",
    );
    let mut foo = package_with!(
        "foo",
        &[
            ("dummy", Some("dummy <- function() NULL")),
            ("get_pb", Some("get_pb <- function() 1")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(lib, pkg) { pkgenv <- environment(dummy); makeActiveBinding(\"pb\", get_pb, pkgenv) }",
                ),
            ),
        ],
        Vec::new(),
        export("pb"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    Arc::make_mut(&mut foo.index).lifecycle.on_load = true;
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding
            && diagnostic.package == "foo"
            && diagnostic.binding.as_deref() == Some("pb")
    }));
    assert!(retained_binding(&plan, "foo", "get_pb"));
}

#[test]
fn runtime_make_active_binding_does_not_satisfy_missing_export() {
    let root = package_with!(
        "root",
        &[(
            "f",
            Some("f <- function() makeActiveBinding(\"pb\", function() 1, asNamespace(\"root\"))"),
        )],
        Vec::new(),
        ExportMap::from([("f".into(), "f".into()), ("pb".into(), "pb".into())]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding
            && diagnostic.binding.as_deref() == Some("pb")
    }));
}

#[test]
fn root_lifecycle_remains_an_entrypoint_even_when_not_exported() {
    let mut root = package_with!(
        "root",
        &[
            ("public", Some("public <- function() 1")),
            (
                ".onLoad",
                Some(".onLoad <- function(...) initialize_state()"),
            ),
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
    Arc::make_mut(&mut root.index).lifecycle.on_load = true;
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "root", ".onLoad"));
    assert!(retained_binding(&plan, "root", "initialize_state"));
    assert!(!retained_binding(&plan, "root", "unused"));
}

#[test]
fn root_reexported_import_is_demanded_without_local_binding() {
    let root = package_with!(
        "root",
        &[],
        vec![ImportSpec::From {
            package: "utils".into(),
            bindings: vec![ImportBinding {
                local: "head".into(),
                remote: "head".into(),
            }],
        }],
        export("head"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Imports: utils\n",
    );
    let utils = package_with!(
        "utils",
        &[
            ("head", Some("head <- function(x) x")),
            ("unused", Some("unused <- function() 1")),
        ],
        Vec::new(),
        export("head"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, utils]), 1)
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "root", "head"));
    assert!(retained_binding(&plan, "utils", "head"));
    assert!(!retained_binding(&plan, "utils", "unused"));
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnresolvedBinding)
    );
}

#[test]
fn qualified_access_loads_only_demanded_foreign_binding() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with!(
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
    let foo = package_with!(
        "foo",
        &[("bar", Some("bar <- function() baz::qux()"))],
        Vec::new(),
        export("bar"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let baz = package_with!(
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
fn unused_root_import_metadata_does_not_create_reachability() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() 1"))],
        vec![ImportSpec::All {
            package: "foo".into(),
            except: Vec::new(),
        }],
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package_with!(
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
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert!(!retained_binding(&plan, "foo", "a"));
    assert!(!retained_binding(&plan, "foo", "b"));
}

#[test]
fn renamed_import_from_resolves_remote_binding() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() local_x()"))],
        vec![ImportSpec::From {
            package: "foo".into(),
            bindings: vec![ImportBinding {
                local: "local_x".into(),
                remote: "x".into(),
            }],
        }],
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package_with!(
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
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() x()"))],
        vec![ImportSpec::All {
            package: "foo".into(),
            except: Vec::new(),
        }],
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package_with!(
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
fn depends_metadata_alone_does_not_create_reachability() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() 1"))],
        Vec::new(),
        export("f"),
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
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert!(!retained_binding(&plan, "foo", "x"));
}

#[test]
fn exact_external_package_terminates_internal_traversal() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with!(
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
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1)
        .with_external_packages(["foo".into()])
        .analyze("root")
        .unwrap();
    assert!(
        plan.provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo"
                && matches!(&node.kind, NodeKind::ExternalBinding { name } if name == "bar"))
    );
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
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn namespace_discovery_matches_named_and_mixed_positional_arguments() {
    for source in [
        "f <- function() requireNamespace(quietly = TRUE, package = \"root\")",
        "f <- function() requireNamespace(quietly = TRUE, \"root\")",
    ] {
        let root = package("root", &[("f", Some(source))]);
        let plan = Linker::new(FakeProvider::new(vec![root]), 1)
            .analyze("root")
            .unwrap();
        assert!(
            !plan
                .blockers()
                .iter()
                .any(|diagnostic| { diagnostic.code == RejectCode::DynamicPackageDiscovery }),
            "static package argument was lost for {source}"
        );
    }
}

#[test]
fn constant_argument_specializes_private_namespace_helper() {
    let root = package_with!(
        "root",
        &[
            ("f", Some("f <- function() helper(\"foo\")")),
            (
                "helper",
                Some("helper <- function(package) requireNamespace(package)"),
            ),
        ],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package("foo", &[]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_external_packages(["foo".into()])
        .with_policy(LinkPolicy {
            namespace_discovery: DiscoveryPolicy::ExternalOnly,
        })
        .analyze("root")
        .unwrap();

    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery),
        "{:?}",
        plan.blockers()
    );
    assert!(
        plan.program()
            .packages()
            .any(|(_, package)| package.identity().name == "foo"
                && package.role() == slinker::ir::PackageRole::External)
    );
}

#[test]
fn unknown_argument_keeps_public_namespace_helper_dynamic() {
    let root = package(
        "root",
        &[(
            "helper",
            Some("helper <- function(package) requireNamespace(package)"),
        )],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
}

#[test]
fn bounded_string_operations_specialize_namespace_helper() {
    let root = package_with!(
        "root",
        &[
            ("f", Some("f <- function() helper(\"foo-extra\")")),
            (
                "helper",
                Some(
                    "helper <- function(spec) { parts <- strsplit(spec, \"-\", fixed = TRUE)[[1L]]; package <- paste0(parts[[1L]], \"\"); requireNamespace(package) }",
                ),
            ),
        ],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package("foo", &[]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_external_packages(["foo".into()])
        .with_policy(LinkPolicy {
            namespace_discovery: DiscoveryPolicy::ExternalOnly,
        })
        .analyze("root")
        .unwrap();

    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
}

#[test]
fn unknown_string_index_keeps_namespace_discovery_dynamic() {
    let root = package_with!(
        "root",
        &[
            (
                "f",
                Some("f <- function(index) helper(\"foo-extra\", index)"),
            ),
            (
                "helper",
                Some(
                    "helper <- function(spec, index) { parts <- strsplit(spec, \"-\", fixed = TRUE)[[1L]]; requireNamespace(parts[[index]]) }",
                ),
            ),
        ],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
}

#[test]
fn resolved_null_coalescing_helper_propagates_constant() {
    let root = package_with!(
        "root",
        &[
            ("f", Some("f <- function() helper(NULL)")),
            (
                "helper",
                Some("helper <- function(package) requireNamespace(package %||% \"foo\")"),
            ),
            (
                "%||%",
                Some("`%||%` <- function(left, right) if (!is.null(left)) left else right"),
            ),
        ],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package("foo", &[]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_external_packages(["foo".into()])
        .with_policy(LinkPolicy {
            namespace_discovery: DiscoveryPolicy::ExternalOnly,
        })
        .analyze("root")
        .unwrap();

    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
}

#[test]
fn bounded_switch_propagates_selected_package() {
    let root = package_with!(
        "root",
        &[
            ("f", Some("f <- function() helper(\"short\")")),
            (
                "helper",
                Some(
                    "helper <- function(kind) requireNamespace(switch(kind, short = \"foo\", long = \"bar\"))",
                ),
            ),
        ],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let foo = package("foo", &[]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_external_packages(["foo".into()])
        .with_policy(LinkPolicy {
            namespace_discovery: DiscoveryPolicy::ExternalOnly,
        })
        .analyze("root")
        .unwrap();

    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
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
        plan.provenance()
            .nodes()
            .iter()
            .any(|node| { node.package == "foo" && matches!(node.kind, NodeKind::Activation) })
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
            plan.blockers()
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
            .blockers()
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
    assert!(!plan.blockers().iter().any(|diagnostic| matches!(
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
        plan.provenance()
            .nodes()
            .iter()
            .filter(|node| node.package == "root"
                && matches!(&node.kind, NodeKind::Binding { name } if name == "a"))
            .count(),
        1
    );
    assert_eq!(
        plan.provenance()
            .nodes()
            .iter()
            .filter(|node| node.package == "root"
                && matches!(&node.kind, NodeKind::Binding { name } if name == "b"))
            .count(),
        1
    );
}

#[test]
fn registered_native_symbol_is_not_an_unresolved_r_binding() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function(x) .Call(croot_f, x)"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        vec![NativeComponent {
            name: "root".into(),
            registration: Some(NativeRegistration {
                prefix: "c".into(),
                suffix: "".into(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "croot_f".into(),
                symbol: "root_f".into(),
            }],
            safety: NativeSafety::Safe(NativeFacts {
                callbacks: Vec::new(),
            }),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding && diagnostic.message.contains("croot_f")
    }));
    assert!(plan.provenance().nodes().iter().any(|node| {
        node.package == "root"
            && matches!(&node.kind, NodeKind::NativeComponent { name } if name == "root")
    }));
}

fn opaque_registered_component() -> NativeComponent {
    NativeComponent {
        name: "root".into(),
        registration: Some(NativeRegistration {
            prefix: "".into(),
            suffix: "".into(),
        }),
        symbols: Vec::new(),
        safety: NativeSafety::Unanalyzed,
    }
}

#[test]
fn opaque_registered_selector_is_consumed_by_native_call() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function(x) .Call(croot_f, x)"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        vec![opaque_registered_component()],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(plan.provenance().nodes().iter().any(|node| {
        node.package == "root"
            && matches!(&node.kind, NodeKind::NativeComponent { name } if name == "root")
    }));
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeEffects)
    );
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnknownNativeLookup
            || (diagnostic.code == RejectCode::UnresolvedBinding
                && diagnostic.message.contains("croot_f"))
    }));
}

#[test]
fn opaque_native_selector_consumption_is_occurrence_specific() {
    let root = package_with!(
        "root",
        &[(
            "f",
            Some("f <- function(x) { identity(croot_f); .Call(croot_f, x) }"),
        )],
        Vec::new(),
        export("f"),
        Vec::new(),
        vec![opaque_registered_component()],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert_eq!(
        plan.blockers()
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == RejectCode::UnresolvedBinding
                    && diagnostic.message.contains("croot_f")
            })
            .count(),
        1
    );
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn ordinary_r_binding_beats_opaque_native_selector_fallback() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function(x) .Call(foo, x)")), ("foo", None)],
        Vec::new(),
        export("f"),
        Vec::new(),
        vec![opaque_registered_component()],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "root", "foo"));
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn shadowed_native_primitive_does_not_consume_selector() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function(.Call, x) .Call(croot_f, x)"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        vec![opaque_registered_component()],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    let function = plan
        .provenance()
        .binding("root", "f")
        .expect("function binding");
    assert!(!plan.provenance().edges().iter().any(|edge| {
        edge.from == function
            && edge.kind == EdgeKind::Native
            && matches!(
                plan.provenance().nodes()[edge.to.0].kind,
                NodeKind::NativeComponent { .. }
            )
    }));
    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding && diagnostic.message.contains("croot_f")
    }));
}

#[test]
fn named_opaque_native_selector_is_matched_by_formal_name() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function(x) .Call(x, .NAME = croot_f)"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        vec![opaque_registered_component()],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(
        plan.provenance()
            .nodes()
            .iter()
            .any(|node| matches!(node.kind, NodeKind::NativeComponent { .. }))
    );
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnknownNativeLookup
            || (diagnostic.code == RejectCode::UnresolvedBinding
                && diagnostic.message.contains("croot_f"))
    }));
}

#[test]
fn string_native_selector_matches_routine_symbol_not_r_binding() {
    let component = NativeComponent {
        name: "root".into(),
        registration: Some(NativeRegistration {
            prefix: "c".into(),
            suffix: "".into(),
        }),
        symbols: vec![NativeSymbolBinding {
            binding: "croot_f".into(),
            symbol: "root_f".into(),
        }],
        safety: NativeSafety::Safe(NativeFacts {
            callbacks: Vec::new(),
        }),
    };
    let root = package_with!(
        "root",
        &[
            (
                "by_symbol",
                Some("by_symbol <- function(x) .Call(\"root_f\", x)"),
            ),
            (
                "by_binding",
                Some("by_binding <- function(x) .Call(\"croot_f\", x)"),
            ),
        ],
        Vec::new(),
        export("by_symbol"),
        Vec::new(),
        vec![component],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );

    let root = package_with!(
        "root",
        &[(
            "by_binding",
            Some("by_binding <- function(x) .Call(\"croot_f\", x)"),
        )],
        Vec::new(),
        export("by_binding"),
        Vec::new(),
        vec![NativeComponent {
            name: "root".into(),
            registration: None,
            symbols: vec![NativeSymbolBinding {
                binding: "croot_f".into(),
                symbol: "root_f".into(),
            }],
            safety: NativeSafety::Safe(NativeFacts {
                callbacks: Vec::new(),
            }),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn registered_native_symbol_can_be_assigned_into_namespace_state() {
    let root = package_with!(
        "root",
        &[
            ("slot", None),
            (
                ".onLoad",
                Some(".onLoad <- function(lib, pkg) slot <<- croot_tick"),
            ),
        ],
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        vec![NativeComponent {
            name: "root".into(),
            registration: Some(NativeRegistration {
                prefix: "c".into(),
                suffix: "".into(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "croot_tick".into(),
                symbol: "root_tick".into(),
            }],
            safety: NativeSafety::Safe(NativeFacts {
                callbacks: Vec::new(),
            }),
        }],
        Vec::new(),
        "",
    );
    let mut root = root;
    Arc::make_mut(&mut root.index).lifecycle.on_load = true;
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding
            && diagnostic.message.contains("croot_tick")
    }));
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::EnvironmentMutation)
    );
}

#[test]
fn opaque_registered_native_rhs_in_onload_is_not_misreported_as_r_binding() {
    let mut root = package_with!(
        "root",
        &[
            ("slot", None),
            (
                ".onLoad",
                Some(".onLoad <- function(lib, pkg) slot <<- croot_tick"),
            ),
        ],
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        vec![NativeComponent {
            name: "root".into(),
            registration: Some(NativeRegistration {
                prefix: "".into(),
                suffix: "".into(),
            }),
            symbols: Vec::new(),
            safety: NativeSafety::Unanalyzed,
        }],
        Vec::new(),
        "",
    );
    Arc::make_mut(&mut root.index).lifecycle.on_load = true;
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding
            && diagnostic.message.contains("croot_tick")
    }));
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeEffects)
    );
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn native_activation_keeps_component_without_widening_r_bindings() {
    let root = package("root", &[("f", Some("f <- function() foo::a()"))]);
    let foo = package_with!(
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
            registration: None,
            symbols: Vec::new(),
            safety: NativeSafety::Safe(NativeFacts {
                callbacks: Vec::new(),
            }),
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
    assert!(plan.provenance().nodes().iter().any(|node| {
        node.package == "foo"
            && matches!(&node.kind, NodeKind::NativeComponent { name } if name == "foo")
    }));
}

#[test]
fn known_native_callback_adds_binding_edge() {
    let root = package("root", &[("f", Some("f <- function() foo::a()"))]);
    let foo = package_with!(
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
            registration: None,
            symbols: Vec::new(),
            safety: NativeSafety::Safe(NativeFacts {
                callbacks: vec!["callback".into()],
            }),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "callback"));
    assert!(
        plan.provenance()
            .edges()
            .iter()
            .any(|edge| edge.kind == EdgeKind::Callback)
    );
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn native_callback_argument_summary_adds_a_targeted_call_site_edge() {
    let root = package_with!(
        "root",
        &[
            ("a", Some("a <- function() .Call(root_a, 1, callback)")),
            ("callback", Some("callback <- function(x) x")),
            ("unrelated", Some("unrelated <- function() 3")),
        ],
        Vec::new(),
        export("a"),
        Vec::new(),
        vec![NativeComponent {
            name: "root".into(),
            registration: Some(NativeRegistration {
                prefix: "".into(),
                suffix: "".into(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "root_a".into(),
                symbol: "root_a".into(),
            }],
            safety: NativeSafety::Summarized(vec![NativeRoutineSummary {
                selector: "root_a".into(),
                callback_arguments: vec![2],
            }]),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "root", "callback"));
    assert!(!retained_binding(&plan, "root", "unrelated"));
    let native = plan
        .provenance()
        .nodes()
        .iter()
        .find(|node| {
            node.package == "root"
                && matches!(&node.kind, NodeKind::NativeComponent { name } if name == "root")
        })
        .unwrap()
        .id;
    let callback = plan.provenance().binding("root", "callback").unwrap();
    assert!(plan.provenance().edges().iter().any(|edge| {
        edge.from == native && edge.to == callback && edge.kind == EdgeKind::Callback
    }));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        matches!(
            diagnostic.code,
            RejectCode::UnknownNativeLookup | RejectCode::UnknownNativeEffects
        )
    }));
}

#[test]
fn native_summary_accepts_oak_proven_local_closure_callback() {
    let root = package_with!(
        "root",
        &[(
            "a",
            Some("a <- function() { callback <- function(x) x; .Call(root_a, callback) }"),
        )],
        Vec::new(),
        export("a"),
        Vec::new(),
        vec![NativeComponent {
            name: "root".into(),
            registration: Some(NativeRegistration {
                prefix: "".into(),
                suffix: "".into(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "root_a".into(),
                symbol: "root_a".into(),
            }],
            safety: NativeSafety::Summarized(vec![NativeRoutineSummary {
                selector: "root_a".into(),
                callback_arguments: vec![1],
            }]),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeEffects)
    );
    let native = plan
        .provenance()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::NativeComponent { .. }))
        .expect("native component")
        .id;
    let owner = plan
        .provenance()
        .binding("root", "a")
        .expect("owner binding");
    assert!(plan.provenance().edges().iter().any(|edge| {
        edge.from == native && edge.to == owner && edge.kind == EdgeKind::Callback
    }));
}

#[test]
fn native_callback_positions_ignore_named_package_and_match_named_selector() {
    let root = package_with!(
        "root",
        &[
            (
                "a",
                Some("a <- function() .Call(PACKAGE = \"root\", .NAME = root_a, 1, callback)"),
            ),
            ("callback", Some("callback <- function(x) x")),
        ],
        Vec::new(),
        export("a"),
        Vec::new(),
        vec![NativeComponent {
            name: "root".into(),
            registration: Some(NativeRegistration {
                prefix: "".into(),
                suffix: "".into(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "root_a".into(),
                symbol: "root_a".into(),
            }],
            safety: NativeSafety::Summarized(vec![NativeRoutineSummary {
                selector: "root_a".into(),
                callback_arguments: vec![2],
            }]),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "root", "callback"));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        matches!(
            diagnostic.code,
            RejectCode::UnknownNativeLookup | RejectCode::UnknownNativeEffects
        )
    }));
}

#[test]
fn summarized_native_callbacks_are_not_global_component_roots() {
    let root = package_with!(
        "root",
        &[
            ("a", Some("a <- function() 1")),
            ("callback", Some("callback <- function(x) x")),
        ],
        Vec::new(),
        export("a"),
        Vec::new(),
        vec![NativeComponent {
            name: "root".into(),
            registration: Some(NativeRegistration {
                prefix: "".into(),
                suffix: "".into(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "root_a".into(),
                symbol: "root_a".into(),
            }],
            safety: NativeSafety::Summarized(vec![NativeRoutineSummary {
                selector: "root_a".into(),
                callback_arguments: vec![2],
            }]),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(!retained_binding(&plan, "root", "callback"));
}

#[test]
fn missing_native_routine_summary_is_an_effect_blocker_not_lookup_failure() {
    let root = package_with!(
        "root",
        &[("a", Some("a <- function() .Call(root_a, 1)"))],
        Vec::new(),
        export("a"),
        Vec::new(),
        vec![NativeComponent {
            name: "root".into(),
            registration: Some(NativeRegistration {
                prefix: "".into(),
                suffix: "".into(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "root_a".into(),
                symbol: "root_a".into(),
            }],
            safety: NativeSafety::Summarized(Vec::new()),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeEffects)
    );
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn unsupported_native_lookup_rejects_without_widening_r_namespace() {
    let root = package("root", &[("f", Some("f <- function() foo::a()"))]);
    let foo = package_with!(
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
            registration: None,
            symbols: Vec::new(),
            safety: NativeSafety::Unsupported(vec!["dynamic R lookup".into()]),
        }],
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(!retained_binding(&plan, "foo", "callback"));
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeEffects)
    );
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn dependency_activation_does_not_root_unreachable_s3_methods() {
    let root = package("root", &[("f", Some("f <- function() foo::x()"))]);
    let foo = package_with!(
        "foo",
        &[
            ("x", Some("x <- function() 1")),
            ("print.foo", Some("print.foo <- function(x, ...) x")),
        ],
        Vec::new(),
        export("x"),
        vec![S3Registration {
            generic: slinker::package::GenericSpec {
                package: None,
                name: "print".into(),
            },
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
    assert!(retained_binding(&plan, "foo", "x"));
    assert!(!retained_binding(&plan, "foo", "print.foo"));
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
    let foo = package_with!(
        "foo",
        &[],
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        vec!["data/x.json".into(), "data/y.json".into()],
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(plan.program().resources().iter().any(|resource| {
        plan.program().package(resource.package).identity().name == "foo"
            && resource.path == "data/x.json"
    }));
    assert!(
        !plan
            .program()
            .resources()
            .iter()
            .any(|resource| resource.path == "data/y.json")
    );
}

#[test]
fn dynamic_resource_package_blocks_only_when_an_installation_is_removed() {
    let dynamic = "f <- function(package = 'root') system.file('data', package = package)";
    let standalone = Linker::new(
        FakeProvider::new(vec![package("root", &[("f", Some(dynamic))])]),
        1,
    )
    .analyze("root")
    .unwrap();
    assert!(standalone.blockers().is_empty());

    let root = package(
        "root",
        &[
            ("f", Some(dynamic)),
            ("g", Some("g <- function() foo::h()")),
        ],
    );
    let foo = package("foo", &[("h", Some("h <- function() 1"))]);
    let linked = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(linked.blockers().iter().any(|diagnostic| {
        diagnostic.binding.is_none()
            && diagnostic.code == RejectCode::DynamicLookup
            && diagnostic.message.contains("system.file")
    }));
}

#[test]
fn base_resource_lookup_is_not_a_package_resource() {
    let root = package(
        "root",
        &[("f", Some("f <- function() system.file('DESCRIPTION')"))],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(plan.blockers().is_empty());
    assert!(plan.program().resources().is_empty());
}

#[test]
fn non_closure_binding_never_invokes_air() {
    let root = package("root", &[("constant", None)]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 4)
        .analyze("root")
        .unwrap();
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
    Linker::new(provider, 2).analyze("root").unwrap();
    assert_eq!(counts.lock().unwrap().get("root").copied().unwrap_or(0), 1);
}

#[test]
fn air_frontend_failure_is_localized_not_package_fatal() {
    // Deliberately make Air reject this binding while the fake target-R
    // validator reports acceptance. This isolates disagreement handling from
    // any particular real-R grammar edge case.
    let root = package(
        "root",
        &[
            ("good", Some("good <- function() 1")),
            ("awkward", Some("awkward <- function() {")),
        ],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 2)
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "root", "good"));
    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("awkward")
            && diagnostic.code == RejectCode::AirUnsupportedSyntax
    }));
}

#[test]
fn air_accepted_unknown_name_is_a_semantic_error_not_a_frontend_error() {
    let root = package("root", &[("f", Some("f <- function() missing_symbol()"))]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::UnresolvedBinding
    }));
    assert!(!plan.blockers().iter().any(|diagnostic| {
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

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("awkward")
            && diagnostic.code == RejectCode::InvalidInstalledRepresentation
    }));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("awkward")
            && diagnostic.code == RejectCode::AirUnsupportedSyntax
    }));
}

#[test]
fn every_non_root_node_has_an_incoming_reason_and_why_path() {
    let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
    let foo = package_with!(
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
    let bar = plan.provenance().binding("foo", "bar").unwrap();
    assert!(plan.provenance().incoming(bar).next().is_some());
    assert!(
        plan.provenance()
            .shortest_path(plan.provenance().roots(), bar)
            .is_some()
    );
}

#[test]
fn missing_packages_are_collated_instead_of_failing_fast() {
    let root = package(
        "root",
        &[
            ("a", Some("a <- function() foo::x()")),
            ("b", Some("b <- function() bar::y()")),
        ],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 2)
        .analyze("root")
        .unwrap();
    let mut missing = plan
        .provenance()
        .missing_packages()
        .map(|id| plan.provenance().nodes()[id.0].package.clone())
        .collect::<Vec<_>>();
    missing.sort();
    assert_eq!(missing, vec!["bar", "foo"]);
    assert_eq!(
        plan.blockers()
            .iter()
            .filter(|d| d.code == RejectCode::MissingDependency)
            .count(),
        2
    );
}

#[test]
fn unused_dependency_import_does_not_pull_or_report_missing_package() {
    let root = package("root", &[("f", Some("f <- function() foo::x()"))]);
    let foo = package_with!(
        "foo",
        &[("x", Some("x <- function() 1"))],
        vec![ImportSpec::From {
            package: "otelsdk".into(),
            bindings: vec![ImportBinding {
                local: "span".into(),
                remote: "span".into(),
            }],
        }],
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
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "otelsdk")
    );
}

#[test]
fn reachable_dependency_import_reports_missing_package_with_provenance() {
    let root = package("root", &[("f", Some("f <- function() foo::x()"))]);
    let foo = package_with!(
        "foo",
        &[("x", Some("x <- function() span()"))],
        vec![ImportSpec::From {
            package: "otelsdk".into(),
            bindings: vec![ImportBinding {
                local: "span".into(),
                remote: "span".into(),
            }],
        }],
        export("x"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    let missing = plan
        .provenance()
        .nodes()
        .iter()
        .find(|node| node.package == "otelsdk" && matches!(&node.kind, NodeKind::MissingPackage))
        .unwrap();
    let path = plan
        .provenance()
        .shortest_path(plan.provenance().roots(), missing.id)
        .unwrap();
    assert!(path.iter().any(|edge| edge.kind == EdgeKind::Import));
    assert!(path.iter().any(|edge| edge.reason.contains("span")));
}

#[test]
fn missing_unused_root_import_is_not_reported() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() 1"))],
        vec![ImportSpec::All {
            package: "required_at_root_load".into(),
            except: Vec::new(),
        }],
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "required_at_root_load")
    );
}

#[test]
fn absent_optional_resource_is_not_a_blocker() {
    let root = package(
        "root",
        &[(
            "f",
            Some("f <- function() system.file(\"missing\", package = \"foo\")"),
        )],
    );
    let foo = package_with!(
        "foo",
        &[],
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::MissingResource)
    );
}

#[test]
fn absent_must_work_resource_is_a_precise_blocker() {
    let root = package(
        "root",
        &[(
            "f",
            Some("f <- function() system.file(\"missing\", package = \"foo\", mustWork = TRUE)"),
        )],
    );
    let foo = package_with!(
        "foo",
        &[],
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::MissingResource)
    );
}

#[test]
fn suggests_alone_never_enters_the_graph() {
    let root = package_with!(
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
    let plan = Linker::new(provider, 1).analyze("root").unwrap();
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn selecting_extra_does_not_root_an_unused_optional_package() {
    let root = package_with!(
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

    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn effective_namespace_import_is_required_even_if_description_also_suggests_it() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() imported_bar()"))],
        vec![ImportSpec::From {
            package: "foo".into(),
            bindings: vec![ImportBinding {
                local: "imported_bar".into(),
                remote: "bar".into(),
            }],
        }],
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with!(
        "foo",
        &[
            ("bar", Some("bar <- function() 1")),
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
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn config_needs_does_not_enable_a_suggested_runtime_package() {
    let root = package_with!(
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

    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
    assert_eq!(
        locate_counts
            .lock()
            .unwrap()
            .get("foo")
            .copied()
            .unwrap_or(0),
        0
    );
}

#[test]
fn required_description_relationship_wins_over_duplicate_suggests_when_source_uses_package() {
    for required_field in ["Imports", "Depends"] {
        let root = package_with!(
            "root",
            &[("f", Some("f <- function() foo::bar()"))],
            Vec::new(),
            export("f"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            &format!("{required_field}: foo\nSuggests: foo\n"),
        );
        let foo = package_with!(
            "foo",
            &[
                ("bar", Some("bar <- function() 1")),
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
            .analyze("root")
            .unwrap();

        assert!(
            retained_binding(&plan, "foo", "bar"),
            "{required_field} should make foo required when source uses it"
        );
        assert!(!retained_binding(&plan, "foo", "unused"));
    }
}

#[test]
fn direct_suggested_namespace_access_is_ignored_without_extra_pkgs() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() foo::bar()"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with!(
        "foo",
        &[
            ("bar", Some("bar <- function() helper()")),
            ("helper", Some("helper <- function() 1")),
        ],
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

    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert!(
        !plan
            .program()
            .relocations()
            .iter()
            .any(|relocation| matches!(relocation, slinker::ir::Relocation::Binding { .. }))
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
    assert_eq!(
        locate_counts
            .lock()
            .unwrap()
            .get("foo")
            .copied()
            .unwrap_or(0),
        0
    );
}

#[test]
fn direct_suggested_namespace_access_is_linked_when_selected() {
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() foo::bar()"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with!(
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
    let root = package_with!(
        "root",
        &[("f", Some("f <- function() foo::bar()"))],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with!(
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
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "baz")
    );
    assert_eq!(counts.lock().unwrap().get("baz").copied().unwrap_or(0), 0);
    assert_eq!(
        locate_counts
            .lock()
            .unwrap()
            .get("baz")
            .copied()
            .unwrap_or(0),
        0
    );
}

#[test]
fn cli_like_unreachable_optional_helpers_do_not_expand_suggests() {
    let root = package_with!(
        "cli",
        &[
            ("cli_alert", Some("cli_alert <- function() format_alert()")),
            (
                "format_alert",
                Some("format_alert <- function() paste('ok')"),
            ),
            (
                "knitr_helper",
                Some("knitr_helper <- function() knitr::knit()"),
            ),
            (
                "testthat_helper",
                Some("testthat_helper <- function() testthat::test_that('x', function() 1)"),
            ),
            (
                "rmarkdown_helper",
                Some("rmarkdown_helper <- function() rmarkdown::render('x.Rmd')"),
            ),
        ],
        Vec::new(),
        export("cli_alert"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests:\n    knitr,\n    testthat,\n    rmarkdown\n",
    );
    let knitr = package("knitr", &[("knit", Some("knit <- function() 1"))]);
    let testthat = package(
        "testthat",
        &[("test_that", Some("test_that <- function(...) 1"))],
    );
    let rmarkdown = package(
        "rmarkdown",
        &[("render", Some("render <- function(...) 1"))],
    );
    let provider = FakeProvider::new(vec![root, knitr, testthat, rmarkdown]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 4).analyze("cli").unwrap();

    assert!(retained_binding(&plan, "cli", "cli_alert"));
    assert!(retained_binding(&plan, "cli", "format_alert"));
    assert!(!retained_binding(&plan, "cli", "knitr_helper"));
    assert!(!retained_binding(&plan, "cli", "testthat_helper"));
    assert!(!retained_binding(&plan, "cli", "rmarkdown_helper"));
    for optional in ["knitr", "testthat", "rmarkdown"] {
        assert!(
            !plan
                .provenance()
                .nodes()
                .iter()
                .any(|node| node.package == optional)
        );
        assert_eq!(
            counts.lock().unwrap().get(optional).copied().unwrap_or(0),
            0
        );
    }
}

#[test]
fn cli_like_required_import_is_demanded_while_suggests_stay_out() {
    let root = package_with!(
        "cli",
        &[
            ("cli_head", Some("cli_head <- function(x) head(x)")),
            (
                "knitr_helper",
                Some("knitr_helper <- function() knitr::knit()"),
            ),
            (
                "rlang_helper",
                Some("rlang_helper <- function() rlang::env()"),
            ),
            (
                "testthat_helper",
                Some("testthat_helper <- function() testthat::test_that('x', function() 1)"),
            ),
        ],
        vec![ImportSpec::From {
            package: "utils".into(),
            bindings: vec![ImportBinding {
                local: "head".into(),
                remote: "head".into(),
            }],
        }],
        export("cli_head"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Imports:\n    utils\nSuggests:\n    knitr,\n    rlang,\n    testthat\n",
    );
    let utils = package_with!(
        "utils",
        &[
            ("head", Some("head <- function(x) x")),
            ("unused", Some("unused <- function() 1")),
        ],
        Vec::new(),
        export("head"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let knitr = package("knitr", &[("knit", Some("knit <- function() 1"))]);
    let rlang = package("rlang", &[("env", Some("env <- function() 1"))]);
    let testthat = package(
        "testthat",
        &[("test_that", Some("test_that <- function(...) 1"))],
    );
    let provider = FakeProvider::new(vec![root, utils, knitr, rlang, testthat]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 4).analyze("cli").unwrap();

    assert!(retained_binding(&plan, "utils", "head"));
    assert!(!retained_binding(&plan, "utils", "unused"));
    for optional in ["knitr", "rlang", "testthat"] {
        assert!(
            !plan
                .provenance()
                .nodes()
                .iter()
                .any(|node| node.package == optional)
        );
        assert_eq!(
            counts.lock().unwrap().get(optional).copied().unwrap_or(0),
            0
        );
    }
}

#[test]
fn unselected_suggested_resource_does_not_discover_package() {
    let root = package_with!(
        "root",
        &[(
            "f",
            Some("f <- function() system.file('data', 'x.json', package = 'foo')"),
        )],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with!(
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

    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert!(
        !plan
            .program()
            .relocations()
            .iter()
            .any(|relocation| matches!(relocation, slinker::ir::Relocation::Resource { .. }))
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn unselected_suggested_attachment_call_is_ignored() {
    let root = package_with!(
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
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();

    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::PackageAttachmentUnsupported)
    );
}

#[test]
fn root_s3_registration_is_available_without_rooting_unknown_dispatch_method() {
    let root = package_with!(
        "root",
        &[
            ("foo", Some("foo <- function(x) UseMethod(\"foo\")")),
            ("foo.bar", Some("foo.bar <- function(x) 1")),
        ],
        Vec::new(),
        export("foo"),
        vec![S3Registration {
            generic: slinker::package::GenericSpec {
                package: None,
                name: "foo".into(),
            },
            class: "bar".into(),
            method: "foo.bar".into(),
        }],
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(!retained_binding(&plan, "root", "foo.bar"));
    assert!(program_has_s3_registration(
        &plan, "root", None, "foo", "bar", "foo.bar"
    ));
    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::ObjectSystem && diagnostic.message.contains("UseMethod")
    }));
}

#[test]
fn reachable_operator_dispatch_blocks_while_registration_stays_namespace_state() {
    let mut root = package_with!(
        "root",
        &[
            ("f", Some("f <- function(other) criterion | other")),
            ("criterion", None),
            (
                "|.root_criterion",
                Some("`|.root_criterion` <- function(e1, e2) e1"),
            ),
        ],
        Vec::new(),
        export("f"),
        vec![S3Registration {
            generic: slinker::package::GenericSpec {
                package: None,
                name: "|".into(),
            },
            class: "root_criterion".into(),
            method: "|.root_criterion".into(),
        }],
        Vec::new(),
        Vec::new(),
        "",
    );
    root.bindings
        .get_mut("criterion")
        .expect("criterion binding")
        .classes = vec!["root_criterion".into()];

    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(!retained_binding(&plan, "root", "|.root_criterion"));
    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::ObjectSystem
            && diagnostic.message.contains("outside PureRStatic")
    }));
    let registration = plan
        .program()
        .s3_registrations()
        .iter()
        .find(|registration| registration.class == "root_criterion")
        .expect("final namespace registration");
    assert_eq!(
        plan.program().binding(registration.method).name,
        "|.root_criterion"
    );
}

#[test]
fn unselected_suggested_s3_generic_does_not_retain_optional_registration_method() {
    let root = package_with!(
        "root",
        &[
            ("public", Some("public <- function() 1")),
            ("print.foo", Some("print.foo <- function(x, ...) x")),
        ],
        Vec::new(),
        export("public"),
        vec![S3Registration {
            generic: slinker::package::GenericSpec {
                package: Some("foo".into()),
                name: "print".into(),
            },
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
    assert!(!program_has_s3_registration(
        &plan,
        "root",
        Some("foo"),
        "print",
        "foo",
        "print.foo"
    ));
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn retained_dependency_method_does_not_pull_unselected_suggested_generic() {
    let root = package("root", &[("f", Some("f <- function() dep::method()"))]);
    let dep = package_with!(
        "dep",
        &[("method", Some("method <- function(x = NULL, ...) x"))],
        Vec::new(),
        export("method"),
        vec![S3Registration {
            generic: slinker::package::GenericSpec {
                package: Some("foo".into()),
                name: "generic".into(),
            },
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
    assert!(!program_has_s3_registration(
        &plan,
        "dep",
        Some("foo"),
        "generic",
        "dep_class",
        "method"
    ));
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn selected_extra_enables_retained_dependency_s3_generic() {
    let root = package("root", &[("f", Some("f <- function() dep::method()"))]);
    let dep = package_with!(
        "dep",
        &[("method", Some("method <- function(x = NULL, ...) x"))],
        Vec::new(),
        export("method"),
        vec![S3Registration {
            generic: slinker::package::GenericSpec {
                package: Some("foo".into()),
                name: "generic".into(),
            },
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

    assert!(program_has_s3_registration(
        &plan,
        "dep",
        Some("foo"),
        "generic",
        "dep_class",
        "method"
    ));
    assert!(
        plan.provenance()
            .nodes()
            .iter()
            .any(|node| { node.package == "foo" && matches!(node.kind, NodeKind::Activation) })
    );
}

#[test]
fn unselected_suggested_guard_prunes_optional_branch() {
    let root = package_with!(
        "root",
        &[(
            "f",
            Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()"),
        )],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with!(
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
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1).analyze("root").unwrap();
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
    assert!(
        plan.program()
            .relocations()
            .iter()
            .any(|relocation| matches!(
                relocation,
                slinker::ir::Relocation::Package {
                    operation: slinker::ir::PackageOperationIr::RequireNamespace { result: false },
                    ..
                }
            ))
    );
}

#[test]
fn selected_extra_enables_guarded_optional_branch_without_rooting_whole_package() {
    let root = package_with!(
        "root",
        &[(
            "f",
            Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()"),
        )],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let foo = package_with!(
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
fn selected_missing_extra_is_reported_as_missing_dependency() {
    let root = package_with!(
        "root",
        &[(
            "f",
            Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()"),
        )],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Suggests: foo\n",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .with_extra_packages(["foo".to_owned()])
        .analyze("root")
        .unwrap();
    assert!(
        plan.provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo" && matches!(&node.kind, NodeKind::MissingPackage))
    );
}

#[test]
fn optional_onload_hook_does_not_activate_suggested_namespace() {
    let root = package("root", &[("f", Some("f <- function() glue::glue(\"x\")"))]);
    let mut glue = package_with!(
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
    Arc::make_mut(&mut glue.index).lifecycle.on_load = true;
    let plan = Linker::new(FakeProvider::new(vec![root, glue]), 4)
        .analyze("root")
        .unwrap();
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "knitr")
    );
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::MissingDependency)
    );
}

#[test]
fn selected_extra_enables_optional_onload_hook_namespace() {
    let root = package("root", &[("f", Some("f <- function() glue::glue(\"x\")"))]);
    let mut glue = package_with!(
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
    Arc::make_mut(&mut glue.index).lifecycle.on_load = true;
    let knitr = package_with!(
        "knitr",
        &[
            ("knit_engines", None),
            ("unused", Some("unused <- function() 1")),
        ],
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
fn external_namespace_is_not_assumed_loaded_for_onload_guard() {
    let root = package("root", &[("f", Some("f <- function() glue::glue(\"x\")"))]);
    let mut glue = package_with!(
        "glue",
        &[
            ("glue", Some("glue <- function(x) x")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(...) if (isNamespaceLoaded(\"knitr\")) knitr::knit_engines$set(glue = glue)",
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
    Arc::make_mut(&mut glue.index).lifecycle.on_load = true;
    let knitr = package_with!(
        "knitr",
        &[("knit_engines", None)],
        Vec::new(),
        export("knit_engines"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "Priority: base\n",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, glue, knitr]), 4)
        .analyze("root")
        .unwrap();
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "knitr")
    );
}

#[test]
fn quoted_iscam_style_symbols_do_not_create_graph_edges() {
    let root = package(
        "root",
        &[(
            "f",
            Some(
                "f <- function() quote(list(atop(P, X), phantom(hat(mu)), sigma, N, M, SD, x1, x2, x3, x4, x5, foo::bar, system.file('data', package = 'foo')))",
            ),
        )],
    );
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();

    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
    for name in [
        "atop", "P", "X", "phantom", "hat", "mu", "sigma", "N", "M", "SD", "x1", "x2", "x3", "x4",
        "x5",
    ] {
        assert!(
            !plan.blockers().iter().any(|diagnostic| {
                diagnostic.binding.as_deref() == Some("f")
                    && diagnostic.code == RejectCode::UnresolvedBinding
                    && diagnostic.message.contains(name)
            }),
            "quoted symbol {name} leaked into lexical dependency diagnostics"
        );
    }
    assert!(!plan.blockers().iter().any(|diagnostic| {
        matches!(
            diagnostic.code,
            RejectCode::MissingDependency | RejectCode::MissingResource
        )
    }));
}

#[test]
fn conditional_special_callee_blocks_path_dependent_specialization() {
    let root = package(
        "root",
        &[(
            "f",
            Some(
                "f <- function(flag) { if (flag) system.file <- identity; system.file('data', package = 'foo') }",
            ),
        )],
    );
    let foo = package_with!(
        "foo",
        &[],
        Vec::new(),
        ExportMap::new(),
        Vec::new(),
        Vec::new(),
        vec!["data".into()],
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .analyze("root")
        .unwrap();

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::SemanticAmbiguity
            && diagnostic.message.contains("system.file")
    }));
    assert!(
        !plan
            .program()
            .resources()
            .iter()
            .any(|resource| { plan.program().package(resource.package).identity().name == "foo" })
    );
}

#[test]
fn repeated_predicate_refines_conditional_local_fallthrough() {
    let root = package(
        "root",
        &[(
            "f",
            Some(
                "f <- function(alternative) { if (!is.null(alternative)) { tvalue <- 1 }; if (!is.null(alternative)) tvalue }",
            ),
        )],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::PotentialUnboundLocal
            && diagnostic.message.contains("tvalue")
    }));
}

#[test]
fn non_returning_package_helper_refines_exhaustive_dispatch() {
    let root = package_with!(
        "root",
        &[
            (
                "f",
                Some(
                    r#"f <- function(direction) {
                        if (direction == "below") showprob <- 1
                        else if (direction == "above") showprob <- 2
                        else .stop_invalid_direction()
                        showprob
                    }"#,
                ),
            ),
            (
                ".stop_invalid_direction",
                Some(".stop_invalid_direction <- function() { stop('invalid direction') }"),
            ),
        ],
        Vec::new(),
        export("f"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "root", ".stop_invalid_direction"));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::PotentialUnboundLocal
            && diagnostic.message.contains("showprob")
    }));
}

#[test]
fn non_returning_summary_does_not_hide_real_invalid_input_fallthrough() {
    let root = package(
        "root",
        &[(
            "f",
            Some(
                r#"f <- function(alternative) {
                    if (!is.null(alternative)) {
                        if (alternative == "less") pvalue <- 1
                        else if (alternative == "greater") pvalue <- 2
                        else if (alternative == "two.sided") pvalue <- 3
                    }
                    if (!is.null(alternative)) pvalue
                }"#,
            ),
        )],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::PotentialUnboundLocal
            && diagnostic.message.contains("pvalue")
    }));
}

#[test]
fn conditional_local_fallthrough_is_not_reported_as_missing_dependency() {
    let root = package(
        "root",
        &[("f", Some("f <- function(flag) { if (flag) x <- 1; x }"))],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::PotentialUnboundLocal
            && diagnostic.message.contains("x")
    }));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && matches!(
                diagnostic.code,
                RejectCode::MissingDependency | RejectCode::UnresolvedBinding
            )
            && diagnostic.message.contains("x")
    }));
}

#[test]
fn later_formal_default_does_not_escape_to_package_resolution() {
    let root = package("root", &[("f", Some("f <- function(x = y, y = 1) x"))]);
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f") && diagnostic.message.contains("`y`")
    }));
}

#[test]
fn for_induction_variable_is_bound_inside_loop_body() {
    let root = package(
        "root",
        &[("f", Some("f <- function(xs) { for (x in xs) print(x) }"))],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f") && diagnostic.message.contains("`x`")
    }));
}

#[test]
fn for_induction_variable_after_loop_keeps_zero_iteration_fallthrough() {
    let root = package(
        "root",
        &[(
            "f",
            Some("f <- function(xs) { for (x in xs) {}; print(x) }"),
        )],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::PotentialUnboundLocal
            && diagnostic.message.contains("`x`")
    }));
}

#[test]
fn captured_activation_superassignment_does_not_require_package_binding() {
    let root = package(
        "root",
        &[(
            "outer",
            Some("outer <- function() { x <- 1; inner <- function() x <<- x + 1; inner() }"),
        )],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("outer")
            && diagnostic.code == RejectCode::EnvironmentMutation
            && diagnostic.message.contains("`x`")
    }));
}

#[test]
fn uncaptured_superassignment_remains_environment_mutation_blocker() {
    let root = package(
        "root",
        &[("outer", Some("outer <- function() { function() x <<- 1 }"))],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("outer")
            && diagnostic.code == RejectCode::EnvironmentMutation
            && diagnostic.message.contains("`x`")
    }));
}

#[test]
fn private_non_returning_helper_refines_enclosing_private_closure() {
    let mut root = package_with!(
        "root",
        &[(
            "public",
            Some(
                "public <- function(direction) { if (direction == 'ok') value <- 1 else .die(); value }",
            ),
        )],
        Vec::new(),
        export("public"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    root.bindings
        .get_mut("public")
        .unwrap()
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    root.private_environments.insert(
        "private:1".into(),
        PrivateEnvironmentImage {
            id: "private:1".into(),
            parent: "namespace:root".into(),
            bindings: HashMap::from([(
                ".die".into(),
                private_closure(
                    ".die",
                    "private:1",
                    ".die <- function() { stop('invalid') }",
                ),
            )]),
        },
    );

    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();

    assert!(retained_private_binding(&plan, "root", "private:1", ".die"));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("public")
            && diagnostic.code == RejectCode::PotentialUnboundLocal
            && diagnostic.message.contains("value")
    }));
}

#[test]
fn graph_export_is_deterministic_semantic_and_count_consistent() {
    let analyze = || {
        let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
        let foo = package_with!(
            "foo",
            &[("bar", Some("bar <- function() 1"))],
            Vec::new(),
            export("bar"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "",
        );
        Linker::new(FakeProvider::new(vec![root, foo]), 2)
            .analyze("root")
            .unwrap()
    };
    let first_plan = analyze();
    let second_plan = analyze();
    let target = TargetEnvironment {
        r_home: PathBuf::from("/opt/R"),
        target: Target {
            r_version: "4.6.1".into(),
            os: "mingw32".into(),
            arch: "x86_64".into(),
        },
        libraries: Vec::new(),
        base_bindings: Default::default(),
    };

    let first = GraphExport::from_plan(&first_plan, &target, "root").unwrap();
    let second = GraphExport::from_plan(&second_plan, &target, "root").unwrap();
    let first_json = serde_json::to_string_pretty(&first).unwrap();
    let second_json = serde_json::to_string_pretty(&second).unwrap();

    assert_eq!(first_json, second_json);
    assert_eq!(first.schema_version, 2);
    assert_eq!(first.stats.nodes, first.nodes.len());
    assert_eq!(first.stats.edges, first.edges.len());
    assert_eq!(first.stats.roots, first.roots.len());
    assert_eq!(first.stats.nodes, first_plan.provenance().nodes().len());
    assert_eq!(first.stats.edges, first_plan.provenance().edges().len());
    assert!(first.nodes.windows(2).all(|pair| pair[0].id <= pair[1].id));
    assert!(first.roots.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(first.edges.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(first.edges.iter().any(|edge| {
        edge.from == "root::f"
            && edge.to == "foo::bar"
            && edge.reasons == vec![GraphEdgeReasonExport::QualifiedReference]
    }));
    assert!(first.root_reasons.iter().any(|root| {
        root.id == "root::f" && root.reasons == vec![GraphEdgeReasonExport::ExportRoot]
    }));
    assert!(first.root_reasons.iter().any(|root| {
        root.id == "package:root" && root.reasons == vec![GraphEdgeReasonExport::PackageRoot]
    }));
}

#[test]
fn graph_export_survives_blocked_analysis() {
    let root = package("root", &[("awkward", Some("awkward <- function() {"))]);
    let provider = FakeProvider::new(vec![root])
        .validation(SyntaxValidation::Rejected("unexpected end of input".into()));
    let plan = Linker::new(provider, 1).analyze("root").unwrap();
    assert!(!plan.blockers().is_empty());

    let target = TargetEnvironment {
        r_home: PathBuf::from("/opt/R"),
        target: Target {
            r_version: "4.6.1".into(),
            os: "mingw32".into(),
            arch: "x86_64".into(),
        },
        libraries: Vec::new(),
        base_bindings: Default::default(),
    };
    let export = GraphExport::from_plan(&plan, &target, "root").unwrap();
    let json = serde_json::to_string(&export).unwrap();

    assert!(!export.nodes.is_empty());
    assert!(!export.blockers.is_empty());
    assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
}

#[test]
fn explanation_dag_is_deterministic_coalesced_and_round_trips() {
    let analyze = || {
        let root = package(
            "root",
            &[("f", Some("f <- function() { foo::bar(); foo::bar() }"))],
        );
        let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
        Linker::new(FakeProvider::new(vec![root, foo]), 2)
            .analyze("root")
            .unwrap()
    };
    let first = ExplanationDag::from_plan(&analyze(), &test_target(), "root").unwrap();
    let second = ExplanationDag::from_plan(&analyze(), &test_target(), "root").unwrap();
    let json = serde_json::to_string_pretty(&first).unwrap();

    assert_eq!(json, serde_json::to_string_pretty(&second).unwrap());
    assert_eq!(
        serde_json::from_str::<ExplanationDag>(&json).unwrap(),
        first
    );
    assert_eq!(first.stats.raw_edges, 4);
    assert!(first.edges.iter().any(|edge| {
        edge.occurrences == 2
            && edge.evidence.iter().all(|evidence| {
                evidence.from_member == "root::f" && evidence.to_member == "foo::bar"
            })
    }));
    let foo = first
        .packages
        .iter()
        .find(|package| package.name == "foo")
        .expect("foo summary");
    assert_eq!(foo.entry_bindings, ["foo::bar"]);
    assert!(!foo.boundary_edges.is_empty());
}

#[test]
fn explanation_dag_condenses_cycles_and_remains_acyclic() {
    let root = package(
        "root",
        &[
            ("a", Some("a <- function() b()")),
            ("b", Some("b <- function() a()")),
        ],
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    let explanation = ExplanationDag::from_plan(&plan, &test_target(), "root").unwrap();
    let cycle = explanation
        .components
        .iter()
        .find(|component| {
            component
                .members
                .iter()
                .any(|member| member.id == "root::a")
                && component
                    .members
                    .iter()
                    .any(|member| member.id == "root::b")
        })
        .expect("a/b component");

    assert!(cycle.cyclic);
    assert_eq!(cycle.members.len(), 2);
    assert!(explanation.edges.iter().all(|edge| edge.from != edge.to));
    let ids = explanation
        .components
        .iter()
        .map(|component| component.id.as_str())
        .collect::<HashSet<_>>();
    assert!(
        explanation
            .edges
            .iter()
            .all(|edge| ids.contains(edge.from.as_str()) && ids.contains(edge.to.as_str()))
    );
}

#[test]
fn explanation_dag_attributes_roots_and_redundant_edges() {
    let root = package_with!(
        "root",
        &[
            ("f", Some("f <- function() { a(); c() }")),
            ("g", Some("g <- function() c()")),
            ("a", Some("a <- function() b()")),
            ("b", Some("b <- function() c()")),
            ("c", Some("c <- function() 1")),
        ],
        Vec::new(),
        ExportMap::from([("f".into(), "f".into()), ("g".into(), "g".into())]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "",
    );
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .analyze("root")
        .unwrap();
    let explanation = ExplanationDag::from_plan(&plan, &test_target(), "root").unwrap();
    let component = |member: &str| {
        explanation
            .components
            .iter()
            .find(|component| component.members.iter().any(|item| item.id == member))
            .expect("member component")
    };
    let f = component("root::f");
    let c = component("root::c");

    assert_eq!(c.root_causes, ["root::f", "root::g"]);
    assert!(
        explanation
            .edges
            .iter()
            .any(|edge| { edge.from == f.id && edge.to == c.id && edge.reachability_redundant })
    );
}
