use super::MaterializeError;
use super::relocated::RelocatedCode;
use crate::ir::{
    BindingId, BindingName, ClosureId, ExternalBindingAccess, GenericHome, ImportSlotIr,
    InitialBindingState, LinkBindingState, LinkNamespaceState, Namespace, NamespaceActivationIr,
    PackageId, PayloadBundleIr, PayloadDependency, ProgramIr, RegisteredNamespace, RemovedImportIr,
    S3RegistrationId, Value, ValueId,
};

const GENERATED_RUNTIME: &str = include_str!("runtime.R");

macro_rules! emit {
    ($out:expr, $($argument:tt)*) => {{
        $out.push_str(&format!($($argument)*));
        $out.push('\n');
    }};
}

pub(super) fn generate_r_source(
    program: &ProgramIr,
    code: &RelocatedCode,
) -> Result<String, MaterializeError> {
    let mut out = String::new();
    out.push_str(
        ".slinker_runtime <- base::new.env(parent = base::baseenv())\nbase::local(envir = .slinker_runtime, {\n",
    );
    out.push_str(GENERATED_RUNTIME);
    emit!(
        out,
        ".slinker_target <- c(version = {}, platform = {}, arch = {})",
        r_string(&program.target().r_version),
        r_string(&program.target().platform),
        r_string(&program.target().arch)
    );
    emit!(
        out,
        ".slinker_root_package <- {}",
        r_string(&program.package(program.root_package()).identity().name)
    );
    emit_bootstrap(&mut out, program, code);
    out.push_str("})\n");
    out.push_str(&root_closures_source(program, code)?);
    out.push_str(
        ".onLoad <- function(libname, pkgname) {\n  .slinker_runtime[[\"bootstrap\"]](base::asNamespace(pkgname), libname, pkgname)\n}\n",
    );
    Ok(out)
}

fn root_closures_source(
    program: &ProgramIr,
    code: &RelocatedCode,
) -> Result<String, MaterializeError> {
    let root_on_load = program.root_artifact().on_load;
    let mut source = String::new();
    for closure in namespace_closures(program, program.root_namespace()) {
        let code_id = program.closure(closure).code;
        let text = code.source(code_id);
        if Some(closure) == root_on_load {
            let value_start = program
                .code(code_id)
                .assigned_value_start()
                .ok_or_else(|| {
                    MaterializeError::InvalidR("Root .onLoad code is not an assignment".into())
                })?;
            source.push_str(".slinker_original_on_load <- ");
            source.push_str(&text[value_start..]);
        } else {
            source.push_str(text);
        }
        source.push('\n');
    }
    Ok(source)
}

fn emit_bootstrap(out: &mut String, program: &ProgramIr, code: &RelocatedCode) {
    let root = program.root_namespace();
    out.push_str("bootstrap <- function(root, libname, pkgname) {\n  .slinker_check_target()\n");
    out.push_str("  on.exit(.slinker_unregister(), add = TRUE)\n");
    for (local, removed) in program.root_artifact().load.removed_imports() {
        emit!(
            out,
            "  .slinker_stub(parent.env(root), {})",
            removed_import(local, removed)
        );
    }
    for activation in program.activations() {
        let package = program.package(program.namespace(activation.namespace).package);
        let identity = package.identity();
        emit!(
            out,
            "  namespaces[[{key}]] <- .slinker_new_namespace({key}, {}, {})",
            r_string(&identity.name),
            r_string(identity.version.as_ref()),
            key = r_string(package.registered_namespace().as_str()),
        );
    }
    for activation in program.activations() {
        emit_activation(out, program, code, activation);
    }
    let after_activation = program.root_artifact().load.after_activation();
    for import in after_activation.imports() {
        emit!(
            out,
            "  assign({}, {}, envir = parent.env(root))",
            r_string(&import.local),
            binding_reference(program, import.target)
        );
    }
    if let Some(bundle) = payload_bundle(program, root) {
        emit!(
            out,
            "  .slinker_populate(root, {}, {})",
            r_string(&program.package(root.package).identity().name),
            external_payload_dependencies(program, bundle)
        );
    }
    let activated_s3 = after_activation.s3_registrations();
    if !activated_s3.is_empty() {
        emit!(
            out,
            "  .slinker_register_s3(root, {}, {})",
            s3_matrix(program, activated_s3, S3Column4::Registry),
            s3_matrix(program, activated_s3, S3Column4::Original)
        );
    }
    out.push_str("  .slinker_unregister()\n");
    if program.root_artifact().on_load.is_some() {
        out.push_str(
            "  get(\".slinker_original_on_load\", envir = root, inherits = FALSE)(libname, pkgname)\n",
        );
    }
    out.push_str("}\n");
}

