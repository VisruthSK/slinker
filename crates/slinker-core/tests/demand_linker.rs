mod support;

use slinker_core::analysis::{
    EdgeKind, ExplanationDag, GraphEdgeReasonExport, Linker, NodeKind, RejectCode,
};
use slinker_core::package::{
    BindingImage, BindingName, BindingOrigin, BindingRepresentation, CanonicalSyntax,
    ClosureSource, DatasetName, Digest, DispatchSubject, EmbeddedClosureSource, ExportMap,
    GenericName, ImportBinding, ImportSpec, InstalledPackage, LifecycleMetadata, NativeComponent,
    NativeFacts, NativeLibrary, NativeRegistration, NativeRoutineSummary, NativeSafety,
    NativeSymbolBinding, ObjectImage, ObjectIssue, ObjectKind, PackageData, PackageIdentity,
    PackageImage, PackageIndex, PackageLocation, PackageProvider, PackageResolver,
    PrivateBindingImage, PrivateEnvironmentImage, S3Registration, SyntaxValidation,
};
use slinker_core::package::{EnvironmentLabel, MemberPath, ObjectIssueKind, UnsupportedObject};
use slinker_core::{Description, Error, Result, Target, TargetEnvironment};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct FakeProvider {
    packages: HashMap<String, Arc<PackageImage>>,
    target_environment: TargetEnvironment,
    image_counts: Arc<Mutex<HashMap<String, usize>>>,
    optional_locate_counts: Arc<Mutex<HashMap<String, usize>>>,
    dispatch: HashMap<(Option<String>, String), BTreeSet<GenericName>>,
    validation: SyntaxValidation,
}

impl FakeProvider {
    fn new(images: Vec<PackageImage>) -> Self {
        Self {
            packages: images
                .into_iter()
                .map(|image| (image.index.identity.name.to_string(), Arc::new(image)))
                .collect(),
            image_counts: Arc::new(Mutex::new(HashMap::new())),
            optional_locate_counts: Arc::new(Mutex::new(HashMap::new())),
            validation: SyntaxValidation::Accepted,
            dispatch: HashMap::new(),
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
                    "names<-",
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
                    "assign",
                    "delayedAssign",
                    "globalenv",
                    "environment<-",
                    "lapply",
                    "get",
                    "get0",
                    "exists",
                    "match.fun",
                    "do.call",
                    "reg.finalizer",
                    "declare",
                    "getNamespaceInfo",
                    "getNamespaceImports",
                    "registerS3method",
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
                .map(BindingName::from)
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

    fn dispatching(mut self, package: Option<&str>, binding: &str, generics: &[&str]) -> Self {
        self.dispatch.insert(
            (package.map(str::to_owned), binding.to_owned()),
            generics.iter().copied().map(GenericName::from).collect(),
        );
        self
    }

    fn validation(mut self, validation: SyntaxValidation) -> Self {
        self.validation = validation;
        self
    }
}

impl PackageResolver for FakeProvider {
    fn target_environment(&self) -> &TargetEnvironment {
        &self.target_environment
    }

    fn locate(&self, name: &str) -> Result<Option<InstalledPackage>> {
        *self
            .optional_locate_counts
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_default() += 1;
        Ok(self.packages.get(name).map(|image| installed(&image.index)))
    }
}

impl PackageProvider for FakeProvider {
    fn index(&self, package: &InstalledPackage) -> Result<Arc<PackageIndex>> {
        self.packages
            .get(package.identity.name.as_str())
            .map(|image| Arc::clone(&image.index))
            .ok_or_else(|| Error::Analysis(format!("missing fake index {}", package.identity.name)))
    }

    fn binding_image(&self, package: &InstalledPackage, _name: &str) -> Result<Arc<PackageImage>> {
        *self
            .image_counts
            .lock()
            .unwrap()
            .entry(package.identity.name.to_string())
            .or_default() += 1;
        self.packages
            .get(package.identity.name.as_str())
            .cloned()
            .ok_or_else(|| Error::Analysis(format!("missing fake image {}", package.identity.name)))
    }

    fn dispatch_generics(&self, subject: DispatchSubject<'_>) -> Result<BTreeSet<GenericName>> {
        let key = match subject {
            DispatchSubject::Base { binding } => (None, binding.to_owned()),
            DispatchSubject::Installed { package, binding } => {
                (Some(package.identity.name.to_string()), binding.to_owned())
            }
        };
        Ok(self.dispatch.get(&key).cloned().unwrap_or_default())
    }

    fn validate_syntax(&self, _source: &str) -> Result<SyntaxValidation> {
        Ok(self.validation.clone())
    }

    fn canonical_syntax(&self, source: &str) -> Result<CanonicalSyntax> {
        Ok(CanonicalSyntax::Stable(source.to_owned()))
    }
}

fn root_calling(dependency: &str, entry: &str) -> PackageImage {
    let source = format!("f <- function() {dependency}::{entry}()");
    package("root", &[("f", Some(&source))])
}

fn package(name: &str, bindings: &[(&str, Option<&str>)]) -> PackageImage {
    PackageFixture::new(name, bindings).exporting_all().build()
}

fn package_importing(name: &str, bindings: &[(&str, Option<&str>)], imports: &str) -> PackageImage {
    PackageFixture::new(name, bindings)
        .exporting_all()
        .description(format!("Imports: {imports}\n"))
        .build()
}

fn utils_platform() -> PackageImage {
    PackageFixture::new(
        "utils",
        &[("packageVersion", None), ("packageDescription", None)],
    )
    .exporting_all()
    .description("Priority: base\n")
    .build()
}

struct PackageFixture<'a> {
    name: &'a str,
    bindings: &'a [(&'a str, Option<&'a str>)],
    imports: Vec<ImportSpec>,
    exports: ExportMap,
    s3: Vec<S3Registration>,
    dynlibs: Vec<NativeComponent>,
    files: Vec<String>,
    description: String,
}

impl<'a> PackageFixture<'a> {
    fn new(name: &'a str, bindings: &'a [(&'a str, Option<&'a str>)]) -> Self {
        Self {
            name,
            bindings,
            imports: Vec::new(),
            exports: ExportMap::new(),
            s3: Vec::new(),
            dynlibs: Vec::new(),
            files: Vec::new(),
            description: String::new(),
        }
    }

    fn exporting_all(self) -> Self {
        let exports = self
            .bindings
            .iter()
            .map(|(binding, _)| ((*binding).into(), (*binding).into()))
            .collect();
        self.exports(exports)
    }

    fn imports(mut self, imports: Vec<ImportSpec>) -> Self {
        self.imports = imports;
        self
    }

    fn exports(mut self, exports: ExportMap) -> Self {
        self.exports = exports;
        self
    }

    fn s3(mut self, s3: Vec<S3Registration>) -> Self {
        self.s3 = s3;
        self
    }

    fn dynlibs(mut self, dynlibs: Vec<NativeComponent>) -> Self {
        self.dynlibs = dynlibs;
        self
    }

    fn files(mut self, files: Vec<String>) -> Self {
        self.files = files;
        self
    }

    fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    fn build(self) -> PackageImage {
        let name = self.name;
        let bindings = self
            .bindings
            .iter()
            .map(|(binding, source)| {
                let closure = source.map(|source| ClosureSource {
                    source: Arc::from(source),
                    environment: EnvironmentLabel::namespace(name),
                });
                let object_kind = if closure.is_some() {
                    ObjectKind::Closure
                } else {
                    ObjectKind::Integer
                };
                let object = ObjectImage {
                    closure,
                    ..ObjectImage::of_kind(BindingRepresentation::Value, object_kind)
                };
                (
                    BindingName::from(*binding),
                    Arc::new(BindingImage {
                        name: (*binding).into(),
                        origin: BindingOrigin::Code,
                        object,
                    }),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut binding_names = bindings.keys().cloned().collect::<Vec<_>>();
        binding_names.sort();
        PackageImage {
            index: Arc::new(PackageIndex {
                identity: PackageIdentity {
                    name: name.into(),
                    version: "1.0.0".parse().expect("valid test package version"),
                    image_fingerprint: Digest::from(format!("fp-{name}")),
                },
                description: Description::parse(&format!(
                    "Package: {name}\nVersion: 1.0.0\n{}",
                    self.description
                )),
                exports: self.exports,
                imports: self.imports,
                s3: self.s3,
                dynlibs: self.dynlibs,
                lifecycle: LifecycleMetadata::default(),
                binding_names: binding_names.into(),
                data: PackageData::default(),
                files: self.files,
                has_sysdata: false,
            }),
            bindings,
            private_environments: HashMap::new(),
        }
    }
}

fn analyze_images(images: Vec<PackageImage>) -> slinker_core::analysis::LinkIr {
    link(FakeProvider::new(images))
}

fn link(provider: FakeProvider) -> slinker_core::analysis::LinkIr {
    Linker::new(provider, 1).analyze("root").unwrap()
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
    ExportMap::from([(name.into(), name.into())])
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

fn unresolved_name_evidence<'a>(
    plan: &'a slinker_core::analysis::LinkIr,
    name: &'a str,
) -> impl Iterator<Item = &'a slinker_core::analysis::Evidence> + 'a {
    plan.blockers()
        .iter()
        .filter(|diagnostic| diagnostic.code == RejectCode::UnresolvedBinding)
        .flat_map(|diagnostic| diagnostic.evidence.iter())
        .filter(move |evidence| evidence.detail.contains(name))
}

fn retained_binding(plan: &slinker_core::analysis::LinkIr, package: &str, binding: &str) -> bool {
    plan.provenance().nodes().iter().any(|node| {
        node.package == package
            && matches!(&node.kind, NodeKind::Binding { name } if name == binding)
    })
}

fn retained_private_binding(
    plan: &slinker_core::analysis::LinkIr,
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
    plan: &slinker_core::analysis::LinkIr,
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
            let generic_package_matches = match &registration.generic.home {
                slinker_core::ir::GenericHome::Lexical => None,
                slinker_core::ir::GenericHome::Program(package) => {
                    Some(plan.program().package(*package).identity().name.as_str())
                }
                slinker_core::ir::GenericHome::Optional(package) => Some(package.as_str()),
            } == generic_package;
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
        object: ObjectImage {
            representation: slinker_core::package::BindingRepresentation::Value,
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
        },
    }
}

#[test]
fn retaining_structured_object_executes_nested_closures() {
    let mut root = PackageFixture::new(
        "root",
        &[
            ("generator_funs", None),
            (
                "nested_dependency",
                Some("nested_dependency <- function() 1"),
            ),
        ],
    )
    .exports(export("generator_funs"))
    .build();
    let binding = Arc::make_mut(root.bindings.get_mut("generator_funs").unwrap());
    binding.object.object_kind = ObjectKind::List;
    binding
        .object
        .embedded_closures
        .push(EmbeddedClosureSource {
            path: "$[[1]]".into(),
            source: Arc::from(".slinker_embedded <- function() nested_dependency()"),
            environment: "namespace:root".into(),
        });

    let plan = analyze_images(vec![root]);
    assert!(retained_binding(&plan, "root", "nested_dependency"));
}

#[test]
fn runtime_construction_executes_reenclosed_closures_in_derived_environment() {
    let mut root = PackageFixture::new("root", &[
            (
                "f",
                Some(
                    "f <- function() { generator <- new.env(parent = capsule); generator$self <- generator; methods <- assign_func_envs(templates, generator); list2env2(methods, generator); generator }",
                ),
            ),
            ("capsule", None),
            ("templates", None),
        ]).exports(export("f")).build();
    Arc::make_mut(root.bindings.get_mut("f").unwrap())
        .object
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    let capsule = Arc::make_mut(root.bindings.get_mut("capsule").unwrap());
    capsule.object.object_kind = ObjectKind::Environment;
    capsule.object.environment = Some("private:1".into());
    {
        let templates = Arc::make_mut(root.bindings.get_mut("templates").unwrap());
        templates.object.object_kind = ObjectKind::List;
        for name in ["first", "second"] {
            templates
                .object
                .embedded_closures
                .push(EmbeddedClosureSource {
                    path: MemberPath::root().field(name),
                    source: Arc::from(format!(
                        ".slinker_embedded <- function() {{ self; {name}_dependency() }}"
                    )),
                    environment: "namespace:root".into(),
                });
        }
    }
    for name in ["first", "second"] {
        root.bindings.insert(
            format!("{name}_dependency").into(),
            Arc::new(BindingImage {
                name: format!("{name}_dependency").into(),
                origin: BindingOrigin::Code,
                object: ObjectImage {
                    representation: slinker_core::package::BindingRepresentation::Value,
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
            }),
        );
    }
    root.private_environments.insert(
        "private:1".into(),
        Arc::new(PrivateEnvironmentImage {
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
        }),
    );
    Arc::make_mut(&mut root.index).binding_names = root
        .bindings
        .keys()
        .map(|name| name.as_str().into())
        .collect();

    let plan = analyze_images(vec![root]);

    assert!(retained_binding(&plan, "root", "first_dependency"));
    assert!(retained_binding(&plan, "root", "second_dependency"));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding && diagnostic.message.contains("self")
    }));
}

#[test]
fn unknown_closure_enclosure_reports_root_cause_without_lexical_cascade() {
    let mut root = package("root", &[("f", Some("f <- function() self + classname"))]);
    Arc::make_mut(root.bindings.get_mut("f").unwrap())
        .object
        .closure
        .as_mut()
        .unwrap()
        .environment = "unsupported:dynamic".into();
    let plan = analyze_images(vec![root]);

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
fn root_keeps_every_binding_while_dependencies_keep_only_reached_ones() {
    let root = PackageFixture::new(
        "root",
        &[
            ("public", Some("public <- function() dep::api()")),
            ("internal", Some("internal <- function() 2")),
        ],
    )
    .exports(export("public"))
    .build();
    let dep = PackageFixture::new(
        "dep",
        &[
            ("api", Some("api <- function() helper()")),
            ("helper", Some("helper <- function() 1")),
            (
                "unused_optional",
                Some("unused_optional <- function() foo::bar()"),
            ),
        ],
    )
    .exports(export("api"))
    .description("Suggests: foo\n")
    .build();
    let foo = PackageFixture::new(
        "foo",
        &[
            ("bar", Some("bar <- function() hidden()")),
            ("hidden", Some("hidden <- function() 1")),
        ],
    )
    .exports(export("bar"))
    .build();
    let provider = FakeProvider::new(vec![root, dep, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 2).analyze("root").unwrap();

    assert!(retained_binding(&plan, "root", "public"));
    assert!(retained_binding(&plan, "root", "internal"));
    assert!(retained_binding(&plan, "dep", "api"));
    assert!(retained_binding(&plan, "dep", "helper"));
    assert!(!retained_binding(&plan, "dep", "unused_optional"));
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
    let mut root = PackageFixture::new("root", &[("public", Some("public <- function() 1"))])
        .exports(export("public"))
        .description("Suggests: foo\n")
        .build();
    Arc::make_mut(root.bindings.get_mut("public").unwrap())
        .object
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    root.private_environments.insert(
        "private:1".into(),
        Arc::new(PrivateEnvironmentImage {
            id: "private:1".into(),
            parent: "namespace:root".into(),
            bindings: HashMap::from([(
                "unused".into(),
                private_closure("unused", "private:1", "unused <- function() foo::bar()"),
            )]),
        }),
    );
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let plan = analyze_images(vec![root, foo]);
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
    let mut root =
        PackageFixture::new("root", &[("public", Some("public <- function() helper()"))])
            .exports(export("public"))
            .description("Suggests: foo\n")
            .build();
    Arc::make_mut(root.bindings.get_mut("public").unwrap())
        .object
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    root.private_environments.insert(
        "private:1".into(),
        Arc::new(PrivateEnvironmentImage {
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
        }),
    );
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let plan = analyze_images(vec![root, foo]);
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
    let mut root = PackageFixture::new("root", &[("public", Some("public <- function() 1"))])
        .exports(export("public"))
        .build();
    Arc::make_mut(root.bindings.get_mut("public").unwrap())
        .object
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    root.private_environments.insert(
        "private:1".into(),
        Arc::new(PrivateEnvironmentImage {
            id: "private:1".into(),
            parent: "namespace:root".into(),
            bindings: HashMap::from([(
                "bad".into(),
                PrivateBindingImage {
                    name: "bad".into(),
                    object: ObjectImage {
                        representation: slinker_core::package::BindingRepresentation::Value,
                        classes: Vec::new(),
                        object_kind: ObjectKind::Unsupported(UnsupportedObject::SexpType(22)),
                        closure: None,
                        environment: None,
                        embedded_closures: Vec::new(),
                        embedded_environments: Vec::new(),
                        issues: vec![ObjectIssue {
                            path: "$".into(),
                            kind: ObjectIssueKind::ExternalPointer,
                            detail: "external pointer".into(),
                        }],
                    },
                },
            )]),
        }),
    );
    let plan = analyze_images(vec![root]);
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnsupportedObject)
    );
}

#[test]
fn onload_can_create_a_missing_exported_active_binding() {
    let mut root = PackageFixture::new("root", &[
            ("dummy", Some("dummy <- function() NULL")),
            ("get_pb", Some("get_pb <- function() 1")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(lib, pkg) { pkgenv <- environment(dummy); makeActiveBinding(\"pb\", get_pb, pkgenv) }",
                ),
            ),
        ]).exports(export("pb")).build();
    Arc::make_mut(&mut root.index).lifecycle.on_load = true;
    let plan = analyze_images(vec![root]);
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding
            && diagnostic.binding.as_deref() == Some("pb")
    }));
    assert!(retained_binding(&plan, "root", "get_pb"));
}

