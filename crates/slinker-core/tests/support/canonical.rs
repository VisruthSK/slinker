use slinker_core::ir::{
    BindingId, ClosureHome, ClosureId, CodeId, EnvironmentId, EnvironmentKind, ImportSlotIr,
    InitialBindingState, LinkBindingState, LinkNamespaceState, NamespaceId, PackageId, ProgramIr,
    RelocationTarget, ResourceId, S3RegistrationId, Value, ValueId,
};
use std::collections::BTreeSet;
use std::fmt::Write as _;

pub struct Canonical<'a> {
    program: &'a ProgramIr,
}

impl<'a> Canonical<'a> {
    pub fn new(program: &'a ProgramIr) -> Self {
        Self { program }
    }

    fn package(&self, id: PackageId) -> String {
        let identity = self.program.package(id).identity();
        format!("{}@{}", identity.name, identity.image_fingerprint)
    }

    fn namespace(&self, id: NamespaceId) -> String {
        self.package(self.program.namespace(id).package)
    }

    fn binding(&self, id: BindingId) -> String {
        format!(
            "{}::{}",
            self.namespace(self.program.binding_namespace(id)),
            self.program.binding(id).name
        )
    }

    fn code(&self, id: CodeId) -> String {
        let code = self.program.code(id);
        format!("code[{}|{}]", code.normalized_shape(), code.source())
    }

    fn environment(&self, id: EnvironmentId) -> String {
        match self.program.environment(id).kind {
            EnvironmentKind::Namespace(namespace) => format!("ns-env({})", self.namespace(namespace)),
            EnvironmentKind::Imports(namespace) => {
                format!("imports-env({})", self.namespace(namespace))
            }
        }
    }

    fn closure(&self, id: ClosureId) -> String {
        let closure = self.program.closure(id);
        format!(
            "closure({} in {})",
            self.code(closure.code),
            self.environment(closure.enclosure)
        )
    }

    fn value(&self, id: ValueId) -> String {
        match self.program.value(id) {
            Value::Closure(closure) => self.closure(*closure),
            Value::Payload(bundle) => format!(
                "payload({})",
                self.namespace(self.program.payload_bundle(*bundle).namespace())
            ),
        }
    }

    fn resource(&self, id: ResourceId) -> String {
        let resource = self.program.resource(id);
        format!("resource({}:{})", self.package(resource.package), resource.path)
    }

    fn s3(&self, id: S3RegistrationId) -> String {
        let registration = self.program.s3_registration(id);
        format!(
            "s3({} {:?} {} -> {})",
            self.namespace(registration.owner_namespace),
            registration.generic,
            registration.class,
            self.binding(registration.method)
        )
    }

    fn relocation_target(&self, target: &RelocationTarget) -> String {
        match target {
            RelocationTarget::Binding { target, access } => {
                format!("binding {} {access:?}", self.binding(*target))
            }
            RelocationTarget::Namespace { package, operation } => {
                format!("namespace {} {operation:?}", self.package(*package))
            }
            RelocationTarget::Resource { target } => format!("resource {}", self.resource(*target)),
            RelocationTarget::NamespaceArgument { package } => {
                format!("namespace-argument {}", self.package(*package))
            }
            RelocationTarget::DescriptionArgument { description } => {
                format!("description-argument {}", self.resource(*description))
            }
            RelocationTarget::Dataset { package, dataset } => {
                format!("dataset {} {dataset}", self.package(*package))
            }
            RelocationTarget::DataArgument { package } => {
                format!("data-argument {}", self.package(*package))
            }
            RelocationTarget::NativeSymbol {
                package,
                component,
                symbol,
            } => format!("native-symbol {} {component} {symbol}", self.package(*package)),
            RelocationTarget::NativeLibrary { package, component } => {
                format!("native-library {} {component}", self.package(*package))
            }
            other => format!("{other:?}"),
        }
    }