fn emit_activation(
    out: &mut String,
    program: &ProgramIr,
    code: &RelocatedCode,
    activation: &NamespaceActivationIr,
) {
    let namespace = program.namespace(activation.namespace);
    let package = program.package(namespace.package);
    let name = &package.identity().name;
    emit!(
        out,
        "  local({{\n    ns <- namespaces[[{}]]\n    imports <- parent.env(ns)",
        r_string(package.registered_namespace().as_str())
    );
    for native in &activation.native_components {
        let Some(library) = native.library.path() else {
            continue;
        };
        let symbols = native
            .bindings()
            .map(|symbol| {
                format!(
                    "{} = {}",
                    r_string(&symbol.binding),
                    r_string(&symbol.symbol)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        emit!(
            out,
            "    .slinker_load_native(ns, {}, {}, {}, {}, c({symbols}))",
            r_string(name),
            r_string(&native.name),
            r_string(&native.alias),
            r_string(library)
        );
    }
    if program
        .dataset_libraries()
        .any(|(library, _)| library == namespace.package)
    {
        emit!(out, "    .slinker_lazydata(ns, {})", r_string(name));
    }
    emit!(
        out,
        "    setNamespaceInfo(ns, \"imports\", {})",
        imports_info(namespace)
    );
    for (local, slot) in &namespace.imports {
        match slot {
            ImportSlotIr::Bound(target) => emit!(
                out,
                "    assign({}, {}, envir = imports)",
                r_string(local),
                binding_reference(program, *target)
            ),
            ImportSlotIr::Removed(removed) => emit!(
                out,
                "    .slinker_stub(imports, {})",
                removed_import(local, removed)
            ),
        }
    }
    for closure in namespace_closures(program, namespace) {
        let source = code.source(program.closure(closure).code);
        emit!(
            out,
            "    eval(parse(text = {}), envir = ns)",
            r_string(source)
        );
    }
    if let Some(bundle) = payload_bundle(program, namespace) {
        emit!(
            out,
            "    .slinker_populate(ns, {}, {})",
            r_string(name),
            external_payload_dependencies(program, bundle)
        );
    }
    emit!(
        out,
        "    .slinker_activate(ns, {}, {}, {}, {}, {})\n  }})",
        r_vector(activation.exports.names().iter().map(BindingName::as_str)),
        s3_matrix(program, &namespace.s3_registrations, S3Column4::Registry),
        s3_matrix(program, &namespace.s3_registrations, S3Column4::Original),
        r_vector(activation.removed_bindings.iter().map(BindingName::as_str)),
        if activation.on_load.is_some() {
            "TRUE"
        } else {
            "FALSE"
        }
    );
}

fn namespace_closures(
    program: &ProgramIr,
    namespace: &Namespace,
) -> impl Iterator<Item = ClosureId> {
    namespace.bindings.values().filter_map(|binding| {
        match initial_value(program, *binding).map(|value| program.value(value)) {
            Some(Value::Closure(closure)) => Some(*closure),
            _ => None,
        }
    })
}

fn payload_bundle<'a>(
    program: &'a ProgramIr,
    namespace: &Namespace,
) -> Option<&'a PayloadBundleIr> {
    match &namespace.state {
        LinkNamespaceState::Root(state) | LinkNamespaceState::Linked(state) => {
            state.payload.map(|bundle| program.payload_bundle(bundle))
        }
        LinkNamespaceState::External { .. } => None,
    }
}

fn external_payload_dependencies(program: &ProgramIr, bundle: &PayloadBundleIr) -> String {
    r_vector(bundle.dependencies().iter().filter_map(|dependency| {
        match dependency {
            PayloadDependency::External(namespace) => Some(
                program
                    .package(program.namespace(*namespace).package)
                    .identity()
                    .name
                    .as_str(),
            ),
            PayloadDependency::Linked(_) => None,
        }
    }))
}

fn initial_value(program: &ProgramIr, binding: BindingId) -> Option<ValueId> {
    match &program.binding(binding).state {
        LinkBindingState::Materialized {
            initial: InitialBindingState::Value(value),
            ..
        } => Some(*value),
        LinkBindingState::Materialized { .. } | LinkBindingState::External { .. } => None,
    }
}

pub(super) fn binding_reference(program: &ProgramIr, binding: BindingId) -> String {
    let namespace = program.namespace(program.binding_namespace(binding));
    let package = program.package(namespace.package);
    let exported_external = matches!(
        program.binding(binding).state,
        LinkBindingState::External {
            access: ExternalBindingAccess::Exported,
            ..
        }
    );
    if exported_external {
        format!(
            "base::getExportedValue({}, {})",
            r_string(&package.identity().name),
            r_string(&program.binding(binding).name)
        )
    } else {
        namespace_get(program, binding)
    }
}

pub(super) fn namespace_get(program: &ProgramIr, binding: BindingId) -> String {
    format!(
        "base::get({}, envir = {}, inherits = FALSE)",
        r_string(&program.binding(binding).name),
        namespace_expression(
            program,
            program
                .namespace(program.binding_namespace(binding))
                .package
        )
    )
}

pub(super) fn namespace_expression(program: &ProgramIr, package: PackageId) -> String {
    let root = program.package(program.root_package());
    match program.package(package).registered_namespace() {
        RegisteredNamespace::Package(name) => format!("base::asNamespace({})", r_string(name)),
        RegisteredNamespace::Private(key) => format!(
            "base::asNamespace({})[[\".slinker_runtime\"]][[\"namespaces\"]][[{}]]",
            r_string(&root.identity().name),
            r_string(key.as_str())
        ),
    }
}

#[derive(Clone, Copy)]
enum S3Column4 {
    Registry,
    Original,
}

fn s3_matrix(
    program: &ProgramIr,
    registrations: &[S3RegistrationId],
    column4: S3Column4,
) -> String {
    let rows = registrations
        .iter()
        .map(|registration| program.s3_registration(*registration))
        .collect::<Vec<_>>();
    let cells = rows
        .iter()
        .map(|row| r_string(&row.generic.name))
        .chain(rows.iter().map(|row| r_string(&row.class)))
        .chain(
            rows.iter()
                .map(|row| r_string(&program.binding(row.method).name)),
        )
        .chain(rows.iter().map(|row| match &row.generic.home {
            GenericHome::Lexical => "NA_character_".to_owned(),
            GenericHome::Program(package) => match column4 {
                S3Column4::Registry => {
                    r_string(program.package(*package).registered_namespace().as_str())
                }
                S3Column4::Original => r_string(&program.package(*package).identity().name),
            },
            GenericHome::Optional(package) => r_string(package),
        }))
        .collect::<Vec<_>>();
    format!("matrix(as.character(c({})), ncol = 4L)", cells.join(", "))
}

fn imports_info(namespace: &Namespace) -> String {
    let records = namespace.import_records.iter().map(|record| {
        let locals = record
            .names
            .iter()
            .map(|(local, _)| local)
            .map(|local| r_string(local))
            .collect::<Vec<_>>()
            .join(", ");
        let remotes = record
            .names
            .iter()
            .map(|(_, remote)| remote)
            .map(|remote| r_string(remote))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} = structure(as.character(c({remotes})), names = as.character(c({locals})))",
            r_string(&record.package)
        )
    });
    let entries = std::iter::once("base = TRUE".to_owned())
        .chain(records)
        .collect::<Vec<_>>();
    format!("list({})", entries.join(", "))
}