#[test]
fn dependency_onload_can_create_a_missing_exported_active_binding() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function() foo::pb"))])
        .exports(export("f"))
        .description("Imports: foo\n")
        .build();
    let mut foo = PackageFixture::new("foo", &[
            ("dummy", Some("dummy <- function() NULL")),
            ("get_pb", Some("get_pb <- function() 1")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(lib, pkg) { pkgenv <- environment(dummy); makeActiveBinding(\"pb\", get_pb, pkgenv) }",
                ),
            ),
        ]).exports(export("pb")).build();
    Arc::make_mut(&mut foo.index).lifecycle.on_load = true;
    let plan = analyze_images(vec![root, foo]);
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding
            && diagnostic.package == "foo"
            && diagnostic.binding.as_deref() == Some("pb")
    }));
    assert!(retained_binding(&plan, "foo", "get_pb"));
}

#[test]
fn runtime_make_active_binding_does_not_satisfy_missing_export() {
    let root = PackageFixture::new(
        "root",
        &[(
            "f",
            Some("f <- function() makeActiveBinding(\"pb\", function() 1, asNamespace(\"root\"))"),
        )],
    )
    .exports(ExportMap::from([
        ("f".into(), "f".into()),
        ("pb".into(), "pb".into()),
    ]))
    .build();
    let plan = analyze_images(vec![root]);
    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnresolvedBinding
            && diagnostic.binding.as_deref() == Some("pb")
    }));
}