    pub fn render(&self) -> String {
        let program = self.program;
        let mut lines = BTreeSet::<String>::new();
        let mut ordered = Vec::<String>::new();

        lines.insert(format!("target {:?}", program.target()));
        lines.insert(format!("root {}", self.package(program.root_package())));
        for (id, package) in program.packages() {
            lines.insert(format!("package {} {package:?}", self.package(id)));
        }
        for namespace in program.namespaces() {
            let key = self.package(namespace.package);
            match &namespace.state {
                LinkNamespaceState::Root(state) | LinkNamespaceState::Linked(state) => lines
                    .insert(format!(
                        "namespace {key} materialized {} {} payload={:?}",
                        self.environment(state.namespace_environment),
                        self.environment(state.imports_environment),
                        state
                            .payload
                            .map(|bundle| self.namespace(program.payload_bundle(bundle).namespace())),
                    )),
                LinkNamespaceState::External { package } => {
                    lines.insert(format!("namespace {key} external {}", self.package(*package)))
                }
            };
            for (name, binding) in &namespace.bindings {
                let state = match &program.binding(*binding).state {
                    LinkBindingState::Materialized { initial, .. } => match initial {
                        InitialBindingState::Unbound => "unbound".to_owned(),
                        InitialBindingState::Value(value) => self.value(*value),
                    },
                    LinkBindingState::External { access, .. } => format!("external {access:?}"),
                };
                lines.insert(format!("binding {key}::{name} = {state}"));
            }
            for (name, slot) in &namespace.imports {
                let slot = match slot {
                    ImportSlotIr::Bound(binding) => self.binding(*binding),
                    ImportSlotIr::Removed(removed) => format!("removed {removed:?}"),
                };
                lines.insert(format!("import {key}::{name} <- {slot}"));
            }
            for record in &namespace.import_records {
                lines.insert(format!("import-record {key} {record:?}"));
            }
            for registration in &namespace.s3_registrations {
                lines.insert(format!("namespace-s3 {key} {}", self.s3(*registration)));
            }
        }
        for registration in program.s3_registrations().iter().enumerate() {
            let _ = registration;
        }
        for (_, resource) in program.indexed_resources() {
            lines.insert(format!(
                "resource {}:{}",
                self.package(resource.package),
                resource.path
            ));
        }
        for (package, library) in program.dataset_libraries() {
            lines.insert(format!(
                "datasets {} {:?} {:?}",
                self.package(package),
                library.objects(),
                library.sets()
            ));
        }
        for (_, bundle) in program.indexed_payload_bundles() {
            let namespace = self.namespace(bundle.namespace());
            let bindings = bundle
                .bindings()
                .iter()
                .map(|binding| self.binding(*binding))
                .collect::<BTreeSet<_>>();
            let patches = bundle
                .closure_patches()
                .iter()
                .map(|patch| {
                    format!(
                        "{} {} {}",
                        match &patch.home {
                            ClosureHome::Namespace => "namespace".to_owned(),
                            ClosureHome::Reached { root, steps } => format!("{root} {steps:?}"),
                        },
                        patch.binding,
                        self.code(patch.code)
                    )
                })
                .collect::<BTreeSet<_>>();
            let dependencies = bundle
                .dependencies()
                .iter()
                .map(|dependency| format!("{:?}", self.namespace(dependency.namespace())))
                .collect::<BTreeSet<_>>();
            lines.insert(format!(
                "payload {namespace} {bindings:?} {patches:?} {dependencies:?}"
            ));
        }
        for relocation in program.relocations() {
            let code = program.code(relocation.site.code);
            lines.insert(format!(
                "relocation {} {:?} -> {}",
                self.code(relocation.site.code),
                code.occurrence(relocation.site.occurrence),
                self.relocation_target(&relocation.target)
            ));
        }
        for activation in program.activations() {
            ordered.push(format!(
                "activation {} on_load={:?} natives={:?} exports={:?} removed={:?}",
                self.namespace(activation.namespace),
                activation.on_load.map(|binding| self.binding(binding)),
                activation.native_components,
                activation.exports.names(),
                activation.removed_bindings,
            ));
        }
        let root = program.root_artifact();
        lines.insert(format!(
            "root-artifact description={:?} exports={:?} natives={:?} on_load={:?}",
            root.description,
            root.exports.names(),
            root.native_components,
            root.on_load.map(|closure| self.closure(closure)),
        ));
        for (stage, load) in [
            ("before", root.load.before_bootstrap()),
            ("after", root.load.after_activation()),
        ] {
            for import in load.imports() {
                lines.insert(format!(
                    "root-load {stage} import {} <- {}",
                    import.local,
                    self.binding(import.target)
                ));
            }
            for registration in load.s3_registrations() {
                lines.insert(format!("root-load {stage} s3 {}", self.s3(*registration)));
            }
        }
        lines.insert(format!(
            "root-load removed {:?}",
            root.load.removed_imports()
        ));

        let mut output = String::new();
        for line in lines {
            let _ = writeln!(output, "{line}");
        }
        for line in ordered {
            let _ = writeln!(output, "{line}");
        }
        output
    }
}