fn r_vector<'a>(values: impl Iterator<Item = &'a str>) -> String {
    format!(
        "as.character(c({}))",
        values.map(r_string).collect::<Vec<_>>().join(", ")
    )
}

pub(super) fn render_namespace(program: &ProgramIr) -> String {
    let before_bootstrap = program.root_artifact().load.before_bootstrap();
    let mut out = String::new();
    for name in program.root_artifact().exports.names() {
        emit!(out, "export({})", r_string(name));
    }
    for import in before_bootstrap.imports() {
        let package = program
            .namespace(program.binding_namespace(import.target))
            .package;
        emit!(
            out,
            "importFrom({}, {})",
            r_string(&program.package(package).identity().name),
            r_string(&program.binding(import.target).name)
        );
    }
    for native in &program.root_artifact().native_components {
        let registration = native.registration.iter().map(|fixes| {
            format!(
                ".registration = TRUE, .fixes = c({}, {})",
                r_string(&fixes.prefix),
                r_string(&fixes.suffix)
            )
        });
        let symbols = native.symbols.iter().map(|symbol| {
            format!(
                "{} = {}",
                r_binding_name(&symbol.binding),
                r_string(&symbol.symbol)
            )
        });
        emit!(
            out,
            "useDynLib({})",
            std::iter::once(r_binding_name(&native.name))
                .chain(registration)
                .chain(symbols)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for registration in before_bootstrap.s3_registrations() {
        let registration = program.s3_registration(*registration);
        let generic = match &registration.generic.home {
            GenericHome::Program(package) => format!(
                "{}::{}",
                r_binding_name(&program.package(*package).identity().name),
                r_binding_name(&registration.generic.name)
            ),
            GenericHome::Optional(package) => format!(
                "{}::{}",
                r_binding_name(package),
                r_binding_name(&registration.generic.name)
            ),
            GenericHome::Lexical => r_string(&registration.generic.name),
        };
        emit!(
            out,
            "S3method({generic}, {}, {})",
            r_string(&registration.class),
            r_string(&program.binding(registration.method).name)
        );
    }
    out
}

pub(super) fn native_library(program: &ProgramIr, package: PackageId, component: &str) -> String {
    format!(
        "base::getNamespaceInfo({}, \"DLLs\")[[{}]]",
        namespace_expression(program, package),
        r_string(component)
    )
}

fn removed_import(local: &str, removed: &RemovedImportIr) -> String {
    format!(
        "{}, {}, {}",
        r_string(local),
        r_string(&removed.package),
        r_string(&removed.binding)
    )
}

pub(super) fn r_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

fn r_binding_name(value: &str) -> String {
    let simple = !value.is_empty()
        && value.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_alphabetic() || byte == b'.'
            } else {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_')
            }
        })
        && !(value.starts_with('.') && value.as_bytes().get(1).is_some_and(u8::is_ascii_digit));
    if simple {
        value.into()
    } else {
        format!("`{}`", value.replace('`', "\\`"))
    }
}