#[test]
fn dependency_lifecycle_is_an_entrypoint_even_when_not_exported() {
    let mut dep = PackageFixture::new(
        "dep",
        &[
            ("public", Some("public <- function() 1")),
            (
                ".onLoad",
                Some(".onLoad <- function(...) initialize_state()"),
            ),
            ("initialize_state", Some("initialize_state <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
    )
    .exports(export("public"))
    .build();
    Arc::make_mut(&mut dep.index).lifecycle.on_load = true;
    let plan = Linker::new(
        FakeProvider::new(vec![root_calling("dep", "public"), dep]),
        1,
    )
    .analyze("root")
    .unwrap();

    assert!(retained_binding(&plan, "dep", ".onLoad"));
    assert!(retained_binding(&plan, "dep", "initialize_state"));
    assert!(!retained_binding(&plan, "dep", "unused"));
}

#[test]
fn root_reexported_import_is_demanded_without_local_binding() {
    let root = PackageFixture::new("root", &[])
        .imports(vec![ImportSpec::From {
            package: "utils".into(),
            bindings: vec![ImportBinding {
                local: "head".into(),
                remote: "head".into(),
            }],
        }])
        .exports(export("head"))
        .description("Imports: utils\n")
        .build();
    let utils = PackageFixture::new(
        "utils",
        &[
            ("head", Some("head <- function(x) x")),
            ("unused", Some("unused <- function() 1")),
        ],
    )
    .exports(export("head"))
    .build();
    let plan = analyze_images(vec![root, utils]);

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
    let foo = PackageFixture::new(
        "foo",
        &[
            ("bar", Some("bar <- function() helper()")),
            ("helper", Some("helper <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
    )
    .exports(export("bar"))
    .build();
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
    let foo = PackageFixture::new("foo", &[("bar", Some("bar <- function() baz::qux()"))])
        .exports(export("bar"))
        .build();
    let baz = PackageFixture::new(
        "baz",
        &[
            ("qux", Some("qux <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
    )
    .exports(export("qux"))
    .build();
    let plan = Linker::new(FakeProvider::new(vec![root, foo, baz]), 2)
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(retained_binding(&plan, "baz", "qux"));
    assert!(!retained_binding(&plan, "baz", "unused"));
}

#[test]
fn unused_root_import_metadata_does_not_create_reachability() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function() 1"))])
        .imports(vec![ImportSpec::All {
            package: "foo".into(),
            except: Vec::new(),
        }])
        .exports(export("f"))
        .build();
    let foo = PackageFixture::new(
        "foo",
        &[
            ("a", Some("a <- function() 1")),
            ("b", Some("b <- function() 2")),
        ],
    )
    .exports(ExportMap::from([
        ("a".into(), "a".into()),
        ("b".into(), "b".into()),
    ]))
    .build();
    let plan = analyze_images(vec![root, foo]);
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
    let root = PackageFixture::new("root", &[("f", Some("f <- function() local_x()"))])
        .imports(vec![ImportSpec::From {
            package: "foo".into(),
            bindings: vec![ImportBinding {
                local: "local_x".into(),
                remote: "x".into(),
            }],
        }])
        .exports(export("f"))
        .build();
    let foo = PackageFixture::new(
        "foo",
        &[
            ("x", Some("x <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
    )
    .exports(export("x"))
    .build();
    let plan = analyze_images(vec![root, foo]);
    assert!(retained_binding(&plan, "foo", "x"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn import_all_resolves_reachable_export_only() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function() x()"))])
        .imports(vec![ImportSpec::All {
            package: "foo".into(),
            except: Vec::new(),
        }])
        .exports(export("f"))
        .build();
    let foo = PackageFixture::new(
        "foo",
        &[
            ("x", Some("x <- function() 1")),
            ("y", Some("y <- function() 2")),
        ],
    )
    .exports(ExportMap::from([
        ("x".into(), "x".into()),
        ("y".into(), "y".into()),
    ]))
    .build();
    let plan = analyze_images(vec![root, foo]);
    assert!(retained_binding(&plan, "foo", "x"));
    assert!(!retained_binding(&plan, "foo", "y"));
}

#[test]
fn depends_metadata_alone_does_not_create_reachability() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function() 1"))])
        .exports(export("f"))
        .description("Depends: foo\n")
        .build();
    let foo = package("foo", &[("x", Some("x <- function() 1"))]);
    let plan = analyze_images(vec![root, foo]);
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
    let foo = PackageFixture::new(
        "foo",
        &[
            ("bar", Some("bar <- function() hidden()")),
            ("hidden", Some("hidden <- function() 1")),
        ],
    )
    .exports(export("bar"))
    .build();
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1)
        .with_external_packages(["foo"])
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
    let plan = link(provider);
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
        let plan = analyze_images(vec![root]);
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
    let root = PackageFixture::new(
        "root",
        &[
            ("f", Some("f <- function() helper(\"foo\")")),
            (
                "helper",
                Some("helper <- function(package) requireNamespace(package)"),
            ),
        ],
    )
    .exports(export("f"))
    .build();
    let foo = package("foo", &[]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_external_packages(["foo"])
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
                && package.role() == slinker_core::ir::PackageRole::External)
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
    let plan = analyze_images(vec![root]);

    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
}

#[test]
fn bounded_string_operations_specialize_namespace_helper() {
    let root = PackageFixture::new("root", &[
            ("f", Some("f <- function() helper(\"foo-extra\")")),
            (
                "helper",
                Some(
                    "helper <- function(spec) { parts <- strsplit(spec, \"-\", fixed = TRUE)[[1L]]; package <- paste0(parts[[1L]], \"\"); requireNamespace(package) }",
                ),
            ),
        ]).exports(export("f")).build();
    let foo = package("foo", &[]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_external_packages(["foo"])
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
    let root = PackageFixture::new("root", &[
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
        ]).exports(export("f")).build();
    let plan = analyze_images(vec![root]);

    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
}

#[test]
fn resolved_null_coalescing_helper_propagates_constant() {
    let root = PackageFixture::new(
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
    )
    .exports(export("f"))
    .build();
    let foo = package("foo", &[]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_external_packages(["foo"])
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
    let root = PackageFixture::new("root", &[
            ("f", Some("f <- function() helper(\"short\")")),
            (
                "helper",
                Some(
                    "helper <- function(kind) requireNamespace(switch(kind, short = \"foo\", long = \"bar\"))",
                ),
            ),
        ]).exports(export("f")).build();
    let foo = package("foo", &[]);
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_external_packages(["foo"])
        .analyze("root")
        .unwrap();

    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
}

fn root_calling_helper_with(
    helpers: &[(&str, &str)],
    entry: &str,
) -> slinker_core::analysis::LinkIr {
    let mut bindings = vec![
        ("f", format!("f <- function() helper({entry})")),
        (
            "helper",
            "helper <- function(package) requireNamespace(package)".to_owned(),
        ),
    ];
    bindings.extend(
        helpers
            .iter()
            .map(|(name, source)| (*name, (*source).to_owned())),
    );
    let bindings = bindings
        .iter()
        .map(|(name, source)| (*name, Some(source.as_str())))
        .collect::<Vec<_>>();
    let root = PackageFixture::new("root", &bindings)
        .exports(export("f"))
        .build();
    Linker::new(FakeProvider::new(vec![root, package("foo", &[])]), 1)
        .with_external_packages(["foo"])
        .analyze("root")
        .unwrap()
}

fn has_dynamic_discovery(plan: &slinker_core::analysis::LinkIr) -> bool {
    plan.blockers()
        .iter()
        .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
}

#[test]
fn recursion_starts_from_bottom_so_its_base_case_survives() {
    let plan = root_calling_helper_with(
        &[(
            "pick",
            "pick <- function(n) if (print(n)) pick(n) else \"foo\"",
        )],
        "pick(\"x\")",
    );
    assert!(!has_dynamic_discovery(&plan), "{:?}", plan.blockers());
}

#[test]
fn mutual_recursion_converges_to_its_base_case() {
    let plan = root_calling_helper_with(
        &[
            (
                "ping",
                "ping <- function(n) if (print(n)) pong(n) else \"foo\"",
            ),
            ("pong", "pong <- function(n) ping(n)"),
        ],
        "ping(\"x\")",
    );
    assert!(!has_dynamic_discovery(&plan), "{:?}", plan.blockers());
}

#[test]
fn recursion_without_a_base_case_is_unknown_not_a_silent_constant() {
    let plan = root_calling_helper_with(&[("spin", "spin <- function(n) spin(n)")], "spin(\"x\")");
    assert!(has_dynamic_discovery(&plan), "{:?}", plan.blockers());
}

#[test]
fn unknown_branch_join_keeps_agreeing_values_and_widens_disagreeing_ones() {
    let agreeing = root_calling_helper_with(
        &[(
            "pick",
            "pick <- function(n) if (print(n)) \"foo\" else \"foo\"",
        )],
        "pick(\"x\")",
    );
    assert!(
        !has_dynamic_discovery(&agreeing),
        "{:?}",
        agreeing.blockers()
    );

    let disagreeing = root_calling_helper_with(
        &[(
            "pick",
            "pick <- function(n) if (print(n)) \"foo\" else \"bar\"",
        )],
        "pick(\"x\")",
    );
    assert!(has_dynamic_discovery(&disagreeing));
}

#[test]
fn static_discovery_of_a_declared_dependency_links_it() {
    let root = package_importing(
        "root",
        &[("f", Some("f <- function() requireNamespace(\"foo\")"))],
        "foo",
    );
    let foo = package("foo", &[("x", Some("x <- function() 1"))]);
    let plan = analyze_images(vec![root, foo]);
    assert!(plan.blockers().is_empty(), "{:?}", plan.blockers());
    assert!(
        plan.provenance()
            .nodes()
            .iter()
            .any(|node| { node.package == "foo" && matches!(node.kind, NodeKind::Activation) })
    );
    assert!(plan.program().relocations().iter().any(|relocation| {
        relocation.target == slinker_core::ir::RelocationTarget::RequireNamespace { result: true }
    }));
}

#[test]
fn library_and_attaching_require_reject() {
    for source in [
        "f <- function() library(foo)",
        "f <- function() require(foo)",
    ] {
        let root = package("root", &[("f", Some(source))]);
        let plan = analyze_images(vec![root]);
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
    let plan = analyze_images(vec![root]);
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
    let root = PackageFixture::new("root", &[("f", Some("f <- function(x) .Call(croot_f, x)"))])
        .exports(export("f"))
        .dynlibs(vec![NativeComponent {
            name: "root".into(),
            alias: String::new(),
            registration: Some(NativeRegistration {
                prefix: "c".into(),
                suffix: String::new(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "croot_f".into(),
                symbol: "root_f".into(),
            }],
            library: NativeLibrary::Missing,
            safety: NativeSafety::Safe(NativeFacts {
                callbacks: Vec::new(),
            }),
        }])
        .build();
    let plan = analyze_images(vec![root]);
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
        alias: String::new(),
        registration: Some(NativeRegistration {
            prefix: String::new(),
            suffix: String::new(),
        }),
        symbols: Vec::new(),
        library: NativeLibrary::Missing,
        safety: NativeSafety::Unanalyzed,
    }
}

#[test]
fn opaque_registered_selector_is_consumed_by_native_call() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function(x) .Call(croot_f, x)"))])
        .exports(export("f"))
        .dynlibs(vec![opaque_registered_component()])
        .build();
    let plan = analyze_images(vec![root]);

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
    let root = PackageFixture::new(
        "root",
        &[(
            "f",
            Some("f <- function(x) { identity(croot_f); .Call(croot_f, x) }"),
        )],
    )
    .exports(export("f"))
    .dynlibs(vec![opaque_registered_component()])
    .build();
    let plan = analyze_images(vec![root]);

    assert_eq!(unresolved_name_evidence(&plan, "croot_f").count(), 1);
    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn ordinary_r_binding_beats_opaque_native_selector_fallback() {
    let root = PackageFixture::new(
        "root",
        &[("f", Some("f <- function(x) .Call(foo, x)")), ("foo", None)],
    )
    .exports(export("f"))
    .dynlibs(vec![opaque_registered_component()])
    .build();
    let plan = analyze_images(vec![root]);

    assert!(retained_binding(&plan, "root", "foo"));
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn shadowed_native_primitive_does_not_consume_selector() {
    let root = PackageFixture::new(
        "root",
        &[("f", Some("f <- function(.Call, x) .Call(croot_f, x)"))],
    )
    .exports(export("f"))
    .dynlibs(vec![opaque_registered_component()])
    .build();
    let plan = analyze_images(vec![root]);

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
    assert!(unresolved_name_evidence(&plan, "croot_f").next().is_some());
}

#[test]
fn named_opaque_native_selector_is_matched_by_formal_name() {
    let root = PackageFixture::new(
        "root",
        &[("f", Some("f <- function(x) .Call(x, .NAME = croot_f)"))],
    )
    .exports(export("f"))
    .dynlibs(vec![opaque_registered_component()])
    .build();
    let plan = analyze_images(vec![root]);

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
        alias: String::new(),
        registration: Some(NativeRegistration {
            prefix: "c".into(),
            suffix: String::new(),
        }),
        symbols: vec![NativeSymbolBinding {
            binding: "croot_f".into(),
            symbol: "root_f".into(),
        }],
        library: NativeLibrary::Missing,
        safety: NativeSafety::Safe(NativeFacts {
            callbacks: Vec::new(),
        }),
    };
    let root = PackageFixture::new(
        "root",
        &[(
            "by_symbol",
            Some("by_symbol <- function(x) .Call(\"root_f\", x)"),
        )],
    )
    .exports(export("by_symbol"))
    .dynlibs(vec![component])
    .build();
    let plan = analyze_images(vec![root]);

    assert!(
        !plan
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );

    let root = PackageFixture::new(
        "root",
        &[(
            "by_binding",
            Some("by_binding <- function(x) .Call(\"croot_f\", x)"),
        )],
    )
    .exports(export("by_binding"))
    .dynlibs(vec![NativeComponent {
        name: "root".into(),
        alias: String::new(),
        registration: None,
        symbols: vec![NativeSymbolBinding {
            binding: "croot_f".into(),
            symbol: "root_f".into(),
        }],
        library: NativeLibrary::Missing,
        safety: NativeSafety::Safe(NativeFacts {
            callbacks: Vec::new(),
        }),
    }])
    .build();
    let plan = analyze_images(vec![root]);
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnknownNativeLookup)
    );
}

#[test]
fn registered_native_symbol_can_be_assigned_into_namespace_state() {
    let root = PackageFixture::new(
        "root",
        &[
            ("slot", None),
            (
                ".onLoad",
                Some(".onLoad <- function(lib, pkg) slot <<- croot_tick"),
            ),
        ],
    )
    .dynlibs(vec![NativeComponent {
        name: "root".into(),
        alias: String::new(),
        registration: Some(NativeRegistration {
            prefix: "c".into(),
            suffix: String::new(),
        }),
        symbols: vec![NativeSymbolBinding {
            binding: "croot_tick".into(),
            symbol: "root_tick".into(),
        }],
        library: NativeLibrary::Missing,
        safety: NativeSafety::Safe(NativeFacts {
            callbacks: Vec::new(),
        }),
    }])
    .build();
    let mut root = root;
    Arc::make_mut(&mut root.index).lifecycle.on_load = true;
    let plan = analyze_images(vec![root]);
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
    let mut root = PackageFixture::new(
        "root",
        &[
            ("slot", None),
            (
                ".onLoad",
                Some(".onLoad <- function(lib, pkg) slot <<- croot_tick"),
            ),
        ],
    )
    .dynlibs(vec![NativeComponent {
        name: "root".into(),
        alias: String::new(),
        registration: Some(NativeRegistration {
            prefix: String::new(),
            suffix: String::new(),
        }),
        symbols: Vec::new(),
        library: NativeLibrary::Missing,
        safety: NativeSafety::Unanalyzed,
    }])
    .build();
    Arc::make_mut(&mut root.index).lifecycle.on_load = true;
    let plan = analyze_images(vec![root]);
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
    let foo = PackageFixture::new(
        "foo",
        &[
            ("a", Some("a <- function(x) .Call(foo_a, x)")),
            ("b", Some("b <- function(x) .Call(foo_b, x)")),
            ("unused", Some("unused <- function(x) x")),
        ],
    )
    .exports(export("a"))
    .dynlibs(vec![NativeComponent {
        name: "foo".into(),
        alias: String::new(),
        registration: None,
        symbols: Vec::new(),
        library: NativeLibrary::Missing,
        safety: NativeSafety::Safe(NativeFacts {
            callbacks: Vec::new(),
        }),
    }])
    .build();
    let plan = analyze_images(vec![root, foo]);
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
    let foo = PackageFixture::new(
        "foo",
        &[
            ("a", Some("a <- function() 1")),
            ("callback", Some("callback <- function() 2")),
        ],
    )
    .exports(export("a"))
    .dynlibs(vec![NativeComponent {
        name: "foo".into(),
        alias: String::new(),
        registration: None,
        symbols: Vec::new(),
        library: NativeLibrary::Missing,
        safety: NativeSafety::Safe(NativeFacts {
            callbacks: vec!["callback".into()],
        }),
    }])
    .build();
    let plan = analyze_images(vec![root, foo]);
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
fn declared_callables_link_a_native_callback_parameter() {
    let analyze = |source: &str| {
        let dep = PackageFixture::new(
            "dep",
            &[
                ("a", Some(source)),
                ("callback", Some("callback <- function(x) x")),
                ("other", Some("other <- function(x) x")),
                ("unrelated", Some("unrelated <- function() 3")),
            ],
        )
        .exports(export("a"))
        .dynlibs(vec![NativeComponent {
            name: "root".into(),
            alias: String::new(),
            registration: Some(NativeRegistration {
                prefix: String::new(),
                suffix: String::new(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "root_a".into(),
                symbol: "root_a".into(),
            }],
            library: NativeLibrary::Missing,
            safety: NativeSafety::Summarized(vec![NativeRoutineSummary {
                selector: "root_a".into(),
                callback_arguments: vec![2],
            }]),
        }])
        .build();
        analyze_images(vec![root_calling("dep", "a"), dep])
    };
    let unknown_callback = |plan: &slinker_core::analysis::LinkIr| {
        plan.blockers().iter().any(|diagnostic| {
            diagnostic.code == RejectCode::UnknownNativeEffects
                && diagnostic.message.contains("not statically linkable")
        })
    };

    let undeclared = analyze("a <- function(fun) .Call(root_a, 1, fun)");
    assert!(unknown_callback(&undeclared), "{:?}", undeclared.blockers());

    let declared = analyze(
        "a <- function(fun) { declare(slinker(fun = callables(callback, dep::other))); .Call(root_a, 1, fun) }",
    );
    assert!(!unknown_callback(&declared), "{:?}", declared.blockers());
    assert!(retained_binding(&declared, "dep", "callback"));
    assert!(retained_binding(&declared, "dep", "other"));
    assert!(!retained_binding(&declared, "dep", "unrelated"));
}

#[test]
fn declared_callables_are_retained_where_the_binding_is_applied() {
    for application in ["lapply(1, fun)", "do.call(fun, list(1))"] {
        let root = package(
            "root",
            &[(
                "main",
                Some(&format!(
                    "main <- function(fun) {{ declare(slinker(fun = callables(dep::used))); {application} }}"
                )),
            )],
        );
        let dep = package(
            "dep",
            &[
                ("used", Some("used <- function(x) x")),
                ("unrelated", Some("unrelated <- function() 3")),
            ],
        );
        let plan = analyze_images(vec![root, dep]);
        assert!(retained_binding(&plan, "dep", "used"), "{application}");
        assert!(
            !retained_binding(&plan, "dep", "unrelated"),
            "{application}"
        );
    }
}

#[test]
fn native_callback_argument_summary_adds_a_targeted_call_site_edge() {
    let dep = PackageFixture::new(
        "dep",
        &[
            ("a", Some("a <- function() .Call(root_a, 1, callback)")),
            ("callback", Some("callback <- function(x) x")),
            ("unrelated", Some("unrelated <- function() 3")),
        ],
    )
    .exports(export("a"))
    .dynlibs(vec![NativeComponent {
        name: "root".into(),
        alias: String::new(),
        registration: Some(NativeRegistration {
            prefix: String::new(),
            suffix: String::new(),
        }),
        symbols: vec![NativeSymbolBinding {
            binding: "root_a".into(),
            symbol: "root_a".into(),
        }],
        library: NativeLibrary::Missing,
        safety: NativeSafety::Summarized(vec![NativeRoutineSummary {
            selector: "root_a".into(),
            callback_arguments: vec![2],
        }]),
    }])
    .build();
    let plan = analyze_images(vec![root_calling("dep", "a"), dep]);

    assert!(retained_binding(&plan, "dep", "callback"));
    assert!(!retained_binding(&plan, "dep", "unrelated"));
    let native = plan
        .provenance()
        .nodes()
        .iter()
        .find(|node| {
            node.package == "dep"
                && matches!(&node.kind, NodeKind::NativeComponent { name } if name == "root")
        })
        .unwrap()
        .id;
    let callback = plan.provenance().binding("dep", "callback").unwrap();
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
    let root = PackageFixture::new(
        "root",
        &[(
            "a",
            Some("a <- function() { callback <- function(x) x; .Call(root_a, callback) }"),
        )],
    )
    .exports(export("a"))
    .dynlibs(vec![NativeComponent {
        name: "root".into(),
        alias: String::new(),
        registration: Some(NativeRegistration {
            prefix: String::new(),
            suffix: String::new(),
        }),
        symbols: vec![NativeSymbolBinding {
            binding: "root_a".into(),
            symbol: "root_a".into(),
        }],
        library: NativeLibrary::Missing,
        safety: NativeSafety::Summarized(vec![NativeRoutineSummary {
            selector: "root_a".into(),
            callback_arguments: vec![1],
        }]),
    }])
    .build();
    let plan = analyze_images(vec![root]);

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
    let root = PackageFixture::new(
        "root",
        &[
            (
                "a",
                Some("a <- function() .Call(PACKAGE = \"root\", .NAME = root_a, 1, callback)"),
            ),
            ("callback", Some("callback <- function(x) x")),
        ],
    )
    .exports(export("a"))
    .dynlibs(vec![NativeComponent {
        name: "root".into(),
        alias: String::new(),
        registration: Some(NativeRegistration {
            prefix: String::new(),
            suffix: String::new(),
        }),
        symbols: vec![NativeSymbolBinding {
            binding: "root_a".into(),
            symbol: "root_a".into(),
        }],
        library: NativeLibrary::Missing,
        safety: NativeSafety::Summarized(vec![NativeRoutineSummary {
            selector: "root_a".into(),
            callback_arguments: vec![2],
        }]),
    }])
    .build();
    let plan = analyze_images(vec![root]);
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
    let dep = PackageFixture::new(
        "dep",
        &[
            ("a", Some("a <- function() 1")),
            ("callback", Some("callback <- function(x) x")),
        ],
    )
    .exports(export("a"))
    .dynlibs(vec![NativeComponent {
        name: "root".into(),
        alias: String::new(),
        registration: Some(NativeRegistration {
            prefix: String::new(),
            suffix: String::new(),
        }),
        symbols: vec![NativeSymbolBinding {
            binding: "root_a".into(),
            symbol: "root_a".into(),
        }],
        library: NativeLibrary::Missing,
        safety: NativeSafety::Summarized(vec![NativeRoutineSummary {
            selector: "root_a".into(),
            callback_arguments: vec![2],
        }]),
    }])
    .build();
    let plan = analyze_images(vec![root_calling("dep", "a"), dep]);
    assert!(!retained_binding(&plan, "dep", "callback"));
}

#[test]
fn missing_native_routine_summary_is_an_effect_blocker_not_lookup_failure() {
    let root = PackageFixture::new("root", &[("a", Some("a <- function() .Call(root_a, 1)"))])
        .exports(export("a"))
        .dynlibs(vec![NativeComponent {
            name: "root".into(),
            alias: String::new(),
            registration: Some(NativeRegistration {
                prefix: String::new(),
                suffix: String::new(),
            }),
            symbols: vec![NativeSymbolBinding {
                binding: "root_a".into(),
                symbol: "root_a".into(),
            }],
            library: NativeLibrary::Missing,
            safety: NativeSafety::Summarized(Vec::new()),
        }])
        .build();
    let plan = analyze_images(vec![root]);
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
    let foo = PackageFixture::new(
        "foo",
        &[
            ("a", Some("a <- function() 1")),
            ("callback", Some("callback <- function() 2")),
        ],
    )
    .exports(export("a"))
    .dynlibs(vec![NativeComponent {
        name: "foo".into(),
        alias: String::new(),
        registration: None,
        symbols: Vec::new(),
        library: NativeLibrary::Missing,
        safety: NativeSafety::Unsupported(vec!["dynamic R lookup".into()]),
    }])
    .build();
    let plan = analyze_images(vec![root, foo]);
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
fn dependency_activation_retains_registered_s3_methods() {
    let root = package("root", &[("f", Some("f <- function() foo::x()"))]);
    let foo = PackageFixture::new(
        "foo",
        &[
            ("x", Some("x <- function() 1")),
            ("print.foo", Some("print.foo <- function(x, ...) x")),
        ],
    )
    .exports(export("x"))
    .s3(vec![S3Registration {
        generic: slinker_core::package::GenericSpec {
            package: None,
            name: "print".into(),
        },
        class: "foo".into(),
        method: "print.foo".into(),
    }])
    .build();
    let plan = analyze_images(vec![root, foo]);
    assert!(retained_binding(&plan, "foo", "x"));
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
    let foo = PackageFixture::new("foo", &[])
        .files(vec!["data/x.json".into(), "data/y.json".into()])
        .build();
    let plan = analyze_images(vec![root, foo]);
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
    let linked = analyze_images(vec![root, foo]);
    assert!(linked.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f")
            && diagnostic.code == RejectCode::DynamicLookup
            && diagnostic.message.contains("system.file")
    }));
}

fn root_with_private_helper(main: &str) -> PackageImage {
    PackageFixture::new("root", &[
            ("main", Some(main)),
            (
                "helper",
                Some(
                    "helper <- function(x, package = 'root') system.file('data', package = package)"
                ),
            ),
        ]).exports(ExportMap::from([("main".into(), "main".into())])).build()
}

#[test]
fn defaulted_resource_package_is_static_only_when_no_invocation_supplies_it() {
    let foo = || package("foo", &[("h", Some("h <- function() 1"))]);
    let applied = root_with_private_helper("main <- function(xs) { foo::h(); lapply(xs, helper) }");
    let plan = analyze_images(vec![applied, foo()]);
    assert!(plan.blockers().is_empty());

    let supplied = root_with_private_helper(
        "main <- function(xs) { foo::h(); lapply(xs, helper); helper(1, 'foo') }",
    );
    let plan = analyze_images(vec![supplied, foo()]);
    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("helper")
            && diagnostic.code == RejectCode::DynamicLookup
            && diagnostic.message.contains("defaults to")
    }));

    let forwarded =
        root_with_private_helper("main <- function(xs, ...) { foo::h(); lapply(xs, helper, ...) }");
    let plan = analyze_images(vec![forwarded, foo()]);
    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("helper")
            && diagnostic.message.contains("defaults to")
    }));
}

#[test]
fn do_call_with_a_literal_list_is_a_typed_invocation() {
    let foo = || package("foo", &[("h", Some("h <- function() 1"))]);
    let defaults_hold = |call: &str| {
        let root = root_with_private_helper(&format!(
            "main <- function(args, ...) {{ foo::h(); {call} }}"
        ));
        let plan = analyze_images(vec![root, foo()]);
        plan.blockers().iter().all(|diagnostic| {
            diagnostic.binding.as_deref() != Some("helper")
                || !diagnostic.message.contains("defaults to")
        })
    };

    assert!(defaults_hold("do.call(helper, list(1))"));
    assert!(defaults_hold("do.call(helper, list(x = 1))"));
    assert!(!defaults_hold("do.call(helper, list(1, 'foo'))"));
    assert!(!defaults_hold("do.call(helper, list(1, package = 'foo'))"));
    assert!(!defaults_hold("do.call(helper, list(...))"));
    assert!(!defaults_hold("do.call(helper, args)"));
}

#[test]
fn base_resource_lookup_is_not_a_package_resource() {
    let root = package(
        "root",
        &[("f", Some("f <- function() system.file('DESCRIPTION')"))],
    );
    let plan = analyze_images(vec![root]);
    assert!(plan.blockers().is_empty());
    assert!(plan.program().resources().is_empty());
}

#[test]
fn synthetic_namespace_metadata_reads_block_for_linked_packages() {
    let analyze = |source: &str| {
        let root = package("root", &[("f", Some(source))]);
        let dep = package("dep", &[("x", Some("x <- function() 1"))]);
        analyze_images(vec![root, dep])
    };
    let blocked = |plan: &slinker_core::analysis::LinkIr| {
        plan.blockers().iter().any(|diagnostic| {
            diagnostic.code == RejectCode::UnsupportedRootTransformation
                && diagnostic.message.contains("synthetic")
        })
    };

    assert!(blocked(&analyze(
        "f <- function() { dep::x(); getNamespaceInfo('dep', 'path') }"
    )));
    for field in ["imports", "dynlibs", "S3methods"] {
        let source = format!("f <- function() {{ dep::x(); getNamespaceInfo('dep', '{field}') }}");
        assert!(!blocked(&analyze(&source)), "{field}");
    }
    assert!(!blocked(&analyze(
        "f <- function() { dep::x(); getNamespaceImports('dep') }"
    )));
    assert!(!blocked(&analyze(
        "f <- function() { dep::x(); getNamespaceInfo('dep', 'spec') }"
    )));
}

#[test]
fn linked_on_load_that_reads_libname_blocks() {
    let analyze = |on_load: &str| {
        let root = package("root", &[("f", Some("f <- function() dep::run()"))]);
        let mut dep = package(
            "dep",
            &[
                ("run", Some("run <- function() 1")),
                (".onLoad", Some(on_load)),
            ],
        );
        Arc::make_mut(&mut dep.index).lifecycle.on_load = true;
        analyze_images(vec![root, dep])
    };
    let blocked = |plan: &slinker_core::analysis::LinkIr| {
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnsupportedLinkedLibname)
    };

    assert!(blocked(&analyze(
        ".onLoad <- function(libname, pkgname) print(file.path(libname, pkgname))"
    )));
    assert!(!blocked(&analyze(
        ".onLoad <- function(libname, pkgname) print(pkgname)"
    )));
}

#[test]
fn activation_order_follows_lifecycle_dependencies_not_only_imports() {
    let root = package("root", &[("f", Some("f <- function() alpha::run()"))]);
    let mut alpha = package(
        "alpha",
        &[
            ("run", Some("run <- function() 1")),
            (
                ".onLoad",
                Some(".onLoad <- function(libname, pkgname) beta::setup()"),
            ),
        ],
    );
    Arc::make_mut(&mut alpha.index).lifecycle.on_load = true;
    let beta = package("beta", &[("setup", Some("setup <- function() 2"))]);
    let plan = analyze_images(vec![root, alpha, beta]);

    let order = plan
        .program()
        .activations()
        .iter()
        .map(|activation| {
            let package = plan
                .program()
                .package(plan.program().namespace(activation.namespace).package)
                .identity()
                .name
                .to_string();
            (package, activation.on_load.is_some())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        [("beta".to_owned(), false), ("alpha".to_owned(), true)]
    );
}

#[test]
fn finalization_does_not_depend_on_provenance() {
    let fixture = || {
        let root = PackageFixture::new(
            "root",
            &[
                ("f", Some("f <- function() alpha::run()")),
                ("g", Some("g <- function() ext:::secret(opened())")),
            ],
        )
        .imports(vec![ImportSpec::All {
            package: "ext".into(),
            except: Vec::new(),
        }])
        .exports(ExportMap::from([
            ("f".into(), "f".into()),
            ("g".into(), "g".into()),
        ]))
        .build();
        let mut alpha = package(
            "alpha",
            &[
                ("run", Some("run <- function() 1")),
                (
                    ".onLoad",
                    Some(".onLoad <- function(libname, pkgname) beta::setup()"),
                ),
            ],
        );
        Arc::make_mut(&mut alpha.index).lifecycle.on_load = true;
        let beta = package("beta", &[("setup", Some("setup <- function() 2"))]);
        let ext = PackageFixture::new(
            "ext",
            &[
                ("opened", Some("opened <- function() 3")),
                ("secret", Some("secret <- function(x) x")),
            ],
        )
        .exports(export("opened"))
        .build();
        vec![root, alpha, beta, ext]
    };
    let analyze = |provenance: bool| {
        let linker =
            Linker::new(FakeProvider::new(fixture()), 1).with_external_packages(["ext".to_owned()]);
        let linker = if provenance {
            linker
        } else {
            linker.without_provenance()
        };
        linker.analyze("root").unwrap()
    };
    let recorded = analyze(true);
    let unrecorded = analyze(false);

    assert!(!recorded.provenance().edges().is_empty());
    assert!(unrecorded.provenance().edges().is_empty());
    assert_eq!(
        format!("{:?}", recorded.program()),
        format!("{:?}", unrecorded.program())
    );
    assert_eq!(
        format!("{:?}", recorded.blockers()),
        format!("{:?}", unrecorded.blockers())
    );
}

#[test]
fn external_internal_access_is_preserved_in_the_program() {
    let root = package(
        "root",
        &[(
            "f",
            Some("f <- function() { foo:::hidden(); foo::shown() }"),
        )],
    );
    let foo = package(
        "foo",
        &[
            ("hidden", Some("hidden <- function() 1")),
            ("shown", Some("shown <- function() 2")),
        ],
    );
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_external_packages(["foo"])
        .analyze("root")
        .unwrap();

    let access = |name: &str| {
        plan.program()
            .bindings()
            .iter()
            .find_map(|binding| match &binding.state {
                slinker_core::ir::LinkBindingState::External { access, .. }
                    if binding.name == name =>
                {
                    Some(*access)
                }
                _ => None,
            })
            .expect("External binding in the program")
    };
    assert_eq!(
        access("hidden"),
        slinker_core::ir::ExternalBindingAccess::Internal
    );
    assert_eq!(
        access("shown"),
        slinker_core::ir::ExternalBindingAccess::Exported
    );
}

#[test]
fn find_package_of_a_linked_package_blocks_before_materialization() {
    let analyze = |source: &str| {
        let root = package_importing("root", &[("f", Some(source))], "foo");
        let foo = package("foo", &[("x", Some("x <- function() 1"))]);
        analyze_images(vec![root, foo, utils_platform()])
    };

    let located = analyze("f <- function() { foo::x(); find.package('foo') }");
    assert!(located.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::UnsupportedRootTransformation
            && diagnostic.message.contains("find.package")
    }));

    let versioned = analyze("f <- function() { foo::x(); utils::packageVersion('foo') }");
    assert!(
        versioned.blockers().is_empty(),
        "{:?}",
        versioned.blockers()
    );
}

#[test]
fn unresolved_name_blocks_only_where_retained_code_can_bind_it() {
    let analyze = |creator: Option<&str>| {
        let mut bindings = vec![("f", Some("f <- function() missing_everywhere()"))];
        bindings.extend(creator.map(|source| ("g", Some(source))));
        analyze_images(vec![package("root", &bindings)])
    };
    let unresolved = |plan: &slinker_core::analysis::LinkIr| {
        unresolved_name_evidence(plan, "missing_everywhere")
            .next()
            .is_some()
    };

    let alone = analyze(None);
    assert!(alone.blockers().is_empty(), "{:?}", alone.blockers());
    for creator in [
        "g <- function(name) assign(name, function() 1, envir = globalenv())",
        "g <- function() assign('missing_everywhere', function() 1, envir = globalenv())",
        "g <- function(values) list2env(values, globalenv())",
        "g <- function(f, e) { environment(f) <- e; f }",
    ] {
        assert!(unresolved(&analyze(Some(creator))), "{creator}");
    }
    let unrelated = analyze(Some(
        "g <- function() assign('other_name', function() 1, envir = globalenv())",
    ));
    assert!(
        unrelated.blockers().is_empty(),
        "{:?}",
        unrelated.blockers()
    );
}

#[test]
fn reflective_lookups_retain_static_names_and_block_dynamic_ones() {
    let analyze = |source: &str| {
        Linker::new(
            FakeProvider::new(vec![root_calling("dep", "f"), package("dep", &[
                ("f", Some(source)),
                ("helper", Some("helper <- function() 1")),
                ("helper_method.cls", Some("helper_method.cls <- function() 1")),
                ("register", Some("register <- function(generic, class) get(paste0(generic, '.', class))")),
                ("unload", Some("unload <- function() 1")),
            ])]),
            1,
        )
        .analyze("root")
        .unwrap()
    };

    let literal = analyze("f <- function() get('helper')()");
    assert!(retained_binding(&literal, "dep", "helper"));
    assert!(literal.blockers().is_empty(), "{:?}", literal.blockers());

    let dynamic = analyze("f <- function(name) get(name)");
    assert!(
        dynamic
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicLookup)
    );

    let specialized = analyze("f <- function() register('helper_method', 'cls')");
    assert!(retained_binding(&specialized, "dep", "helper_method.cls"));

    let finalizer = analyze(
        "f <- function() reg.finalizer(asNamespace('dep'), function(x) x$unload(), onexit = TRUE)",
    );
    assert!(retained_binding(&finalizer, "dep", "unload"));
}

#[test]
fn declared_strings_make_computed_names_exact() {
    let analyze = |source: &str| {
        Linker::new(
            FakeProvider::new(vec![
                root_calling("dep", "f"),
                PackageFixture::new(
                    "dep",
                    &[
                        ("f", Some(source)),
                        ("helper", Some("helper <- function() 1")),
                        ("other", Some("other <- function() 2")),
                        ("unused", Some("unused <- function() 3")),
                    ],
                )
                .exports(export("f"))
                .description(
                    "Imports: ext
",
                )
                .build(),
                package("ext", &[("x", Some("x <- function() 1"))]),
            ]),
            1,
        )
        .with_external_packages(source.contains("'ext'").then(|| "ext".to_owned()))
        .analyze("root")
        .unwrap()
    };

    let lookup = analyze(
        "f <- function(name) { declare(slinker(name = strings('helper', 'other'))); get(name)() }",
    );
    assert!(lookup.blockers().is_empty(), "{:?}", lookup.blockers());
    assert!(retained_binding(&lookup, "dep", "helper"));
    assert!(retained_binding(&lookup, "dep", "other"));
    assert!(!retained_binding(&lookup, "dep", "unused"));

    let narrowed = analyze(
        "f <- function(name) { declare(slinker(name = strings('helper', 'other'))); g <- function() { declare(slinker(name = strings('other'))); match.fun(name)() }; g() }",
    );
    assert!(narrowed.blockers().is_empty(), "{:?}", narrowed.blockers());
    assert!(retained_binding(&narrowed, "dep", "other"));
    assert!(!retained_binding(&narrowed, "dep", "helper"));

    let external =
        analyze("f <- function(pkg) { declare(slinker(pkg = strings('ext'))); asNamespace(pkg) }");
    assert!(external.blockers().is_empty(), "{:?}", external.blockers());

    let linked = analyze(
        "f <- function(pkg) { declare(slinker(pkg = strings('ext', 'dep'))); asNamespace(pkg) }",
    );
    assert!(
        linked.blockers().iter().any(|diagnostic| {
            diagnostic.code == RejectCode::DynamicPackageDiscovery
                && diagnostic.message.contains("can name Linked `dep`")
        }),
        "{:?}",
        linked.blockers()
    );

    let resource_external = analyze(
        "f <- function(pkg) { declare(slinker(pkg = strings('ext'))); system.file('x', package = pkg) }",
    );
    assert!(
        resource_external.blockers().is_empty(),
        "{:?}",
        resource_external.blockers()
    );

    let resource_linked = analyze(
        "f <- function(pkg) { declare(slinker(pkg = strings('dep'))); system.file('x', package = pkg) }",
    );
    assert!(
        resource_linked.blockers().iter().any(|diagnostic| {
            diagnostic.code == RejectCode::DynamicLookup
                && diagnostic.message.contains("can name Linked `dep`")
        }),
        "{:?}",
        resource_linked.blockers()
    );

    let computed_environment = analyze(
        "f <- function(name, env) { declare(slinker(name = strings('helper'))); get(name, envir = env) }",
    );
    assert!(
        computed_environment
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicLookup)
    );
}

#[test]
fn namespace_info_reads_accept_only_reproduced_fields() {
    let linked = |source: &str| {
        Linker::new(
            FakeProvider::new(vec![
                root_calling("dep", "f"),
                package("dep", &[("f", Some(source))]),
            ]),
            1,
        )
        .analyze("root")
        .unwrap()
        .blockers()
        .to_vec()
    };
    for accepted in [
        "f <- function() asNamespace('dep')$.__NAMESPACE__.$imports",
        "f <- function() .__NAMESPACE__.$dynlibs",
        "f <- function(ns) ns$.__NAMESPACE__.$S3methods",
        "f <- function() asNamespace('dep')$.__NAMESPACE__.$exports",
        "f <- function() .__NAMESPACE__.$exports",
        "f <- function() .__NAMESPACE__.$spec",
        "f <- function(ns) ns[['.__NAMESPACE__.']][['exports']]",
    ] {
        let blockers = linked(accepted);
        assert!(blockers.is_empty(), "{accepted}: {blockers:?}");
    }
    for blocked in [
        "f <- function() asNamespace('dep')$.__NAMESPACE__.$path",
        "f <- function() .__NAMESPACE__.$path",
        "f <- function(ns) ns[['.__NAMESPACE__.']]$path",
    ] {
        let blockers = linked(blocked);
        assert!(!blockers.is_empty(), "{blocked}");
    }

    let root_only = Linker::new(
        FakeProvider::new(vec![package(
            "root",
            &[("f", Some("f <- function(ns) ns$.__NAMESPACE__.$imports"))],
        )]),
        1,
    )
    .analyze("root")
    .unwrap();
    assert!(
        root_only.blockers().is_empty(),
        "{:?}",
        root_only.blockers()
    );
}

#[test]
fn dynamic_namespace_is_allowed_only_without_reflection() {
    let blocked = |source: &str| {
        let plan = Linker::new(
            FakeProvider::new(vec![package("root", &[("f", Some(source))])]),
            1,
        )
        .analyze("root")
        .unwrap();
        plan.blockers().iter().any(|diagnostic| {
            matches!(
                diagnostic.code,
                RejectCode::DynamicPackageDiscovery | RejectCode::DynamicLookup
            )
        })
    };

    assert!(!blocked(
        "f <- function(pkg, fun) registerS3method('g', 'c', fun, envir = asNamespace(pkg))"
    ));
    assert!(blocked(
        "f <- function(ns) asNamespace(ns)$.__NAMESPACE__.$exports"
    ));
    assert!(!blocked(
        "f <- function(ns, name) exists(name, envir = asNamespace(ns), inherits = FALSE)"
    ));
    assert!(blocked(
        "f <- function(ns, name) exists(name, envir = asNamespace(ns))"
    ));
}

#[test]
fn construction_interpreter_does_not_reevaluate_unspecialized_calls() {
    let sources = (0..18)
        .map(|level| {
            let next = format!("f{}", level + 1);
            (
                format!("f{level}"),
                format!("f{level} <- function() {{ {next}(); {next}(); {next}() }}"),
            )
        })
        .chain(std::iter::once((
            "f18".to_owned(),
            "f18 <- function() 1".to_owned(),
        )))
        .collect::<Vec<_>>();
    let bindings = sources
        .iter()
        .map(|(name, source)| (name.as_str(), Some(source.as_str())))
        .collect::<Vec<_>>();
    let plan = analyze_images(vec![package("root", &bindings)]);

    assert!(retained_binding(&plan, "root", "f18"));
}

#[test]
fn construction_interpreter_evaluates_each_call_signature_once_per_requester() {
    let levels = 10;
    let sources = (0..levels)
        .map(|level| {
            let next = format!("f{}", level + 1);
            (
                format!("f{level}"),
                format!(
                    "f{level} <- function(x, flag) {{ if (flag) {next}(\"a\", flag) else {next}(\"a\", flag); {next}(\"a\", flag) }}"
                ),
            )
        })
        .chain(std::iter::once((
            format!("f{levels}"),
            format!("f{levels} <- function(x, flag) x"),
        )))
        .collect::<Vec<_>>();
    let bindings = sources
        .iter()
        .map(|(name, source)| (name.as_str(), Some(source.as_str())))
        .collect::<Vec<_>>();
    let plan = analyze_images(vec![package("root", &bindings)]);

    assert!(retained_binding(&plan, "root", &format!("f{levels}")));
    let requesters = bindings.len();
    let signatures = bindings.len();
    assert!(
        plan.construction_evaluations() <= requesters * signatures,
        "{} construction evaluations for {requesters} bindings",
        plan.construction_evaluations()
    );
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
fn air_frontend_failure_is_localized_not_package_fatal() {
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
fn air_accepted_unknown_name_is_not_a_frontend_error() {
    let root = package("root", &[("f", Some("f <- function() missing_symbol()"))]);
    let plan = analyze_images(vec![root]);

    assert!(plan.blockers().is_empty(), "{:?}", plan.blockers());
}

#[test]
fn air_and_target_rejection_is_invalid_installed_representation() {
    let root = package("root", &[("awkward", Some("awkward <- function() {"))]);
    let provider = FakeProvider::new(vec![root])
        .validation(SyntaxValidation::Rejected("unexpected end of input".into()));
    let plan = link(provider);

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
    let foo = PackageFixture::new("foo", &[("bar", Some("bar <- function() 1"))])
        .exports(export("bar"))
        .build();
    let plan = analyze_images(vec![root, foo]);
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
    let foo = PackageFixture::new("foo", &[("x", Some("x <- function() 1"))])
        .imports(vec![ImportSpec::From {
            package: "otelsdk".into(),
            bindings: vec![ImportBinding {
                local: "span".into(),
                remote: "span".into(),
            }],
        }])
        .exports(export("x"))
        .build();
    let plan = analyze_images(vec![root, foo]);
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
    let foo = PackageFixture::new("foo", &[("x", Some("x <- function() span()"))])
        .imports(vec![ImportSpec::From {
            package: "otelsdk".into(),
            bindings: vec![ImportBinding {
                local: "span".into(),
                remote: "span".into(),
            }],
        }])
        .exports(export("x"))
        .build();
    let plan = analyze_images(vec![root, foo]);
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
    let root = PackageFixture::new("root", &[("f", Some("f <- function() 1"))])
        .imports(vec![ImportSpec::All {
            package: "required_at_root_load".into(),
            except: Vec::new(),
        }])
        .exports(export("f"))
        .build();
    let plan = analyze_images(vec![root]);
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
    let foo = PackageFixture::new("foo", &[]).build();
    let plan = analyze_images(vec![root, foo]);
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
    let foo = PackageFixture::new("foo", &[]).build();
    let plan = analyze_images(vec![root, foo]);
    assert!(
        plan.blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::MissingResource)
    );
}

#[test]
fn suggests_alone_never_enters_the_graph() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function() 1"))])
        .exports(export("f"))
        .description("Suggests: foo\n")
        .build();
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = link(provider);
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
    let root = PackageFixture::new("root", &[("f", Some("f <- function() 1"))])
        .exports(export("f"))
        .description("Suggests: foo\n")
        .build();
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 1)
        .with_linked_packages(["foo".to_owned()])
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
    let root = PackageFixture::new("root", &[("f", Some("f <- function() imported_bar()"))])
        .imports(vec![ImportSpec::From {
            package: "foo".into(),
            bindings: vec![ImportBinding {
                local: "imported_bar".into(),
                remote: "bar".into(),
            }],
        }])
        .exports(export("f"))
        .description("Suggests: foo\n")
        .build();
    let foo = PackageFixture::new(
        "foo",
        &[
            ("bar", Some("bar <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
    )
    .exports(export("bar"))
    .build();
    let plan = analyze_images(vec![root, foo]);

    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn config_needs_does_not_enable_a_suggested_runtime_package() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function() foo::bar()"))])
        .exports(export("f"))
        .description("Suggests: foo\nConfig/Needs/website: foo, bar\n")
        .build();
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let locate_counts = provider.optional_locate_count_handle();
    let plan = link(provider);

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
        let root = PackageFixture::new("root", &[("f", Some("f <- function() foo::bar()"))])
            .exports(export("f"))
            .description(format!("{required_field}: foo\nSuggests: foo\n"))
            .build();
        let foo = PackageFixture::new(
            "foo",
            &[
                ("bar", Some("bar <- function() 1")),
                ("unused", Some("unused <- function() 2")),
            ],
        )
        .exports(export("bar"))
        .build();
        let plan = analyze_images(vec![root, foo]);

        assert!(
            retained_binding(&plan, "foo", "bar"),
            "{required_field} should make foo required when source uses it"
        );
        assert!(!retained_binding(&plan, "foo", "unused"));
    }
}

#[test]
fn direct_suggested_namespace_access_is_ignored_without_link() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function() foo::bar()"))])
        .exports(export("f"))
        .description("Suggests: foo\n")
        .build();
    let foo = PackageFixture::new(
        "foo",
        &[
            ("bar", Some("bar <- function() helper()")),
            ("helper", Some("helper <- function() 1")),
        ],
    )
    .exports(export("bar"))
    .build();
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let locate_counts = provider.optional_locate_count_handle();
    let plan = link(provider);

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
            .any(|relocation| matches!(
                relocation.target,
                slinker_core::ir::RelocationTarget::Binding { .. }
            ))
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
    let root = PackageFixture::new("root", &[("f", Some("f <- function() foo::bar()"))])
        .exports(export("f"))
        .description("Suggests: foo\n")
        .build();
    let foo = PackageFixture::new(
        "foo",
        &[
            ("bar", Some("bar <- function() helper()")),
            ("helper", Some("helper <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
    )
    .exports(export("bar"))
    .build();
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_linked_packages(["foo".to_owned()])
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(retained_binding(&plan, "foo", "helper"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn selecting_one_extra_does_not_enable_its_suggests() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function() foo::bar()"))])
        .exports(export("f"))
        .description("Suggests: foo\n")
        .build();
    let foo = PackageFixture::new("foo", &[("bar", Some("bar <- function() baz::qux()"))])
        .exports(export("bar"))
        .description("Suggests: baz\n")
        .build();
    let baz = package("baz", &[("qux", Some("qux <- function() 1"))]);
    let provider = FakeProvider::new(vec![root, foo, baz]);
    let counts = provider.count_handle();
    let locate_counts = provider.optional_locate_count_handle();
    let plan = Linker::new(provider, 2)
        .with_linked_packages(["foo".to_owned()])
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
    let cli = PackageFixture::new(
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
    )
    .exports(export("cli_alert"))
    .description("Suggests:\n    knitr,\n    testthat,\n    rmarkdown\n")
    .build();
    let knitr = package("knitr", &[("knit", Some("knit <- function() 1"))]);
    let testthat = package(
        "testthat",
        &[("test_that", Some("test_that <- function(...) 1"))],
    );
    let rmarkdown = package(
        "rmarkdown",
        &[("render", Some("render <- function(...) 1"))],
    );
    let provider = FakeProvider::new(vec![
        root_calling("cli", "cli_alert"),
        cli,
        knitr,
        testthat,
        rmarkdown,
    ]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 4).analyze("root").unwrap();

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
    let root = PackageFixture::new(
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
    )
    .imports(vec![ImportSpec::From {
        package: "utils".into(),
        bindings: vec![ImportBinding {
            local: "head".into(),
            remote: "head".into(),
        }],
    }])
    .exports(export("cli_head"))
    .description("Imports:\n    utils\nSuggests:\n    knitr,\n    rlang,\n    testthat\n")
    .build();
    let utils = PackageFixture::new(
        "utils",
        &[
            ("head", Some("head <- function(x) x")),
            ("unused", Some("unused <- function() 1")),
        ],
    )
    .exports(export("head"))
    .build();
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
    let root = PackageFixture::new(
        "root",
        &[(
            "f",
            Some("f <- function() system.file('data', 'x.json', package = 'foo')"),
        )],
    )
    .exports(export("f"))
    .description("Suggests: foo\n")
    .build();
    let foo = PackageFixture::new("foo", &[])
        .files(vec!["data/x.json".into()])
        .build();
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = link(provider);

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
            .any(|relocation| matches!(
                relocation.target,
                slinker_core::ir::RelocationTarget::Resource { .. }
            ))
    );
    assert_eq!(counts.lock().unwrap().get("foo").copied().unwrap_or(0), 0);
}

#[test]
fn unselected_suggested_attachment_call_is_ignored() {
    let root = PackageFixture::new("root", &[("f", Some("f <- function() require(foo)"))])
        .exports(export("f"))
        .description("Suggests: foo\n")
        .build();
    let foo = package("foo", &[("bar", Some("bar <- function() 1"))]);
    let plan = analyze_images(vec![root, foo]);

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
fn closed_generic_retains_every_registered_and_lexical_method() {
    let root = package("root", &[("f", Some("f <- function(x) foo::criterion(x)"))]);
    let foo = PackageFixture::new(
        "foo",
        &[
            (
                "criterion",
                Some("criterion <- function(x) UseMethod(\"criterion\")"),
            ),
            (
                "criterion.character",
                Some("criterion.character <- function(x) helper(x)"),
            ),
            (
                "criterion.default",
                Some("criterion.default <- function(x) x"),
            ),
            ("as_criterion", Some("as_criterion <- function(x) x")),
            ("helper", Some("helper <- function(x) x")),
            ("unrelated", Some("unrelated <- function() 1")),
        ],
    )
    .exports(export("criterion"))
    .s3(vec![S3Registration {
        generic: slinker_core::package::GenericSpec {
            package: None,
            name: "criterion".into(),
        },
        class: "root_criterion".into(),
        method: "as_criterion".into(),
    }])
    .build();
    let plan = analyze_images(vec![root, foo]);

    assert!(plan.blockers().is_empty(), "{:?}", plan.blockers());
    for method in [
        "criterion.character",
        "criterion.default",
        "as_criterion",
        "helper",
    ] {
        assert!(retained_binding(&plan, "foo", method), "{method}");
    }
    assert!(!retained_binding(&plan, "foo", "unrelated"));
}

#[test]
fn declared_generic_names_dispatch_each_generic() {
    let analyze = |dispatch: &str| {
        let root = package(
            "root",
            &[("f", Some("f <- function() foo::dispatch(1, 'alpha')"))],
        );
        let foo = package(
            "foo",
            &[
                ("dispatch", Some(dispatch)),
                (
                    "alpha.default",
                    Some("alpha.default <- function(x, kind) 1"),
                ),
                ("beta.default", Some("beta.default <- function(x, kind) 2")),
                (
                    "gamma.default",
                    Some("gamma.default <- function(x, kind) 3"),
                ),
            ],
        );
        analyze_images(vec![root, foo])
    };

    let declared = analyze(
        "dispatch <- function(x, kind) { declare(slinker(kind = strings('alpha', 'beta'))); UseMethod(kind) }",
    );
    assert!(declared.blockers().is_empty(), "{:?}", declared.blockers());
    assert!(retained_binding(&declared, "foo", "alpha.default"));
    assert!(retained_binding(&declared, "foo", "beta.default"));
    assert!(!retained_binding(&declared, "foo", "gamma.default"));

    let root = package(
        "root",
        &[(
            "first",
            Some(
                "first <- function(x) { declare(slinker(x = s3('c1'))); foo::dispatch(x, 'alpha'); foo::chain1() }",
            ),
        )],
    );
    let methods = ["alpha.c1", "alpha.c2", "beta.c1", "beta.c2", "alpha.c3"];
    let mut sources = vec![
        "dispatch <- function(x, kind) { declare(slinker(kind = strings('alpha', 'beta'))); UseMethod(kind) }".to_owned(),
        "chain1 <- function() chain2()".to_owned(),
        "chain2 <- function() chain3()".to_owned(),
        "chain3 <- function() late(NULL)".to_owned(),
        "late <- function(y) { declare(slinker(y = s3('c2'))); dispatch(y, 'beta') }".to_owned(),
    ];
    sources.extend(methods.map(|method| format!("{method} <- function(x, kind) 1")));
    let bindings = sources
        .iter()
        .map(|source| {
            (
                source.split(' ').next().expect("binding name"),
                Some(source.as_str()),
            )
        })
        .collect::<Vec<_>>();
    let closed = analyze_images(vec![root, package("foo", &bindings)]);
    assert!(closed.blockers().is_empty(), "{:?}", closed.blockers());
    for method in ["alpha.c1", "alpha.c2", "beta.c1", "beta.c2"] {
        assert!(retained_binding(&closed, "foo", method), "{method}");
    }
    assert!(!retained_binding(&closed, "foo", "alpha.c3"));

    let undeclared = analyze("dispatch <- function(x, kind) UseMethod(kind)");
    assert!(
        undeclared
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::ObjectSystem)
    );
}

#[test]
fn declared_receiver_class_narrows_generic_methods() {
    let analyze = |caller: &str| {
        let root = package("root", &[("f", Some(caller))]);
        let foo = PackageFixture::new(
            "foo",
            &[
                (
                    "criterion",
                    Some("criterion <- function(x) UseMethod(\"criterion\")"),
                ),
                (
                    "criterion.character",
                    Some("criterion.character <- function(x) helper(x)"),
                ),
                (
                    "criterion.root_criterion",
                    Some("criterion.root_criterion <- function(x) special(x)"),
                ),
                (
                    "criterion.default",
                    Some("criterion.default <- function(x) x"),
                ),
                ("helper", Some("helper <- function(x) x")),
                ("special", Some("special <- function(x) x")),
            ],
        )
        .exports(export("criterion"))
        .build();
        analyze_images(vec![root, foo])
    };

    let declared = analyze(
        "f <- function(x) { declare(slinker(x = s3(\"root_criterion\"))); foo::criterion(x) }",
    );
    assert!(declared.blockers().is_empty(), "{:?}", declared.blockers());
    for retained in ["criterion.root_criterion", "special", "criterion.default"] {
        assert!(retained_binding(&declared, "foo", retained), "{retained}");
    }
    for pruned in ["criterion.character", "helper"] {
        assert!(!retained_binding(&declared, "foo", pruned), "{pruned}");
    }

    let undeclared = analyze("f <- function(x) foo::criterion(x)");
    assert!(retained_binding(&undeclared, "foo", "criterion.character"));

    let escaped = analyze(
        "f <- function(x) { declare(slinker(x = s3(\"root_criterion\"))); lapply(list(x), foo::criterion) }",
    );
    assert!(retained_binding(&escaped, "foo", "criterion.character"));
}

#[test]
fn external_method_registration_opens_a_closed_generic() {
    let root = package(
        "root",
        &[
            ("f", Some("f <- function(x) { bar::g(); criterion(x) }")),
            (
                "criterion",
                Some("criterion <- function(x) UseMethod(\"criterion\")"),
            ),
        ],
    );
    let bar = PackageFixture::new(
        "bar",
        &[
            ("g", Some("g <- function() 1")),
            ("criterion.bar", Some("criterion.bar <- function(x) 1")),
        ],
    )
    .exports(export("g"))
    .s3(vec![S3Registration {
        generic: slinker_core::package::GenericSpec {
            package: Some("root".into()),
            name: "criterion".into(),
        },
        class: "bar".into(),
        method: "criterion.bar".into(),
    }])
    .build();
    let plan = Linker::new(FakeProvider::new(vec![root, bar]), 1)
        .with_external_packages(["bar"])
        .analyze("root")
        .unwrap();

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.code == RejectCode::ObjectSystem
            && diagnostic.message.contains("External package `bar`")
    }));
}

#[test]
fn next_method_is_supported_only_inside_a_closed_method_set() {
    let root = package(
        "root",
        &[
            ("f", Some("f <- function(x) { criterion(x); stray(x) }")),
            (
                "criterion",
                Some("criterion <- function(x) UseMethod(\"criterion\")"),
            ),
            (
                "criterion.child",
                Some("criterion.child <- function(x) NextMethod()"),
            ),
            (
                "criterion.default",
                Some("criterion.default <- function(x) x"),
            ),
            ("stray", Some("stray <- function(x) NextMethod()")),
        ],
    );
    let plan = analyze_images(vec![root]);

    let next_method_blockers = plan
        .blockers()
        .iter()
        .filter(|diagnostic| diagnostic.message.contains("NextMethod"))
        .map(|diagnostic| diagnostic.binding.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(next_method_blockers, [Some("stray")]);
}

#[test]
fn registered_operator_method_is_retained_with_its_dependencies() {
    let mut root = PackageFixture::new(
        "root",
        &[
            ("f", Some("f <- function(other) criterion | other")),
            ("criterion", None),
            (
                "|.root_criterion",
                Some("`|.root_criterion` <- function(e1, e2) is_root_criterion(e2)"),
            ),
            (
                "is_root_criterion",
                Some("is_root_criterion <- function(x) TRUE"),
            ),
        ],
    )
    .exports(export("f"))
    .s3(vec![S3Registration {
        generic: slinker_core::package::GenericSpec {
            package: None,
            name: "|".into(),
        },
        class: "root_criterion".into(),
        method: "|.root_criterion".into(),
    }])
    .build();
    Arc::make_mut(
        root.bindings
            .get_mut("criterion")
            .expect("criterion binding"),
    )
    .object
    .classes = vec!["root_criterion".into()];

    let plan = analyze_images(vec![root]);

    assert!(plan.blockers().is_empty(), "{:?}", plan.blockers());
    assert!(retained_binding(&plan, "root", "|.root_criterion"));
    assert!(retained_binding(&plan, "root", "is_root_criterion"));
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
fn unselected_suggested_s3_generic_keeps_a_delayed_registration_without_inspecting_it() {
    let root = PackageFixture::new(
        "root",
        &[
            ("public", Some("public <- function() 1")),
            ("print.foo", Some("print.foo <- function(x, ...) x")),
        ],
    )
    .exports(export("public"))
    .s3(vec![S3Registration {
        generic: slinker_core::package::GenericSpec {
            package: Some("foo".into()),
            name: "print".into(),
        },
        class: "foo".into(),
        method: "print.foo".into(),
    }])
    .description("Suggests: foo\n")
    .build();
    let foo = package("foo", &[("print", Some("print <- function(x, ...) x"))]);
    let provider = FakeProvider::new(vec![root, foo]);
    let counts = provider.count_handle();
    let plan = link(provider);

    assert!(program_has_s3_registration(
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
fn retained_dependency_method_keeps_its_delayed_registration_on_an_unselected_generic() {
    let root = package("root", &[("f", Some("f <- function() dep::method()"))]);
    let dep = PackageFixture::new(
        "dep",
        &[("method", Some("method <- function(x = NULL, ...) x"))],
    )
    .exports(export("method"))
    .s3(vec![S3Registration {
        generic: slinker_core::package::GenericSpec {
            package: Some("foo".into()),
            name: "generic".into(),
        },
        class: "dep_class".into(),
        method: "method".into(),
    }])
    .description("Suggests: foo\n")
    .build();
    let foo = package("foo", &[("generic", Some("generic <- function(x, ...) x"))]);
    let provider = FakeProvider::new(vec![root, dep, foo]);
    let counts = provider.count_handle();
    let plan = Linker::new(provider, 2).analyze("root").unwrap();

    assert!(retained_binding(&plan, "dep", "method"));
    assert!(program_has_s3_registration(
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
    let dep = PackageFixture::new(
        "dep",
        &[("method", Some("method <- function(x = NULL, ...) x"))],
    )
    .exports(export("method"))
    .s3(vec![S3Registration {
        generic: slinker_core::package::GenericSpec {
            package: Some("foo".into()),
            name: "generic".into(),
        },
        class: "dep_class".into(),
        method: "method".into(),
    }])
    .description("Suggests: foo\n")
    .build();
    let foo = package("foo", &[("generic", Some("generic <- function(x, ...) x"))]);
    let plan = Linker::new(FakeProvider::new(vec![root, dep, foo]), 2)
        .with_linked_packages(["foo".to_owned()])
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

fn suggesting_root(source: &str) -> PackageImage {
    PackageFixture::new("root", &[("f", Some(source))])
        .exports(export("f"))
        .description("Suggests: foo\n")
        .build()
}

fn optional_foo() -> PackageImage {
    PackageFixture::new(
        "foo",
        &[
            ("bar", Some("bar <- function() hidden()")),
            ("hidden", Some("hidden <- function() 1")),
        ],
    )
    .exports(export("bar"))
    .build()
}

fn optional_availability_blockers(plan: &slinker_core::analysis::LinkIr) -> Vec<String> {
    plan.blockers()
        .iter()
        .filter(|diagnostic| diagnostic.code == RejectCode::OptionalAvailability)
        .map(|diagnostic| {
            format!(
                "{}::{}",
                diagnostic.package,
                diagnostic.binding.as_deref().unwrap_or("")
            )
        })
        .collect()
}

#[test]
fn unselected_suggested_availability_guard_blocks_instead_of_freezing() {
    let provider = FakeProvider::new(vec![
        suggesting_root(
            "f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()",
        ),
        optional_foo(),
    ]);
    let counts = provider.count_handle();
    let plan = link(provider);

    assert_eq!(optional_availability_blockers(&plan), ["root::f"]);
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
fn unselected_suggested_availability_query_blocks_without_a_guard() {
    let plan = Linker::new(
        FakeProvider::new(vec![
            suggesting_root("f <- function() requireNamespace(\"foo\", quietly = TRUE)"),
            optional_foo(),
        ]),
        1,
    )
    .analyze("root")
    .unwrap();

    assert_eq!(optional_availability_blockers(&plan), ["root::f"]);
}

#[test]
fn unselected_suggested_loaded_guard_blocks_the_pruned_branch() {
    let plan = Linker::new(
        FakeProvider::new(vec![
            suggesting_root("f <- function() if (isNamespaceLoaded(\"foo\")) foo::bar()"),
            optional_foo(),
        ]),
        1,
    )
    .analyze("root")
    .unwrap();

    assert_eq!(optional_availability_blockers(&plan), ["root::f"]);
}

#[test]
fn unselected_suggested_loaded_query_alone_is_not_environment_frozen() {
    let plan = Linker::new(
        FakeProvider::new(vec![
            suggesting_root("f <- function() isNamespaceLoaded(\"foo\")"),
            optional_foo(),
        ]),
        1,
    )
    .analyze("root")
    .unwrap();

    assert!(plan.blockers().is_empty());
}

#[test]
fn unused_suggests_entry_neither_blocks_nor_links() {
    let plan = Linker::new(
        FakeProvider::new(vec![suggesting_root("f <- function() 1"), optional_foo()]),
        1,
    )
    .analyze("root")
    .unwrap();

    assert!(plan.blockers().is_empty());
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo")
    );
}

#[test]
fn selected_optional_package_follows_the_supported_guard_semantics() {
    let source = "f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()";
    let extra = Linker::new(
        FakeProvider::new(vec![suggesting_root(source), optional_foo()]),
        1,
    )
    .with_linked_packages(["foo".to_owned()])
    .analyze("root")
    .unwrap();
    let external = Linker::new(
        FakeProvider::new(vec![suggesting_root(source), optional_foo()]),
        1,
    )
    .with_external_packages(["foo".to_owned()])
    .analyze("root")
    .unwrap();

    assert!(extra.blockers().is_empty());
    assert!(retained_binding(&extra, "foo", "bar"));
    assert!(external.blockers().is_empty());
    assert!(
        !external
            .provenance()
            .nodes()
            .iter()
            .any(|node| node.package == "foo" && matches!(node.kind, NodeKind::Binding { .. }))
    );
}

#[test]
fn required_package_guard_is_not_an_optional_availability_blocker() {
    let root = PackageFixture::new(
        "root",
        &[(
            "f",
            Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()"),
        )],
    )
    .exports(export("f"))
    .description("Imports: foo\n")
    .build();
    let plan = analyze_images(vec![root, optional_foo()]);

    assert!(plan.blockers().is_empty());
    assert!(retained_binding(&plan, "foo", "bar"));
}

#[test]
fn selected_extra_enables_guarded_optional_branch_without_rooting_whole_package() {
    let root = PackageFixture::new(
        "root",
        &[(
            "f",
            Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()"),
        )],
    )
    .exports(export("f"))
    .description("Suggests: foo\n")
    .build();
    let foo = PackageFixture::new(
        "foo",
        &[
            ("bar", Some("bar <- function() helper()")),
            ("helper", Some("helper <- function() 1")),
            ("unused", Some("unused <- function() 2")),
        ],
    )
    .exports(export("bar"))
    .build();
    let plan = Linker::new(FakeProvider::new(vec![root, foo]), 1)
        .with_linked_packages(["foo".to_owned()])
        .analyze("root")
        .unwrap();
    assert!(retained_binding(&plan, "foo", "bar"));
    assert!(retained_binding(&plan, "foo", "helper"));
    assert!(!retained_binding(&plan, "foo", "unused"));
}

#[test]
fn selected_missing_extra_is_reported_as_missing_dependency() {
    let root = PackageFixture::new(
        "root",
        &[(
            "f",
            Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar()"),
        )],
    )
    .exports(export("f"))
    .description("Suggests: foo\n")
    .build();
    let plan = Linker::new(FakeProvider::new(vec![root]), 1)
        .with_linked_packages(["foo".to_owned()])
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
    let mut glue = PackageFixture::new("glue", &[
            ("glue", Some("glue <- function(x) x")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(...) { if (isNamespaceLoaded(\"knitr\") && \"knit_engines\" %in% getNamespaceExports(\"knitr\")) { knitr::knit_engines$set(glue = glue) } else { setHook(packageEvent(\"knitr\", \"onLoad\"), function(...) knitr::knit_engines$set(glue = glue)) } }",
                ),
            ),
        ]).exports(export("glue")).description("Suggests: knitr\n").build();
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
    assert_eq!(optional_availability_blockers(&plan), ["glue::.onLoad"]);
}

#[test]
fn selected_extra_enables_optional_onload_hook_namespace() {
    let root = package("root", &[("f", Some("f <- function() glue::glue(\"x\")"))]);
    let mut glue = PackageFixture::new("glue", &[
            ("glue", Some("glue <- function(x) x")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(...) { if (isNamespaceLoaded(\"knitr\")) knitr::knit_engines$set(glue = glue) else setHook(packageEvent(\"knitr\", \"onLoad\"), function(...) knitr::knit_engines$set(glue = glue)) }",
                ),
            ),
        ]).exports(export("glue")).description("Suggests: knitr\n").build();
    Arc::make_mut(&mut glue.index).lifecycle.on_load = true;
    let knitr = PackageFixture::new(
        "knitr",
        &[
            ("knit_engines", None),
            ("unused", Some("unused <- function() 1")),
        ],
    )
    .exports(export("knit_engines"))
    .build();
    let plan = Linker::new(FakeProvider::new(vec![root, glue, knitr]), 4)
        .with_linked_packages(["knitr".to_owned()])
        .analyze("root")
        .unwrap();
    assert!(optional_availability_blockers(&plan).is_empty());
    assert!(retained_binding(&plan, "knitr", "knit_engines"));
    assert!(!retained_binding(&plan, "knitr", "unused"));
}

#[test]
fn external_namespace_is_not_assumed_loaded_for_onload_guard() {
    let root = package("root", &[("f", Some("f <- function() glue::glue(\"x\")"))]);
    let mut glue = PackageFixture::new("glue", &[
            ("glue", Some("glue <- function(x) x")),
            (
                ".onLoad",
                Some(
                    ".onLoad <- function(...) if (isNamespaceLoaded(\"knitr\")) knitr::knit_engines$set(glue = glue)",
                ),
            ),
        ]).exports(export("glue")).description("Suggests: knitr\n").build();
    Arc::make_mut(&mut glue.index).lifecycle.on_load = true;
    let knitr = PackageFixture::new("knitr", &[("knit_engines", None)])
        .exports(export("knit_engines"))
        .description("Priority: base\n")
        .build();
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
    let plan = analyze_images(vec![root, foo]);

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
    let foo = PackageFixture::new("foo", &[])
        .files(vec!["data".into()])
        .build();
    let plan = analyze_images(vec![root, foo]);

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
    let plan = analyze_images(vec![root]);

    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f") && diagnostic.message.contains("tvalue")
    }));
}

#[test]
fn non_returning_package_helper_refines_exhaustive_dispatch() {
    let root = PackageFixture::new(
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
    )
    .exports(export("f"))
    .build();
    let plan = analyze_images(vec![root]);

    assert!(retained_binding(&plan, "root", ".stop_invalid_direction"));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f") && diagnostic.message.contains("showprob")
    }));
}

#[test]
fn non_returning_summary_does_not_hide_real_invalid_input_fallthrough() {
    let root = package(
        "root",
        &[
            (
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
            ),
            ("pvalue", None),
        ],
    );
    let plan = analyze_images(vec![root]);

    assert!(plan.blockers().is_empty(), "{:?}", plan.blockers());
    assert!(retained_binding(&plan, "root", "pvalue"));
}

#[test]
fn conditional_local_fallthrough_retains_an_enclosing_binding_only_when_one_exists() {
    let source = "f <- function(flag) { if (flag) x <- 1; x }";
    let analyze =
        |bindings: &[(&str, Option<&str>)]| analyze_images(vec![package("root", bindings)]);

    let standalone = analyze(&[("f", Some(source))]);
    assert!(
        standalone.blockers().is_empty(),
        "{:?}",
        standalone.blockers()
    );

    let enclosed = analyze(&[("f", Some(source)), ("x", None)]);
    assert!(enclosed.blockers().is_empty(), "{:?}", enclosed.blockers());
    assert!(retained_binding(&enclosed, "root", "x"));
}

#[test]
fn later_formal_default_does_not_escape_to_package_resolution() {
    let root = package("root", &[("f", Some("f <- function(x = y, y = 1) x"))]);
    let plan = analyze_images(vec![root]);

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
    let plan = analyze_images(vec![root]);

    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("f") && diagnostic.message.contains("`x`")
    }));
}

#[test]
fn for_induction_variable_after_loop_keeps_zero_iteration_fallthrough() {
    let root = package(
        "root",
        &[
            (
                "f",
                Some("f <- function(xs) { for (x in xs) {}; print(x) }"),
            ),
            ("x", None),
        ],
    );
    let plan = analyze_images(vec![root]);

    assert!(retained_binding(&plan, "root", "x"));
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
    let plan = analyze_images(vec![root]);

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
    let plan = analyze_images(vec![root]);

    assert!(plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("outer")
            && diagnostic.code == RejectCode::EnvironmentMutation
            && diagnostic.message.contains("`x`")
    }));
}

#[test]
fn private_non_returning_helper_refines_enclosing_private_closure() {
    let mut root = PackageFixture::new("root", &[(
            "public",
            Some(
                "public <- function(direction) { if (direction == 'ok') value <- 1 else .die(); value }",
            ),
        )]).exports(export("public")).build();
    Arc::make_mut(root.bindings.get_mut("public").unwrap())
        .object
        .closure
        .as_mut()
        .unwrap()
        .environment = "private:1".into();
    root.private_environments.insert(
        "private:1".into(),
        Arc::new(PrivateEnvironmentImage {
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
        }),
    );

    let plan = analyze_images(vec![root]);

    assert!(retained_private_binding(&plan, "root", "private:1", ".die"));
    assert!(!plan.blockers().iter().any(|diagnostic| {
        diagnostic.binding.as_deref() == Some("public") && diagnostic.message.contains("value")
    }));
}

#[test]
fn graph_export_is_deterministic_semantic_and_count_consistent() {
    let analyze = || {
        let root = package("root", &[("f", Some("f <- function() foo::bar()"))]);
        let foo = PackageFixture::new("foo", &[("bar", Some("bar <- function() 1"))])
            .exports(export("bar"))
            .build();
        analyze_images(vec![root, foo])
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

    let first = ExplanationDag::from_plan(&first_plan, &target, "root").unwrap();
    let second = ExplanationDag::from_plan(&second_plan, &target, "root").unwrap();
    let first_json = serde_json::to_string_pretty(&first).unwrap();
    let second_json = serde_json::to_string_pretty(&second).unwrap();

    assert_eq!(first_json, second_json);
    assert_eq!(first.stats.raw_nodes, first_plan.provenance().nodes().len());
    assert_eq!(first.stats.raw_edges, first_plan.provenance().edges().len());
    assert!(
        first
            .edges
            .iter()
            .flat_map(|edge| &edge.evidence)
            .any(|evidence| {
                evidence.from_member == "root::f"
                    && evidence.to_member == "foo::bar"
                    && evidence.reason == GraphEdgeReasonExport::QualifiedReference
            })
    );
    assert!(first.roots.iter().any(|root| {
        root.id == "root::f" && root.reasons == vec![GraphEdgeReasonExport::ExportRoot]
    }));
    assert!(first.roots.iter().any(|root| {
        root.id == "package:root" && root.reasons == vec![GraphEdgeReasonExport::PackageRoot]
    }));
}

#[test]
fn graph_export_survives_blocked_analysis() {
    let root = package("root", &[("awkward", Some("awkward <- function() {"))]);
    let provider = FakeProvider::new(vec![root])
        .validation(SyntaxValidation::Rejected("unexpected end of input".into()));
    let plan = link(provider);
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
    let export = ExplanationDag::from_plan(&plan, &target, "root").unwrap();
    let json = serde_json::to_string(&export).unwrap();

    assert!(!export.components.is_empty());
    assert!(!export.diagnostics.is_empty());
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
        analyze_images(vec![root, foo])
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
    let plan = analyze_images(vec![root]);
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
    let root = package(
        "root",
        &[
            ("f", Some("f <- function() dep::f()")),
            ("g", Some("g <- function() dep::g()")),
        ],
    );
    let dep = PackageFixture::new(
        "dep",
        &[
            ("f", Some("f <- function() { a(); c() }")),
            ("g", Some("g <- function() c()")),
            ("a", Some("a <- function() b()")),
            ("b", Some("b <- function() c()")),
            ("c", Some("c <- function() 1")),
        ],
    )
    .exports(ExportMap::from([
        ("f".into(), "f".into()),
        ("g".into(), "g".into()),
    ]))
    .build();
    let plan = analyze_images(vec![root, dep]);
    let explanation = ExplanationDag::from_plan(&plan, &test_target(), "root").unwrap();
    let component = |member: &str| {
        explanation
            .components
            .iter()
            .find(|component| component.members.iter().any(|item| item.id == member))
            .expect("member component")
    };
    let f = component("dep::f");
    let c = component("dep::c");

    assert_eq!(c.root_causes, ["root::f", "root::g"]);
    assert!(
        explanation
            .edges
            .iter()
            .any(|edge| { edge.from == f.id && edge.to == c.id && edge.reachability_redundant })
    );
}

#[test]
fn linked_discovery_with_unhonored_arguments_blocks() {
    let analyze = |source: &str| {
        let root = package_importing("root", &[("f", Some(source))], "foo");
        let foo = package("foo", &[("x", Some("x <- function() 1"))]);
        analyze_images(vec![root, foo, utils_platform()])
    };
    let blocks = |source: &str| {
        analyze(source).blockers().iter().any(|diagnostic| {
            diagnostic.code == RejectCode::UnsupportedRootTransformation
                && diagnostic.message.contains("foo")
        })
    };

    assert!(blocks(
        "f <- function() loadNamespace('foo', versionCheck = list(op = '>=', version = '2.0'))"
    ));
    assert!(blocks(
        "f <- function() utils::packageVersion('foo', lib.loc = 'x')"
    ));
    assert!(blocks(
        "f <- function() requireNamespace('foo', lib.loc = 'x')"
    ));
    assert!(!blocks(
        "f <- function() requireNamespace('foo', quietly = TRUE)"
    ));
    assert!(!blocks(
        "f <- function() asNamespace('foo', base.OK = FALSE)"
    ));
}

#[test]
fn rlang_package_queries_on_a_linked_package_are_rewritten() {
    let analyze = |source: &str| {
        let root = package_importing("root", &[("f", Some(source))], "foo, rlang");
        let foo = package("foo", &[("x", Some("x <- function() 1"))]);
        let rlang = package(
            "rlang",
            &[
                ("check_installed", None),
                ("is_installed", None),
                ("ns_env", None),
            ],
        );
        Linker::new(FakeProvider::new(vec![root, foo, rlang]), 1)
            .with_external_packages(["rlang"])
            .analyze("root")
            .unwrap()
    };
    let rewritten = |source: &str, target: slinker_core::ir::RelocationTarget| {
        let plan = analyze(source);
        assert!(
            plan.blockers().is_empty(),
            "{source}: {:?}",
            plan.blockers()
        );
        plan.program()
            .relocations()
            .iter()
            .any(|relocation| relocation.target == target)
    };

    assert!(rewritten(
        "f <- function() rlang::check_installed('foo', reason = 'to work')",
        slinker_core::ir::RelocationTarget::InstalledQuery { check: true }
    ));
    assert!(rewritten(
        "f <- function() rlang::is_installed(pkg = 'foo')",
        slinker_core::ir::RelocationTarget::InstalledQuery { check: false }
    ));
    assert!(
        analyze("f <- function() rlang::ns_env('foo')")
            .program()
            .relocations()
            .iter()
            .any(|relocation| matches!(
                relocation.target,
                slinker_core::ir::RelocationTarget::NamespaceArgument { .. }
            ))
    );
    assert!(!rewritten(
        "f <- function() rlang::is_installed('unrelated')",
        slinker_core::ir::RelocationTarget::InstalledQuery { check: false }
    ));
    assert!(
        analyze("f <- function() rlang::is_installed('foo', version = '2.0')")
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::UnsupportedRootTransformation)
    );
    assert!(
        analyze("f <- function(p) rlang::ns_env(p)")
            .blockers()
            .iter()
            .any(|diagnostic| diagnostic.code == RejectCode::DynamicPackageDiscovery)
    );
}

#[test]
fn enumerating_a_linked_namespace_blocks_but_targeted_lookup_does_not() {
    let analyze = |source: &str| {
        let root = package("root", &[("f", Some(source))]);
        let dep = package(
            "dep",
            &[
                ("x", Some("x <- function() 1")),
                ("unused", Some("unused <- function() 2")),
            ],
        );
        analyze_images(vec![root, dep])
    };
    let enumeration_blocked = |source: &str| {
        analyze(source).blockers().iter().any(|diagnostic| {
            diagnostic.code == RejectCode::UnsupportedRootTransformation
                && diagnostic.message.contains("reads every binding")
        })
    };

    for blocked in [
        "f <- function() as.list(asNamespace('dep'))",
        "f <- function() as.list(x = base::getNamespace('dep'))",
        "f <- function() as.list.environment(asNamespace('dep'), all.names = TRUE)",
        "f <- function() mget(ls(asNamespace('dep')), asNamespace('dep'))",
        "f <- function() mget(c('x', 'unused'), envir = asNamespace('dep'))",
        "f <- function() eapply(asNamespace('dep'), identity)",
        "f <- function() eapply(FUN = identity, env = asNamespace('dep'))",
        "f <- function() { ns <- asNamespace('dep'); as.list(ns) }",
        "f <- function() { ns <- asNamespace('dep'); mget(ls(ns), ns) }",
        "f <- function() { ns <- asNamespace('dep'); eapply(ns, identity) }",
    ] {
        assert!(enumeration_blocked(blocked), "{blocked}");
    }
    for accepted in [
        "f <- function() asNamespace('dep')$x()",
        "f <- function() { dep::x(); get('x', envir = asNamespace('dep'))() }",
        "f <- function() as.list(asNamespace('dep')$x)",
        "f <- function() as.list(ls(asNamespace('dep')))",
        "f <- function() as.list(c(1, 2))",
        "f <- function(env) { dep::x(); as.list(env) }",
        "f <- function() { dep::x(); mget('a', envir = environment()) }",
    ] {
        assert!(!enumeration_blocked(accepted), "{accepted}");
    }
}

fn lexical_method_namespace(
    entry: &str,
    methods: &[(&str, &str)],
    s3: Vec<S3Registration>,
) -> PackageImage {
    let mut bindings = vec![("run", Some(entry))];
    bindings.extend(methods.iter().map(|(name, source)| (*name, Some(*source))));
    PackageFixture::new("foo", &bindings)
        .exports(export("run"))
        .s3(s3)
        .build()
}

fn lexical_root() -> PackageImage {
    package("root", &[("f", Some("f <- function(x) foo::run(x)"))])
}

fn node_count(plan: &slinker_core::analysis::LinkIr, package: &str, binding: &str) -> usize {
    plan.provenance()
        .nodes()
        .iter()
        .filter(|node| {
            node.package == package
                && matches!(&node.kind, NodeKind::Binding { name } if name == binding)
        })
        .count()
}

#[test]
fn unregistered_lexical_method_is_retained_when_its_namespace_calls_a_base_generic() {
    let foo = lexical_method_namespace(
        "run <- function(x) print(x)",
        &[
            ("print.cls", "print.cls <- function(x, ...) 1"),
            ("other.cls", "other.cls <- function(x, ...) 2"),
            ("helper.fn", "helper.fn <- function(x) 3"),
        ],
        Vec::new(),
    );
    let provider =
        FakeProvider::new(vec![lexical_root(), foo]).dispatching(None, "print", &["print"]);
    let plan = link(provider);

    assert!(retained_binding(&plan, "foo", "print.cls"));
    assert!(!retained_binding(&plan, "foo", "other.cls"));
    assert!(!retained_binding(&plan, "foo", "helper.fn"));
}

#[test]
fn method_shaped_binding_is_not_retained_when_the_callee_is_not_a_generic() {
    let foo = lexical_method_namespace(
        "run <- function(x) print(x)",
        &[("print.cls", "print.cls <- function(x, ...) 1")],
        Vec::new(),
    );
    let plan = analyze_images(vec![lexical_root(), foo]);

    assert!(!retained_binding(&plan, "foo", "print.cls"));
}

#[test]
fn lexical_method_demand_comes_from_the_namespace_that_calls_the_generic() {
    let foo = lexical_method_namespace(
        "run <- function(x) x",
        &[("print.cls", "print.cls <- function(x, ...) 1")],
        Vec::new(),
    );
    let root = package(
        "root",
        &[("f", Some("f <- function(x) { foo::run(x); print(x) }"))],
    );
    let provider = FakeProvider::new(vec![root, foo]).dispatching(None, "print", &["print"]);
    let plan = link(provider);

    assert!(!retained_binding(&plan, "foo", "print.cls"));
}

#[test]
fn unregistered_lexical_method_is_retained_for_a_generic_of_an_external_package() {
    let foo = lexical_method_namespace(
        "run <- function(x) ext::gen(x)",
        &[("gen.cls", "gen.cls <- function(x, ...) 1")],
        Vec::new(),
    );
    let ext = package("ext", &[("gen", None)]);
    let provider =
        FakeProvider::new(vec![lexical_root(), foo, ext]).dispatching(Some("ext"), "gen", &["gen"]);
    let plan = Linker::new(provider, 1)
        .with_external_packages(["ext"])
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "foo", "gen.cls"));
}

#[test]
fn lexical_method_follows_an_external_reexport_to_the_defining_namespace() {
    let foo = lexical_method_namespace(
        "run <- function(x) mid::gen(x)",
        &[("gen.cls", "gen.cls <- function(x, ...) 1")],
        Vec::new(),
    );
    let ext = package("ext", &[("gen", None)]);
    let mid = PackageFixture::new("mid", &[])
        .imports(vec![ImportSpec::From {
            package: "ext".into(),
            bindings: vec![ImportBinding {
                local: "gen".into(),
                remote: "gen".into(),
            }],
        }])
        .exports(export("gen"))
        .description("Imports: ext\n")
        .build();
    let provider = FakeProvider::new(vec![lexical_root(), foo, ext, mid]).dispatching(
        Some("ext"),
        "gen",
        &["gen"],
    );
    let plan = Linker::new(provider, 1)
        .with_external_packages(["ext", "mid"])
        .analyze("root")
        .unwrap();

    assert!(retained_binding(&plan, "foo", "gen.cls"));
}

#[test]
fn group_generic_operator_retains_the_group_and_member_methods_only() {
    let foo = lexical_method_namespace(
        "run <- function(x) x + x",
        &[
            ("Ops.cls", "Ops.cls <- function(e1, e2) 1"),
            ("+.cls", "`+.cls` <- function(e1, e2) 2"),
            ("-.cls", "`-.cls` <- function(e1, e2) 3"),
        ],
        Vec::new(),
    );
    let provider =
        FakeProvider::new(vec![lexical_root(), foo]).dispatching(None, "+", &["+", "Ops"]);
    let plan = link(provider);

    assert!(retained_binding(&plan, "foo", "Ops.cls"));
    assert!(retained_binding(&plan, "foo", "+.cls"));
    assert!(!retained_binding(&plan, "foo", "-.cls"));
}

#[test]
fn lexical_retention_does_not_duplicate_a_registered_method() {
    let foo = lexical_method_namespace(
        "run <- function(x) print(x)",
        &[
            ("print.reg", "print.reg <- function(x, ...) 1"),
            ("print.cls", "print.cls <- function(x, ...) 2"),
        ],
        vec![S3Registration {
            generic: slinker_core::package::GenericSpec {
                package: None,
                name: "print".into(),
            },
            class: "reg".into(),
            method: "print.reg".into(),
        }],
    );
    let provider =
        FakeProvider::new(vec![lexical_root(), foo]).dispatching(None, "print", &["print"]);
    let plan = link(provider);

    assert!(retained_binding(&plan, "foo", "print.cls"));
    assert_eq!(node_count(&plan, "foo", "print.reg"), 1);
    assert!(program_has_s3_registration(
        &plan,
        "foo",
        None,
        "print",
        "reg",
        "print.reg"
    ));
    assert!(
        !program_has_s3_registration(&plan, "foo", None, "print", "cls", "print.cls"),
        "an unregistered lexical method must not gain a registration"
    );
}

fn with_data(mut image: PackageImage, sets: &[(&str, &[&str])], file_backed: bool) -> PackageImage {
    let sets = sets
        .iter()
        .map(|(set, objects)| {
            (
                (*set).into(),
                objects
                    .iter()
                    .map(|object| DatasetName::from(*object))
                    .collect(),
            )
        })
        .collect();
    Arc::make_mut(&mut image.index).data = PackageData::new(sets, file_backed.into());
    image
}

fn data_package(file_backed: bool) -> PackageImage {
    with_data(
        package("foo", &[("run", Some("run <- function() 1"))]),
        &[
            ("alpha", &["alpha"]),
            ("beta", &["beta"]),
            ("multi", &["left", "right"]),
        ],
        file_backed,
    )
}

fn carried_objects(plan: &slinker_core::analysis::LinkIr, package: &str) -> Vec<String> {
    plan.program()
        .dataset_libraries()
        .filter(|(id, _)| plan.program().package(*id).identity().name == package)
        .flat_map(|(_, library)| library.objects().iter().map(ToString::to_string))
        .collect()
}

fn carried_sets(plan: &slinker_core::analysis::LinkIr, package: &str) -> Vec<String> {
    plan.program()
        .dataset_libraries()
        .filter(|(id, _)| plan.program().package(*id).identity().name == package)
        .flat_map(|(_, library)| library.sets().keys().map(ToString::to_string))
        .collect()
}

fn importing_root(source: &str) -> PackageImage {
    PackageFixture::new("root", &[("f", Some(source))])
        .exports(export("f"))
        .description("Imports: foo\n")
        .build()
}

fn data_use(source: &str) -> slinker_core::analysis::LinkIr {
    Linker::new(
        FakeProvider::new(vec![importing_root(source), data_package(false)]),
        1,
    )
    .analyze("root")
    .unwrap()
}

#[test]
fn qualified_dataset_access_carries_only_the_reached_dataset() {
    let plan = data_use("f <- function() foo::alpha");

    assert!(plan.blockers().is_empty());
    assert_eq!(carried_objects(&plan, "foo"), ["alpha"]);
    assert!(carried_sets(&plan, "foo").is_empty());
    assert!(
        plan.program()
            .relocations()
            .iter()
            .any(|relocation| matches!(
                &relocation.target,
                slinker_core::ir::RelocationTarget::Dataset { dataset, .. } if dataset == "alpha"
            ))
    );
    assert!(
        !plan
            .provenance()
            .nodes()
            .iter()
            .any(|node| { matches!(&node.kind, NodeKind::Dataset { name } if name == "beta") })
    );
}

#[test]
fn dataset_use_inside_a_linked_function_is_carried() {
    let root = package("root", &[("f", Some("f <- function() foo::run()"))]);
    let foo = with_data(
        package("foo", &[("run", Some("run <- function() foo::beta"))]),
        &[("alpha", &["alpha"]), ("beta", &["beta"])],
        false,
    );
    let plan = analyze_images(vec![root, foo]);

    assert!(plan.blockers().is_empty());
    assert_eq!(carried_objects(&plan, "foo"), ["beta"]);
}

#[test]
fn static_data_call_carries_the_whole_named_set() {
    let plan = data_use("f <- function(e) data(multi, package = \"foo\", envir = e)");

    assert!(plan.blockers().is_empty());
    assert_eq!(carried_objects(&plan, "foo"), ["left", "right"]);
    assert_eq!(carried_sets(&plan, "foo"), ["multi"]);
    assert!(
        plan.program()
            .relocations()
            .iter()
            .any(|relocation| matches!(
                relocation.target,
                slinker_core::ir::RelocationTarget::DataArgument { .. }
            ))
    );
}

#[test]
fn data_set_named_by_string_or_list_resolves_like_a_bare_name() {
    for source in [
        "f <- function() data(\"multi\", package = \"foo\")",
        "f <- function() data(list = \"multi\", package = \"foo\")",
        "f <- function() utils::data(multi, package = \"foo\")",
    ] {
        let plan = Linker::new(
            FakeProvider::new(vec![
                importing_root(source),
                data_package(false),
                utils_platform(),
            ]),
            1,
        )
        .analyze("root")
        .unwrap();
        assert!(plan.blockers().is_empty(), "{source}");
        assert_eq!(carried_sets(&plan, "foo"), ["multi"], "{source}");
    }
}

#[test]
fn dynamic_or_unsupported_data_forms_block() {
    for (source, code) in [
        (
            "f <- function(nm) data(list = nm, package = \"foo\")",
            RejectCode::DynamicLookup,
        ),
        (
            "f <- function() data(c(\"alpha\", \"beta\"), package = \"foo\")",
            RejectCode::DynamicLookup,
        ),
        (
            "f <- function() data(package = \"foo\")",
            RejectCode::DynamicLookup,
        ),
        (
            "f <- function(pkg) data(alpha, package = pkg)",
            RejectCode::DynamicPackageDiscovery,
        ),
        (
            "f <- function() data(alpha, package = \"foo\", lib.loc = \"x\")",
            RejectCode::UnsupportedRootTransformation,
        ),
        (
            "f <- function() data(missing_set, package = \"foo\")",
            RejectCode::UnresolvedBinding,
        ),
    ] {
        let plan = data_use(source);
        assert!(
            plan.blockers().iter().any(|blocker| blocker.code == code),
            "{source}: {:?}",
            plan.blockers()
        );
        assert!(
            plan.program().dataset_libraries().next().is_none(),
            "{source}"
        );
    }
}

#[test]
fn data_call_without_a_package_searches_attached_packages_and_blocks() {
    let plan = data_use("f <- function() data(alpha)");

    assert!(
        plan.blockers()
            .iter()
            .any(|blocker| blocker.code == RejectCode::DynamicLookup),
        "{:?}",
        plan.blockers()
    );
    assert!(plan.program().dataset_libraries().next().is_none());
}

#[test]
fn data_call_on_an_unselected_suggested_package_blocks() {
    let plan = Linker::new(
        FakeProvider::new(vec![
            suggesting_root("f <- function() data(alpha, package = \"foo\")"),
            data_package(false),
        ]),
        1,
    )
    .analyze("root")
    .unwrap();

    assert_eq!(optional_availability_blockers(&plan), ["root::f"]);
    assert!(plan.program().dataset_libraries().next().is_none());
}

#[test]
fn file_backed_data_cannot_be_carried_and_blocks() {
    let plan = Linker::new(
        FakeProvider::new(vec![
            importing_root("f <- function() data(alpha, package = \"foo\")"),
            data_package(true),
        ]),
        1,
    )
    .analyze("root")
    .unwrap();

    assert!(
        plan.blockers()
            .iter()
            .any(|blocker| blocker.code == RejectCode::UnsupportedObject)
    );
}

#[test]
fn exported_binding_wins_over_a_dataset_of_the_same_name() {
    let root = package("root", &[("f", Some("f <- function() foo::alpha"))]);
    let foo = with_data(
        package("foo", &[("alpha", Some("alpha <- function() 1"))]),
        &[("alpha", &["alpha"])],
        false,
    );
    let plan = analyze_images(vec![root, foo]);

    assert!(retained_binding(&plan, "foo", "alpha"));
    assert!(plan.program().dataset_libraries().next().is_none());
}

#[test]
fn unreached_datasets_are_never_carried() {
    let plan = data_use("f <- function() 1");

    assert!(plan.program().dataset_libraries().next().is_none());
}

fn dependency_importing_from_missing_package() -> PackageImage {
    PackageFixture::new(
        "dep",
        &[
            ("f1", Some("f1 <- function() a()")),
            ("f2", Some("f2 <- function() b()")),
            ("f3", Some("f3 <- function() c3()")),
        ],
    )
    .imports(vec![ImportSpec::From {
        package: "gone".into(),
        bindings: ["a", "b", "c3"]
            .into_iter()
            .map(|name| ImportBinding {
                local: name.into(),
                remote: name.into(),
            })
            .collect(),
    }])
    .exports(ExportMap::from([
        ("f1".into(), "f1".into()),
        ("f2".into(), "f2".into()),
        ("f3".into(), "f3".into()),
    ]))
    .description("Imports: gone\n")
    .build()
}

fn missing_package_blockers(
    plan: &slinker_core::analysis::LinkIr,
) -> Vec<&slinker_core::analysis::Diagnostic> {
    plan.blockers()
        .iter()
        .filter(|diagnostic| diagnostic.code == RejectCode::MissingDependency)
        .collect()
}

#[test]
fn one_missing_package_is_one_primary_blocker_with_every_requester_as_evidence() {
    let root = package(
        "root",
        &[
            ("r1", Some("r1 <- function() dep::f1()")),
            ("r2", Some("r2 <- function() dep::f2()")),
            ("r3", Some("r3 <- function() dep::f3()")),
        ],
    );
    let plan = Linker::new(
        FakeProvider::new(vec![root, dependency_importing_from_missing_package()]),
        1,
    )
    .analyze("root")
    .unwrap();

    let primaries = missing_package_blockers(&plan);
    assert_eq!(primaries.len(), 1, "{primaries:?}");
    assert_eq!(primaries[0].package, "gone");
    let requesters = primaries[0]
        .evidence
        .iter()
        .map(|evidence| {
            format!(
                "{}::{}",
                evidence.package,
                evidence.binding.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(requesters, ["dep::f1", "dep::f2", "dep::f3"]);
    assert_eq!(
        primaries[0].evidence_summary().as_deref(),
        Some("dep::f1, dep::f2, dep::f3")
    );
}

#[test]
fn independent_primary_blockers_stay_independent() {
    let root = package(
        "root",
        &[
            ("r1", Some("r1 <- function() dep::f1()")),
            ("r2", Some("r2 <- function() dep::f2()")),
            ("r3", Some("r3 <- function() { other::x(); other::y() }")),
            ("r4", Some("r4 <- function(n) get(n)")),
        ],
    );
    let plan = Linker::new(
        FakeProvider::new(vec![root, dependency_importing_from_missing_package()]),
        1,
    )
    .analyze("root")
    .unwrap();

    let mut missing = missing_package_blockers(&plan)
        .into_iter()
        .map(|primary| (primary.package.clone(), primary.evidence.len()))
        .collect::<Vec<_>>();
    missing.sort();
    assert_eq!(missing, [("gone".into(), 2), ("other".into(), 2)]);
    assert!(plan.blockers().iter().any(|blocker| {
        blocker.code == RejectCode::DynamicLookup && blocker.binding.as_deref() == Some("r4")
    }));
}

#[test]
fn derivative_grouping_leaves_the_graph_observational_and_the_report_deterministic() {
    let analyze = |jobs| {
        let root = package(
            "root",
            &[
                ("r1", Some("r1 <- function() dep::f1()")),
                ("r2", Some("r2 <- function() dep::f2()")),
                ("r3", Some("r3 <- function() dep::f3()")),
            ],
        );
        Linker::new(
            FakeProvider::new(vec![root, dependency_importing_from_missing_package()]),
            jobs,
        )
        .analyze("root")
        .unwrap()
    };
    let plan = analyze(1);

    let missing = plan.provenance().missing_packages().collect::<Vec<_>>();
    assert_eq!(missing.len(), 1);
    let derivations = plan
        .provenance()
        .edges()
        .iter()
        .filter(|edge| edge.to == missing[0])
        .count();
    assert_eq!(derivations, 3);

    let render = |plan: &slinker_core::analysis::LinkIr| {
        plan.blockers()
            .iter()
            .map(|blocker| {
                (
                    blocker.code,
                    blocker.message.clone(),
                    blocker
                        .evidence
                        .iter()
                        .map(|evidence| {
                            (
                                evidence.package.clone(),
                                evidence.binding.clone(),
                                evidence.span.as_ref().map(|span| {
                                    (
                                        plan.sources().origin(&span.source).to_string(),
                                        span.start,
                                        span.end,
                                    )
                                }),
                                evidence.detail.clone(),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(render(&plan), render(&analyze(1)));
    assert_eq!(render(&plan), render(&analyze(4)));
}

#[test]
fn one_name_creator_is_one_primary_blocker_for_the_free_names_it_could_bind() {
    let root = package(
        "root",
        &[
            (
                "f",
                Some("f <- function(n) { assign(n, 1); alpha + beta + gamma }"),
            ),
            ("h", Some("h <- function() epsilon")),
        ],
    );
    let plan = analyze_images(vec![root]);

    let primaries = plan
        .blockers()
        .iter()
        .filter(|blocker| blocker.code == RejectCode::UnresolvedBinding)
        .collect::<Vec<_>>();
    assert_eq!(primaries.len(), 1, "{primaries:?}");
    assert_eq!(primaries[0].binding.as_deref(), Some("f"));
    assert_eq!(
        primaries[0]
            .evidence
            .iter()
            .map(|evidence| evidence.detail.as_str())
            .collect::<Vec<_>>(),
        [
            "`alpha` is bound nowhere",
            "`beta` is bound nowhere",
            "`gamma` is bound nowhere",
            "`epsilon` is bound nowhere",
        ]
    );
    assert_eq!(
        primaries[0].evidence_summary().as_deref(),
        Some("root::f, root::h")
    );
}

#[test]
fn syntax_forms_that_dispatch_retain_their_lexical_methods() {
    for (source, callee, generics, method) in [
        ("run <- function(x) x[1]", "[", &["["][..], "[.cls"),
        ("run <- function(x) x[[1]]", "[[", &["[["][..], "[[.cls"),
        ("run <- function(x) x$a", "$", &["$"][..], "$.cls"),
        ("run <- function(x) -x", "-", &["-", "Ops"][..], "Ops.cls"),
        ("run <- function(x) !x", "!", &["!", "Ops"][..], "!.cls"),
        (
            "run <- function(x) { x[1] <- 0; x }",
            "[<-",
            &["[<-"][..],
            "[<-.cls",
        ),
        (
            "run <- function(x) { x$a <- 0; x }",
            "$<-",
            &["$<-"][..],
            "$<-.cls",
        ),
        (
            "run <- function(x) { names(x) <- 'n'; x }",
            "names<-",
            &["names<-"][..],
            "names<-.cls",
        ),
        (
            "run <- function(x) { names(x)[2] <- 'n'; x }",
            "names<-",
            &["names<-"][..],
            "names<-.cls",
        ),
    ] {
        let foo = lexical_method_namespace(
            source,
            &[
                (method, &format!("`{method}` <- function(x, ...) 1")),
                ("other.cls", "other.cls <- function(x, ...) 2"),
            ],
            Vec::new(),
        );
        let provider =
            FakeProvider::new(vec![lexical_root(), foo]).dispatching(None, callee, generics);
        let plan = link(provider);
        assert!(retained_binding(&plan, "foo", method), "{source}");
        assert!(!retained_binding(&plan, "foo", "other.cls"), "{source}");
    }
}

mod schedule_equivalence {
    use super::support::canonical::Canonical;
    use super::{
        FakeProvider, PackageFixture, analyze_images, export, package, private_closure, test_target,
    };
    use slinker_core::analysis::{ExplanationDag, LinkIr, Linker, Schedule};
    use slinker_core::package::{
        EmbeddedClosureSource, ImportBinding, ImportSpec, MemberPath, NativeComponent, NativeFacts,
        NativeLibrary, NativeRegistration, NativeSafety, NativeSymbolBinding, ObjectKind,
        PackageImage, PrivateEnvironmentImage, S3Registration,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    struct Scenario {
        name: &'static str,
        images: Vec<PackageImage>,
        external: &'static [&'static str],
    }

    fn scenarios() -> Vec<Scenario> {
        vec![
            cycle_chain(),
            diamond(),
            onload_active_binding(),
            s3_activation(),
            reenclosed_derived_environment(),
            recursion(),
            private_environment(),
            imports(),
            closed_generic(),
            name_creators(),
            contextual_namespace_chain(),
            native_registered(),
            guarded_optional(),
            resources(),
            blocked(),
        ]
    }

    fn cycle_chain() -> Scenario {
        let root = PackageFixture::new("root", &[("f", Some("f <- function() dep::g()"))])
            .exports(export("f"))
            .build();
        let dep = PackageFixture::new("dep", &[("g", Some("g <- function() leaf::h()"))])
            .exports(export("g"))
            .build();
        let leaf = PackageFixture::new(
            "leaf",
            &[
                ("h", Some("h <- function() a()")),
                ("a", Some("a <- function() b()")),
                ("b", Some("b <- function() { c <- a; c() }")),
            ],
        )
        .exports(export("h"))
        .build();
        Scenario {
            name: "cycle_chain",
            images: vec![root, dep, leaf],
            external: &[],
        }
    }

    fn diamond() -> Scenario {
        let root =
            PackageFixture::new("root", &[("f", Some("f <- function() { p::x(); q::y() }"))])
                .exports(export("f"))
                .build();
        let p = PackageFixture::new("p", &[("x", Some("x <- function() shared::z()"))])
            .exports(export("x"))
            .build();
        let q = PackageFixture::new("q", &[("y", Some("y <- function() shared::z()"))])
            .exports(export("y"))
            .build();
        let shared = PackageFixture::new(
            "shared",
            &[
                ("z", Some("z <- function() w()")),
                ("w", Some("w <- function() 1")),
            ],
        )
        .exports(export("z"))
        .build();
        Scenario {
            name: "diamond",
            images: vec![root, p, q, shared],
            external: &[],
        }
    }

    fn onload_active_binding() -> Scenario {
        let root = PackageFixture::new("root", &[("f", Some("f <- function() foo::pb"))])
            .exports(export("f"))
            .description("Imports: foo\n")
            .build();
        let mut dependency = PackageFixture::new(
            "foo",
            &[
                ("dummy", Some("dummy <- function() NULL")),
                ("get_pb", Some("get_pb <- function() helper()")),
                ("helper", Some("helper <- function() 1")),
                (
                    ".onLoad",
                    Some(
                        ".onLoad <- function(lib, pkg) { pkgenv <- environment(dummy); makeActiveBinding(\"pb\", get_pb, pkgenv) }",
                    ),
                ),
            ],
        )
        .exports(export("pb"))
        .build();
        Arc::make_mut(&mut dependency.index).lifecycle.on_load = true;
        Scenario {
            name: "onload_active_binding",
            images: vec![root, dependency],
            external: &[],
        }
    }

    fn s3_activation() -> Scenario {
        let root = package("root", &[("f", Some("f <- function() foo::x()"))]);
        let registration = |generic: &str, method: &str| S3Registration {
            generic: slinker_core::package::GenericSpec {
                package: None,
                name: generic.into(),
            },
            class: "foo".into(),
            method: method.into(),
        };
        let dependency = PackageFixture::new(
            "foo",
            &[
                ("x", Some("x <- function() 1")),
                ("print.foo", Some("print.foo <- function(x, ...) helper(x)")),
                ("format.foo", Some("format.foo <- function(x, ...) x")),
                ("helper", Some("helper <- function(x) x")),
            ],
        )
        .exports(export("x"))
        .s3(vec![
            registration("print", "print.foo"),
            registration("format", "format.foo"),
        ])
        .build();
        Scenario {
            name: "s3_activation",
            images: vec![root, dependency],
            external: &[],
        }
    }

    fn reenclosed_derived_environment() -> Scenario {
        let mut root = PackageFixture::new(
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
                ("first_dependency", Some("first_dependency <- function() 1")),
                (
                    "second_dependency",
                    Some("second_dependency <- function() 1"),
                ),
            ],
        )
        .exports(export("f"))
        .build();
        Arc::make_mut(root.bindings.get_mut("f").unwrap())
            .object
            .closure
            .as_mut()
            .unwrap()
            .environment = "private:1".into();
        let capsule = Arc::make_mut(root.bindings.get_mut("capsule").unwrap());
        capsule.object.object_kind = ObjectKind::Environment;
        capsule.object.environment = Some("private:1".into());
        let templates = Arc::make_mut(root.bindings.get_mut("templates").unwrap());
        templates.object.object_kind = ObjectKind::List;
        for name in ["first", "second"] {
            templates
                .object
                .embedded_closures
                .push(EmbeddedClosureSource {
                    path: MemberPath::root().field(name),
                    source: Arc::from(format!(
                        ".slinker_embedded <- function() {{ self; {name}_dependency() }}"
                    )),
                    environment: "namespace:root".into(),
                });
        }
        root.private_environments.insert(
            "private:1".into(),
            Arc::new(PrivateEnvironmentImage {
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
            }),
        );
        Scenario {
            name: "reenclosed_derived_environment",
            images: vec![root],
            external: &[],
        }
    }

    fn recursion() -> Scenario {
        let root = PackageFixture::new(
            "root",
            &[
                ("f", Some("f <- function() helper(ping(\"x\"))")),
                (
                    "helper",
                    Some("helper <- function(package) requireNamespace(package)"),
                ),
                (
                    "ping",
                    Some("ping <- function(n) if (print(n)) pong(n) else \"foo\""),
                ),
                ("pong", Some("pong <- function(n) ping(n)")),
            ],
        )
        .exports(export("f"))
        .build();
        Scenario {
            name: "recursion",
            images: vec![root, package("foo", &[])],
            external: &["foo"],
        }
    }

    fn private_environment() -> Scenario {
        let mut root = PackageFixture::new(
            "root",
            &[
                ("public", Some("public <- function() used()")),
                ("other", Some("other <- function() used()")),
            ],
        )
        .exports(export("public"))
        .build();
        for name in ["public", "other"] {
            Arc::make_mut(root.bindings.get_mut(name).unwrap())
                .object
                .closure
                .as_mut()
                .unwrap()
                .environment = "private:1".into();
        }
        root.private_environments.insert(
            "private:1".into(),
            Arc::new(PrivateEnvironmentImage {
                id: "private:1".into(),
                parent: "namespace:root".into(),
                bindings: HashMap::from([
                    (
                        "used".into(),
                        private_closure("used", "private:1", "used <- function() deeper()"),
                    ),
                    (
                        "deeper".into(),
                        private_closure("deeper", "private:1", "deeper <- function() 1"),
                    ),
                    (
                        "unused".into(),
                        private_closure("unused", "private:1", "unused <- function() 2"),
                    ),
                ]),
            }),
        );
        Scenario {
            name: "private_environment",
            images: vec![root],
            external: &[],
        }
    }

    fn imports() -> Scenario {
        let root = PackageFixture::new(
            "root",
            &[("f", Some("f <- function() { renamed(); bar() }"))],
        )
        .exports(export("f"))
        .imports(vec![
            ImportSpec::From {
                package: "foo".into(),
                bindings: vec![ImportBinding {
                    local: "renamed".into(),
                    remote: "orig".into(),
                }],
            },
            ImportSpec::All {
                package: "baz".into(),
                except: Vec::new(),
            },
        ])
        .description("Imports: foo, baz\n")
        .build();
        let dependency = PackageFixture::new("foo", &[("orig", Some("orig <- function() 1"))])
            .exports(export("orig"))
            .build();
        let wide = PackageFixture::new(
            "baz",
            &[
                ("bar", Some("bar <- function() baz_helper()")),
                ("baz_helper", Some("baz_helper <- function() 1")),
            ],
        )
        .exports(export("bar"))
        .build();
        Scenario {
            name: "imports",
            images: vec![root, dependency, wide],
            external: &[],
        }
    }

    fn closed_generic() -> Scenario {
        let root = package("root", &[("f", Some("f <- function(x) foo::criterion(x)"))]);
        let dependency = PackageFixture::new(
            "foo",
            &[
                (
                    "criterion",
                    Some("criterion <- function(x) UseMethod(\"criterion\")"),
                ),
                (
                    "criterion.character",
                    Some("criterion.character <- function(x) helper(x)"),
                ),
                (
                    "criterion.default",
                    Some("criterion.default <- function(x) x"),
                ),
                ("as_criterion", Some("as_criterion <- function(x) x")),
                ("helper", Some("helper <- function(x) x")),
                ("unrelated", Some("unrelated <- function() 1")),
            ],
        )
        .exports(export("criterion"))
        .s3(vec![S3Registration {
            generic: slinker_core::package::GenericSpec {
                package: None,
                name: "criterion".into(),
            },
            class: "root_criterion".into(),
            method: "as_criterion".into(),
        }])
        .build();
        Scenario {
            name: "closed_generic",
            images: vec![root, dependency],
            external: &[],
        }
    }

    fn name_creators() -> Scenario {
        let root = package(
            "root",
            &[
                (
                    "f",
                    Some("f <- function(n) { assign(n, 1); alpha + beta + gamma }"),
                ),
                ("g", Some("g <- function(n) { delayedAssign(n, 2); delta }")),
                ("h", Some("h <- function() epsilon")),
            ],
        );
        Scenario {
            name: "name_creators",
            images: vec![root],
            external: &[],
        }
    }

    fn contextual_namespace_chain() -> Scenario {
        let root = PackageFixture::new(
            "root",
            &[
                ("f", Some("f <- function() outer(\"foo\")")),
                ("outer", Some("outer <- function(package) inner(package)")),
                (
                    "inner",
                    Some("inner <- function(package) requireNamespace(package)"),
                ),
                ("g", Some("g <- function() inner(\"foo\")")),
            ],
        )
        .exports(export("f"))
        .build();
        Scenario {
            name: "contextual_namespace_chain",
            images: vec![root, package("foo", &[])],
            external: &["foo"],
        }
    }

    fn native_registered() -> Scenario {
        let root = PackageFixture::new(
            "root",
            &[
                ("f", Some("f <- function(x) .Call(croot_f, x)")),
                ("g", Some("g <- function(x) .Call(croot_g, x)")),
            ],
        )
        .exports(export("f"))
        .dynlibs(vec![NativeComponent {
            name: "root".into(),
            alias: String::new(),
            registration: Some(NativeRegistration {
                prefix: "c".into(),
                suffix: String::new(),
            }),
            symbols: vec![
                NativeSymbolBinding {
                    binding: "croot_f".into(),
                    symbol: "root_f".into(),
                },
                NativeSymbolBinding {
                    binding: "croot_g".into(),
                    symbol: "root_g".into(),
                },
            ],
            library: NativeLibrary::Missing,
            safety: NativeSafety::Safe(NativeFacts {
                callbacks: Vec::new(),
            }),
        }])
        .build();
        Scenario {
            name: "native_registered",
            images: vec![root],
            external: &[],
        }
    }

    fn guarded_optional() -> Scenario {
        let root = PackageFixture::new(
            "root",
            &[
                (
                    "f",
                    Some("f <- function() if (requireNamespace(\"foo\", quietly = TRUE)) foo::bar() else helper()"),
                ),
                ("helper", Some("helper <- function() 1")),
            ],
        )
        .exports(export("f"))
        .description("Suggests: foo\n")
        .build();
        let dependency = PackageFixture::new("foo", &[("bar", Some("bar <- function() 2"))])
            .exports(export("bar"))
            .build();
        Scenario {
            name: "guarded_optional",
            images: vec![root, dependency],
            external: &[],
        }
    }

    fn resources() -> Scenario {
        let root = PackageFixture::new(
            "root",
            &[(
                "f",
                Some("f <- function() system.file(\"data\", \"x.json\", package = \"foo\")"),
            )],
        )
        .exports(export("f"))
        .build();
        let dependency = PackageFixture::new("foo", &[])
            .files(vec!["data/x.json".into(), "data/y.json".into()])
            .build();
        Scenario {
            name: "resources",
            images: vec![root, dependency],
            external: &[],
        }
    }

    fn blocked() -> Scenario {
        let root = PackageFixture::new(
            "root",
            &[
                (
                    "f",
                    Some(
                        "f <- function(p) { library(foo); requireNamespace(p); undefined_name() }",
                    ),
                ),
                ("g", Some("g <- function() getNamespace(\"foo\")")),
            ],
        )
        .exports(export("f"))
        .build();
        Scenario {
            name: "blocked",
            images: vec![root, package("foo", &[])],
            external: &[],
        }
    }

    struct Observation {
        program: String,
        explanation: String,
        blockers: Vec<String>,
    }

    fn observe(scenario: &Scenario, schedule: Schedule, jobs: usize) -> Observation {
        let provider = FakeProvider::new(scenario.images.clone());
        let plan: LinkIr = Linker::new(provider, jobs)
            .with_external_packages(scenario.external.iter().copied())
            .with_schedule(schedule)
            .analyze("root")
            .unwrap();
        let explanation = ExplanationDag::from_plan(&plan, &test_target(), "root").unwrap();
        let mut blockers = plan
            .blockers()
            .iter()
            .map(|diagnostic| {
                format!(
                    "{:?} {} {:?} {}",
                    diagnostic.code, diagnostic.package, diagnostic.binding, diagnostic.message
                )
            })
            .collect::<Vec<_>>();
        blockers.sort();
        Observation {
            program: Canonical::new(plan.program()).render(),
            explanation: serde_json::to_string_pretty(&explanation).unwrap(),
            blockers,
        }
    }

    fn first_difference(left: &str, right: &str) -> String {
        left.lines()
            .zip(right.lines())
            .find(|(left, right)| left != right)
            .map_or_else(
                || {
                    format!(
                        "length {} vs {}",
                        left.lines().count(),
                        right.lines().count()
                    )
                },
                |(left, right)| format!("`{left}` vs `{right}`"),
            )
    }

    #[test]
    fn every_legal_need_order_yields_the_same_program_provenance_and_blockers() {
        let schedules = [Schedule::Fifo, Schedule::Lifo]
            .into_iter()
            .chain((1..=24).map(Schedule::Seeded))
            .collect::<Vec<_>>();
        for scenario in scenarios() {
            let reference = observe(&scenario, Schedule::Fifo, 1);
            for schedule in &schedules {
                for jobs in [1, 2, 8] {
                    let observed = observe(&scenario, *schedule, jobs);
                    assert!(
                        reference.program == observed.program,
                        "{} program under {schedule:?} jobs={jobs}: {}",
                        scenario.name,
                        first_difference(&reference.program, &observed.program)
                    );
                    assert!(
                        reference.explanation == observed.explanation,
                        "{} provenance under {schedule:?} jobs={jobs}: {}",
                        scenario.name,
                        first_difference(&reference.explanation, &observed.explanation)
                    );
                    assert!(
                        reference.blockers == observed.blockers,
                        "{} blockers under {schedule:?} jobs={jobs}: {:?} vs {:?}",
                        scenario.name,
                        reference.blockers,
                        observed.blockers
                    );
                }
            }
        }
    }

    #[test]
    fn schedule_scenarios_are_not_vacuous() {
        for scenario in scenarios() {
            let plan = analyze_images(scenario.images.clone());
            assert!(
                !plan.program().namespaces().is_empty(),
                "{} produces a program",
                scenario.name
            );
        }
        let blocked = observe(&blocked(), Schedule::Fifo, 1);
        assert!(!blocked.blockers.is_empty());
        let rendered = observe(&cycle_chain(), Schedule::Fifo, 1).program;
        assert!(rendered.contains("leaf") && rendered.contains("binding"));
    }
}
